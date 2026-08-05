//! `TypstCompiler::Cli`: materialise [`CompileInputs`] into a temp directory
//! and shell out to a `typst compile` subprocess. Unlike the in-process
//! engine, this can be killed on timeout and (optionally) memory-capped —
//! the point of the exercise (issue #73): a caller-supplied template that
//! never terminates costs one process for the duration of its own timeout,
//! not one OS thread for the life of the server.
//!
//! Security note — the sandbox properties are not the same as `Library`'s.
//! In-process, the Typst `World` is entirely virtual and cannot reach the
//! host filesystem at all. A real `typst` binary against a real temp dir
//! *can*, subject to its own `--root` confinement, which is why `--root` is
//! always pointed at the temp dir explicitly rather than left to typst's
//! cwd-derived default. The temp dir itself is the one filesystem escape in
//! an otherwise in-memory pipeline: it is created with owner-only
//! permissions and is always removed — on success, on a failed compile, and
//! on a timeout-kill — because `dir` (a [`tempfile::TempDir`]) is held for
//! the whole function and its `Drop` impl deletes the directory regardless of
//! which return path is taken.
//!
//! `--root` only confines what the *compiler* can read once it starts.
//! Writing [`CompileInputs`] to disk beforehand is a second, independent
//! attack surface: an `AssetProvider` key that reaches this code unsanitised
//! (the template path's sibling discovery deliberately preserves `/`-nested
//! keys so relative `#import`s resolve — see `template.rs`'s module docs)
//! must not be allowed to contain a `..` component, or a malicious manifest
//! key becomes an arbitrary-file-write outside the temp dir. `safe_join`
//! is the one place that risk is closed off, applied uniformly to every path
//! this module writes.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{CompileFormat, CompileInputs};

const FONTS_DIR: &str = ".mdcast-fonts";

/// Config for the `typst` CLI compiler. `bin` is looked up on `PATH` unless
/// it's an absolute/relative path itself (`std::process::Command`'s usual
/// resolution).
#[derive(Clone, Debug, PartialEq)]
pub struct CliCompiler {
    pub bin: String,
    /// Wall-clock budget for one compile. Exceeded → the child is killed and
    /// reaped before the error is returned, so the caller never sees a
    /// lingering process.
    pub timeout: Duration,
    /// Address-space cap applied to the child via `setrlimit(RLIMIT_AS)`.
    /// Unix-only (see `Cargo.toml`'s `cfg(unix)` `libc` dependency) — `None`,
    /// or any value on a non-unix target, leaves the child unbounded.
    pub max_memory_bytes: Option<u64>,
}

impl CliCompiler {
    pub fn new(bin: impl Into<String>, timeout: Duration) -> Self {
        Self {
            bin: bin.into(),
            timeout,
            max_memory_bytes: None,
        }
    }

    pub fn with_max_memory_bytes(mut self, bytes: Option<u64>) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Runs `<bin> --version` and logs the result — see `mod.rs`'s
    /// `TypstCompiler::log_startup_info`.
    pub(super) async fn log_version(&self) {
        match Command::new(&self.bin).arg("--version").output().await {
            Ok(out) if out.status.success() => {
                let version = String::from_utf8_lossy(&out.stdout);
                tracing::info!(
                    bin = %self.bin,
                    version = version.trim(),
                    "typst CLI compiler selected"
                );
            }
            Ok(out) => tracing::warn!(
                bin = %self.bin,
                status = %out.status,
                stderr = %String::from_utf8_lossy(&out.stderr),
                "`typst --version` did not succeed; MDCAST_TYPST=cli may not work"
            ),
            Err(error) => tracing::warn!(
                bin = %self.bin,
                %error,
                "could not run the typst CLI binary; MDCAST_TYPST=cli may not work"
            ),
        }
    }

