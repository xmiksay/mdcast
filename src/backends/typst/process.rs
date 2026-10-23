//! The one bounded-child-process primitive both `cli`-mode children share:
//! the `typst compile` subprocess (`compiler/cli.rs`) and the SVG-outline
//! child (`svg_outline/child.rs`). Each gets the same wall-clock timeout with
//! an explicit kill+reap on elapse — what `spawn_blocking` structurally cannot
//! offer — and the same optional `RLIMIT_AS` cap, so `MDCAST_TYPST_TIMEOUT_SECS`
//! / `MDCAST_TYPST_MAX_MEMORY_BYTES` mean one thing for every child. No
//! child may dump core (see `apply_child_limits`).

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Timeout + address-space cap applied to one child.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bounds {
    pub timeout: Duration,
    /// Unix-only; `None`, or any value on a non-unix target, leaves the child
    /// unbounded.
    pub max_memory_bytes: Option<u64>,
}

/// typst diagnostics are the only stderr anyone reads, and only on failure.
const MAX_STDERR: usize = 1 << 20;

pub(super) struct Finished {
    pub status: ExitStatus,
    /// At most `stdout_cap` bytes.
    pub stdout: Vec<u8>,
    /// The child wrote more than `stdout_cap` bytes; `stdout` is truncated.
    pub stdout_overflow: bool,
    pub stderr: Vec<u8>,
}

/// Runs `cmd` to completion under `bounds`, feeding it `stdin` (or `/dev/null`
/// when `None`). `what` names the child in errors ("typst compile").
///
/// The pipes are serviced *concurrently* with `wait()`, never after: a child
/// blocked on a full stdout/stderr pipe, or on a stdin nobody finished
/// writing, would otherwise deadlock `wait()` under the very timeout meant to
/// bound it. Output past the caps is read and discarded rather than left in
/// the pipe for the same reason — a child that floods its output keeps
/// running into the timeout instead of into the parent's memory.
pub(super) async fn run_bounded(
    mut cmd: Command,
    stdin: Option<Vec<u8>>,
    bounds: Bounds,
    stdout_cap: usize,
    what: &str,
) -> Result<Finished> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(unix)]
    apply_child_limits(&mut cmd, bounds.max_memory_bytes);

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "spawn {what} (`{}`)",
            cmd.as_std().get_program().to_string_lossy()
        )
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
/// `RLIMIT_CORE` is always zeroed: a child stopped by the memory cap aborts,
/// and its core dump (tens to hundreds of MB, written to its working
/// directory) would turn every hostile render into disk fill — the server's
/// writable layer, or the repo root under `make verify`.
///
/// `RLIMIT_AS` bounds the whole address space (heap, stacks, mmap'd files),
/// which is what actually stops an unbounded child from exhausting host
/// memory; it is not a precise "bytes of document data" cap.
#[cfg(unix)]
fn apply_child_limits(cmd: &mut Command, max_memory_bytes: Option<u64>) {
    // SAFETY: the closure only calls `libc::setrlimit`, an async-signal-safe
    // syscall wrapper that performs no allocation — the constraint `pre_exec`
    // documents (it runs post-fork, pre-exec, in a single-threaded child
    // where most of the standard library is unsafe to use).
    unsafe {
        cmd.pre_exec(move || {
            let no_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(limit) = max_memory_bytes {
                let rl = libc::rlimit {
                    rlim_cur: limit as libc::rlim_t,
                    rlim_max: limit as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &rl) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn stdin_is_fed_while_stdout_is_drained() {
        // Larger than any pipe buffer in both directions: deadlocks unless
        // feeding and draining overlap.
        let input = vec![b'x'; 4 << 20];
        let bounds = Bounds {
            timeout: Duration::from_secs(20),
            max_memory_bytes: None,
        };
        let done = run_bounded(
            Command::new("cat"),
            Some(input.clone()),
            bounds,
            8 << 20,
            "cat",
        )
        .await
        .unwrap();
        assert!(done.status.success());
        assert_eq!(done.stdout, input);
        assert!(!done.stdout_overflow);
    }

    /// Reads the limits the child actually runs under, rather than trusting
    /// that `pre_exec` ran.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn children_never_dump_core_and_get_the_memory_cap() {
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
        for max_memory_bytes in [None, Some(1u64 << 30)] {
            let mut cmd = Command::new("cat");
            cmd.arg("/proc/self/limits");
            let bounds = Bounds {
                timeout: Duration::from_secs(20),
                max_memory_bytes,
            };
            let done = run_bounded(cmd, None, bounds, 1 << 16, "cat")
                .await
                .unwrap();
            assert!(done.status.success());
            let limits = String::from_utf8(done.stdout).unwrap();
            assert_eq!(limit(&limits, "Max core file size"), ["0", "0"]);
            if let Some(cap) = max_memory_bytes {
                let cap = cap.to_string();
                assert_eq!(limit(&limits, "Max address space"), [cap.as_str(), &cap]);
            }
        }
    }
}
