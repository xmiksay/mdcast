//! The one bounded-child-process primitive every engine subprocess shares:
//! the `typst compile` child (`backends/typst/compiler/cli.rs`), the
//! SVG-outline child (`backends/typst/svg_outline/child.rs`) and the pandoc
//! child (`backends/pandoc.rs`). Each gets a wall-clock timeout with an
//! explicit kill+reap on elapse — what `spawn_blocking` structurally cannot
//! offer — and the same `setrlimit` set (address space, file size, CPU time).
//! No child may dump core (see `apply_child_limits`), and no child inherits
//! the server's environment (see `run_bounded`). Lives at the crate root, not
//! under `backends/typst/`, because the `pandoc` feature must not require
//! `typst`.

use std::ffi::{OsStr, OsString};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Timeout + resource caps applied to one child. The `Option` caps are
/// Unix-only; `None`, or any value on a non-unix target, leaves that resource
/// unbounded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bounds {
    pub timeout: Duration,
    /// `RLIMIT_AS`.
    pub max_memory_bytes: Option<u64>,
    /// `RLIMIT_FSIZE` — the largest file the child may write.
    pub max_file_bytes: Option<u64>,
    /// `RLIMIT_CPU`, in seconds.
    pub cpu_secs: Option<u64>,
}

/// Diagnostics are the only stderr anyone reads, and only on failure.
const MAX_STDERR: usize = 1 << 20;

/// The parent variables a child keeps — see [`run_bounded`].
const INHERITED_ENV: [&str; 2] = ["PATH", "TZ"];

/// An engine child process could not be started (binary missing, not
/// executable, …) — or ran but rejected the invocation itself, as a `typst`
/// too old for the flags mdcast passes does. Typed so an embedder can tell
/// "this deployment is broken" from "this document does not render":
/// `mdcast-server` answers it `500 internal`, not `422 render_failed`.
#[derive(Debug)]
pub struct SpawnError {
    pub what: String,
    pub program: String,
    pub source: std::io::Error,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not run {} (`{}`)", self.what, self.program)
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(crate) struct Finished {
    pub status: ExitStatus,
    /// At most `stdout_cap` bytes.
    #[cfg_attr(not(feature = "typst"), allow(dead_code))] // pandoc writes to `-o`
    pub stdout: Vec<u8>,
    /// The child wrote more than `stdout_cap` bytes; `stdout` is truncated.
    #[cfg_attr(not(feature = "typst"), allow(dead_code))]
    pub stdout_overflow: bool,
    pub stderr: Vec<u8>,
}

/// Runs `cmd` to completion under `bounds`, feeding it `stdin` (or `/dev/null`
/// when `None`). `what` names the child in errors ("typst compile").
///
/// The child's environment is exactly `env` plus the parent's `PATH` and `TZ`
/// (each only when set) — everything else (`MDCAST_TOKEN` above all, but also
/// `TYPST_*` overrides and proxy settings) is cleared, whatever `cmd` had
/// set. `PATH` survives only so a bare program name like `typst` still
/// resolves; `TZ` so `datetime.today()` and pandoc's dates agree with the
/// in-process engine, which reads the server's own zone.
///
/// A child that cannot be started at all fails with a [`SpawnError`] in the
/// chain: that is the deployment's fault, never the document's.
///
/// The pipes are serviced *concurrently* with `wait()`, never after: a child
/// blocked on a full stdout/stderr pipe, or on a stdin nobody finished
/// writing, would otherwise deadlock `wait()` under the very timeout meant to
/// bound it. Output past the caps is read and discarded rather than left in
/// the pipe for the same reason — a child that floods its output keeps
/// running into the timeout instead of into the parent's memory.
pub(crate) async fn run_bounded(
    mut cmd: Command,
    env: &[(&str, &OsStr)],
    stdin: Option<Vec<u8>>,
    bounds: Bounds,
    stdout_cap: usize,
    what: &str,
) -> Result<Finished> {
    let inherited: Vec<(&str, OsString)> = INHERITED_ENV
        .iter()
        .filter_map(|&key| std::env::var_os(key).map(|v| (key, v)))
        .collect();
    cmd.env_clear()
        .envs(inherited.iter().map(|(k, v)| (*k, v.as_os_str())))
        .envs(env.iter().copied());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(unix)]
    apply_child_limits(&mut cmd, bounds);