    pub(super) async fn compile(&self, inputs: CompileInputs) -> Result<Vec<u8>> {
        let dir = tempfile::Builder::new()
            .prefix("mdcast-typst-")
            .tempdir()
            .context("create typst compile temp dir")?;
        restrict_to_owner(dir.path())?;

        write_inputs(dir.path(), &inputs).await?;

        let output_name = match inputs.format {
            CompileFormat::Pdf => "output.pdf",
            CompileFormat::Html => "output.html",
        };
        let main = safe_join(dir.path(), &inputs.main)?;
        let main_rel = main
            .strip_prefix(dir.path())
            .expect("safe_join always returns a path under root");

        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(dir.path())
            .arg("compile")
            .arg("--root")
            .arg(dir.path())
            .arg("--font-path")
            .arg(dir.path().join(FONTS_DIR));
        if inputs.format == CompileFormat::Html {
            cmd.arg("--features")
                .arg("html")
                .arg("--format")
                .arg("html");
        }
        cmd.arg(main_rel).arg(output_name);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        apply_memory_limit(&mut cmd, self.max_memory_bytes);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn typst compiler `{}`", self.bin))?;
        let mut stdout = child.stdout.take().expect("stdout was piped");
        let mut stderr = child.stderr.take().expect("stderr was piped");

        // stdout/stderr must be drained *concurrently* with `wait()`, not
        // after — the child can block on a full pipe (typst diagnostics can
        // exceed the OS pipe buffer) while nobody is reading it, which would
        // otherwise deadlock `wait()` under the very timeout meant to bound
        // it.
        let drain = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let _ = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err),);
            err
        };
        let run = futures::future::join(child.wait(), drain);

        let (status, stderr) = match tokio::time::timeout(self.timeout, run).await {
            Ok((status, stderr)) => (status.context("wait for typst compile")?, stderr),
            Err(_elapsed) => {
                let _ = child.kill().await; // sends the kill and reaps it
                bail!(
                    "typst compile timed out after {:?} and was killed",
                    self.timeout
                );
            }
        };

        if !status.success() {
            bail!(
                "typst compile failed ({status}):\n{}",
                String::from_utf8_lossy(&stderr)
            );
        }

        tokio::fs::read(dir.path().join(output_name))
            .await
            .context("read typst compile output")
    }
}

/// Joins `rel` onto `root`, rejecting any component that could escape it
/// (`..`, an absolute prefix, a bare root). See this module's doc comment —
/// this is the one place that stands between an unsanitised `AssetProvider`
/// key and a real filesystem write.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => bail!("refusing to materialise unsafe path outside the compile root: {rel:?}"),
        }
    }
    Ok(path)
}

async fn write_inputs(root: &Path, inputs: &CompileInputs) -> Result<()> {
    for (path, content) in &inputs.sources {
        write_file(root, path, content.as_bytes()).await?;
    }
    for (path, bytes) in &inputs.files {
        write_file(root, path, bytes).await?;
    }
    // Always created, even when empty, so `--font-path` never points at a
    // missing directory.
    let fonts_dir = root.join(FONTS_DIR);
    tokio::fs::create_dir_all(&fonts_dir)
        .await
        .context("create typst font-path dir")?;
    for (i, bytes) in inputs.fonts.iter().enumerate() {
        let path = fonts_dir.join(format!("font-{i}.{}", sniff_font_extension(bytes)));
        tokio::fs::write(&path, bytes)
            .await
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

async fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let path = safe_join(root, rel)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create {}", parent.display()))?;
    }
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("write {}", path.display()))
}

/// Sniffs enough of a font's magic bytes to pick an extension `fontdb`'s
/// directory scan (which `typst compile --font-path` delegates to)
/// recognises by suffix, not content — see `typst-kit`'s `FontSearcher`.
/// mdcast's own font bytes carry no filename (`ResolvedDoc.fonts` is raw
/// bytes fetched by digest/key), so this reconstructs one just well enough
/// for the CLI to find them.
fn sniff_font_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"OTTO") {
        "otf"
    } else if bytes.starts_with(b"ttcf") {
        "ttc"
    } else {
        "ttf"
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<()> {
    Ok(())
}

/// `setrlimit(RLIMIT_AS)` in the child, applied via `pre_exec` (runs after
/// `fork`, before `exec`) — the only place a per-process memory cap can be
/// installed for an arbitrary binary. `RLIMIT_AS` bounds the whole address
/// space (heap, stacks, mmap'd files), which is what actually stops an
/// unbounded Typst compile from exhausting host memory; it is not a precise
/// "bytes of document data" cap.
#[cfg(unix)]
fn apply_memory_limit(cmd: &mut Command, max_memory_bytes: Option<u64>) {
    let Some(limit) = max_memory_bytes else {
        return;
    };
    // SAFETY: the closure only calls `libc::setrlimit`, an async-signal-safe
    // syscall wrapper that performs no allocation — the constraint `pre_exec`
    // documents (it runs post-fork, pre-exec, in a single-threaded child
    // where most of the standard library is unsafe to use).
    unsafe {
        cmd.pre_exec(move || {
            let rl = libc::rlimit {
                rlim_cur: limit as libc::rlim_t,
                rlim_max: limit as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
