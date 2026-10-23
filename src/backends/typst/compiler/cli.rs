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
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::super::process::{Bounds, run_bounded};
use super::{CompileFormat, CompileInputs};

const FONTS_DIR: &str = ".mdcast-fonts";

/// Config for the `typst` CLI compiler. `bin` is looked up on `PATH` unless
/// it's an absolute/relative path itself (`std::process::Command`'s usual
/// resolution). Build it with [`CliCompiler::new`] and the `with_*`
/// methods: it is `#[non_exhaustive]` so a new knob is not a breaking change.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CliCompiler {
    pub bin: String,
    /// Wall-clock budget for one child — the compile, and separately the
    /// SVG-outline child when `outline` is set. Exceeded → the child is
    /// killed and reaped before the error is returned, so the caller never
    /// sees a lingering process.
    pub timeout: Duration,
    /// Address-space cap applied to each child via `setrlimit(RLIMIT_AS)`.
    /// Unix-only (see `Cargo.toml`'s `cfg(unix)` `libc` dependency) — `None`,
    /// or any value on a non-unix target, leaves the child unbounded.
    pub max_memory_bytes: Option<u64>,
    /// The child that runs the SVG-outline step (issue #92) under the same
    /// `timeout`/`max_memory_bytes` as the compile. `None` outlines
    /// in-process instead — unbounded, with a one-time warning; the `mdcast`
    /// and `mdcast-server` binaries always set it to themselves.
    pub outline: Option<OutlineCommand>,
}

/// A program that, run with `args`, speaks the SVG-outline child protocol on
/// stdin/stdout — in practice one of mdcast's own binaries in its hidden
/// [`OUTLINE_CHILD_ARG`](crate::backends::typst::OUTLINE_CHILD_ARG) mode.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct OutlineCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl OutlineCommand {
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// The running executable in its hidden outline-child mode — what a
    /// binary that dispatches [`OUTLINE_CHILD_ARG`](crate::backends::typst::OUTLINE_CHILD_ARG)
    /// itself passes to [`CliCompiler::with_outline`].
    pub fn current_exe() -> Result<Self> {
        // `/proc/self/exe` names the running image itself, so it still execs
        // the same binary after a deploy replaces or deletes the file on
        // disk; `current_exe()`'s path would then run the new binary (child
        // protocol skew) or nothing at all.
        #[cfg(target_os = "linux")]
        let program = PathBuf::from("/proc/self/exe");
        #[cfg(not(target_os = "linux"))]
        let program = std::env::current_exe().context("locate the running executable")?;
        Ok(Self::new(
            program,
            [crate::backends::typst::OUTLINE_CHILD_ARG],
        ))
    }
}

impl CliCompiler {
    pub fn new(bin: impl Into<String>, timeout: Duration) -> Self {
        Self {
            bin: bin.into(),
            timeout,
            max_memory_bytes: None,
            outline: None,
        }
    }

    pub fn with_max_memory_bytes(mut self, bytes: Option<u64>) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    pub fn with_outline(mut self, outline: Option<OutlineCommand>) -> Self {
        self.outline = outline;
        self
    }

    pub(in crate::backends::typst) fn bounds(&self) -> Bounds {
        Bounds {
            timeout: self.timeout,
            max_memory_bytes: self.max_memory_bytes,
        }
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

        let done = run_bounded(cmd, None, self.bounds(), 0, "typst compile").await?;
        if !done.status.success() {
            bail!(
                "typst compile failed ({}):\n{}",
                done.status,
                String::from_utf8_lossy(&done.stderr)
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

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