    let mut child = cmd.spawn().map_err(|source| SpawnError {
        what: what.to_string(),
        program: cmd.as_std().get_program().to_string_lossy().into_owned(),
        source,
    })?;
    let child_stdin = child.stdin.take();
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        bail!("{what}: child pipes were not captured");
    };

    let feed = async move {
        if let (Some(mut pipe), Some(bytes)) = (child_stdin, stdin) {
            // A write error means the child stopped reading (exited or died);
            // its exit status and response say why, so it is not reported here.
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        }
    };
    let drain = async {
        let (out, err) = tokio::join!(
            read_capped(&mut stdout, stdout_cap),
            read_capped(&mut stderr, MAX_STDERR),
        );
        (out, err)
    };
    let run = async { tokio::join!(child.wait(), drain, feed) };

    let (status, (out, err), ()) = match tokio::time::timeout(bounds.timeout, run).await {
        Ok(done) => done,
        Err(_elapsed) => {
            let _ = child.kill().await; // sends the kill and reaps it
            bail!("{what} timed out after {:?} and was killed", bounds.timeout);
        }
    };
    let status = status.with_context(|| format!("wait for {what}"))?;
    let (stdout, stdout_overflow) = out.with_context(|| format!("read {what} stdout"))?;
    let (stderr, _) = err.with_context(|| format!("read {what} stderr"))?;
    Ok(Finished {
        status,
        stdout,
        stdout_overflow,
        stderr,
    })
}

/// Reads `r` to EOF, keeping the first `cap` bytes. `true` = more was
/// discarded.
async fn read_capped(
    r: &mut (impl AsyncRead + Unpin),
    cap: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok((kept, overflow));
        }
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..n.min(room)]);
        overflow |= n > room;
    }
}

/// Per-child resource limits, installed via `pre_exec` (runs after `fork`,
/// before `exec`) — the only place a per-process limit can be set for an
/// arbitrary binary.
///
/// `RLIMIT_CORE` is always zeroed: a child stopped by a cap aborts, and its
/// core dump (tens to hundreds of MB, written to its working directory) would
/// turn every hostile render into disk fill — the server's writable layer, or
/// the repo root under `make verify`.
///
/// `RLIMIT_AS` bounds the whole address space (heap, stacks, mmap'd files),
/// which is what actually stops an unbounded child from exhausting host
/// memory; it is not a precise "bytes of document data" cap. `RLIMIT_FSIZE`
/// stops a child filling the disk before the parent's own output-size check
/// ever runs. `RLIMIT_CPU` is defense in depth behind the wall-clock timeout:
/// it still holds should the parent die and leave the child unreaped.
///
/// Each limit is clamped to the inherited hard limit, since raising one is
/// `EPERM` for an unprivileged process and would fail the spawn outright.
#[cfg(unix)]
fn apply_child_limits(cmd: &mut Command, bounds: Bounds) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    type Resource = libc::__rlimit_resource_t;
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    type Resource = libc::c_int;

    fn lower(resource: Resource, limit: u64) -> std::io::Result<()> {
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: plain syscalls on a stack value; see the `pre_exec` note.
        unsafe {
            if libc::getrlimit(resource, &mut current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let value = (limit as libc::rlim_t).min(current.rlim_max);
            let rl = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            if libc::setrlimit(resource, &rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    // SAFETY: the closure only calls `getrlimit`/`setrlimit`, async-signal-
    // safe syscall wrappers that perform no allocation — the constraint
    // `pre_exec` documents (it runs post-fork, pre-exec, in a single-threaded
    // child where most of the standard library is unsafe to use).
    unsafe {
        cmd.pre_exec(move || {
            lower(libc::RLIMIT_CORE, 0)?;
            if let Some(limit) = bounds.max_memory_bytes {
                lower(libc::RLIMIT_AS, limit)?;
            }
            if let Some(limit) = bounds.max_file_bytes {
                lower(libc::RLIMIT_FSIZE, limit)?;
            }
            if let Some(limit) = bounds.cpu_secs {
                lower(libc::RLIMIT_CPU, limit)?;
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_binary_is_a_typed_spawn_error() {
        let err = run_bounded(
            Command::new("/nonexistent/mdcast-no-such-binary"),
            &[],
            None,
            bounds(),
            0,
            "nothing",
        )
        .await
        .err()
        .unwrap();
        let spawn = err.downcast_ref::<SpawnError>().unwrap();
        assert_eq!(spawn.source.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn read_capped_keeps_the_prefix_and_flags_the_rest() {
        let data = vec![7u8; 200_000];
        let (kept, overflow) = read_capped(&mut data.as_slice(), 10).await.unwrap();
        assert_eq!(kept, vec![7u8; 10]);
        assert!(overflow);

        let (kept, overflow) = read_capped(&mut &b"abc"[..], 3).await.unwrap();
        assert_eq!(kept, b"abc");
        assert!(!overflow);
    }

    fn bounds() -> Bounds {
        Bounds {
            timeout: Duration::from_secs(20),
            max_memory_bytes: None,
            max_file_bytes: None,
            cpu_secs: None,
        }
    }

    #[tokio::test]
    async fn stdin_is_fed_while_stdout_is_drained() {
        // Larger than any pipe buffer in both directions: deadlocks unless
        // feeding and draining overlap.
        let input = vec![b'x'; 4 << 20];
        let done = run_bounded(
            Command::new("cat"),
            &[],
            Some(input.clone()),
            bounds(),
            8 << 20,
            "cat",
        )
        .await
        .unwrap();
        assert!(done.status.success());
        assert_eq!(done.stdout, input);
        assert!(!done.stdout_overflow);
    }

    /// The child sees exactly `PATH` (and `TZ`, when the parent has one) plus
    /// what the caller passed: nothing else the server inherited, and not
    /// even a variable set on `cmd` itself — so a bearer token can never ride
    /// along whichever way it got there.
    #[tokio::test]
    async fn children_get_only_path_and_the_given_env() {
        let mut cmd = Command::new("env");
        cmd.env("MDCAST_TOKEN", "leaked-token");
        let home = OsStr::new("/sandbox/home");
        let done = run_bounded(cmd, &[("HOME", home)], None, bounds(), 1 << 16, "env")
            .await
            .unwrap();
        assert!(done.status.success());
        let out = String::from_utf8(done.stdout).unwrap();
        let mut keys: Vec<&str> = out.lines().filter_map(|l| l.split('=').next()).collect();
        keys.sort_unstable();
        let mut expected = vec!["HOME", "PATH"];
        if let Some(tz) = std::env::var_os("TZ") {
            expected.push("TZ");
            assert!(
                out.contains(&format!("TZ={}", tz.to_string_lossy())),
                "{out}"
            );
        }
        assert_eq!(keys, expected, "{out}");
        assert!(out.contains("HOME=/sandbox/home"), "{out}");
        assert!(!out.contains("leaked-token"), "{out}");
    }

    /// Reads the limits the child actually runs under, rather than trusting
    /// that `pre_exec` ran.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn children_never_dump_core_and_get_every_cap() {
        let limit = |limits: &str, name: &str| -> Vec<String> {
            let line = limits
                .lines()
                .find(|l| l.starts_with(name))
                .unwrap_or_else(|| panic!("no {name:?} in {limits}"));
            line[name.len()..]
                .split_whitespace()
                .take(2)
                .map(str::to_string)
                .collect()
        };
        let capped = Bounds {
            max_memory_bytes: Some(1 << 30),
            max_file_bytes: Some(1 << 20),
            cpu_secs: Some(42),
            ..bounds()
        };
        for bounds in [bounds(), capped] {
            let mut cmd = Command::new("cat");
            cmd.arg("/proc/self/limits");
            let done = run_bounded(cmd, &[], None, bounds, 1 << 16, "cat")
                .await
                .unwrap();
            assert!(done.status.success());
            let limits = String::from_utf8(done.stdout).unwrap();
            assert_eq!(limit(&limits, "Max core file size"), ["0", "0"]);
            for (name, cap) in [
                ("Max address space", bounds.max_memory_bytes),
                ("Max file size", bounds.max_file_bytes),
                ("Max cpu time", bounds.cpu_secs),
            ] {
                if let Some(cap) = cap {
                    let cap = cap.to_string();
                    assert_eq!(limit(&limits, name), [cap.as_str(), &cap], "{name}");
                }
            }
        }
    }
}
