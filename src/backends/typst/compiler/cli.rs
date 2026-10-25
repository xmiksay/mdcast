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
//! Beyond `--root`, the child gets nothing it does not need: no system fonts
//! (`--ignore-system-fonts`), no package source (`--package-path` and
//! `--package-cache-path` point at an empty directory, and every proxy
//! variable at a closed loopback port so a `@preview` import fails fast
//! instead of downloading — typst fetches into the cache dir whether or not
//! it is writable, so the empty dir alone is not enough), and no inherited
//! environment (`process::run_bounded`) — `MDCAST_TOKEN` never reaches it.
//! `HOME`/`XDG_CACHE_HOME` point into the temp dir so nothing it might
//! default to lands in the server's own home.
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

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::super::diagnostics;
use super::{CompileFormat, CompileInputs};
use crate::SpawnError;
use crate::process::{Bounds, run_bounded};

/// Default for [`CliCompiler::max_output_bytes`].
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Proxy target for every protocol: the discard port on loopback, where
/// nothing listens, so a package download is refused without leaving the
/// host.
const NO_NETWORK_PROXY: &str = "http://127.0.0.1:9";

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
    /// Largest artifact a compile may produce. Checked on the output file
    /// before it is read into memory, and enforced on the child as
    /// `RLIMIT_FSIZE` (plus a small margin) so it cannot fill the disk first.
    pub max_output_bytes: u64,
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
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            outline: None,
        }
    }

    pub fn with_max_memory_bytes(mut self, bytes: Option<u64>) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    pub fn with_max_output_bytes(mut self, bytes: u64) -> Self {
        self.max_output_bytes = bytes;
        self
    }

    pub fn with_outline(mut self, outline: Option<OutlineCommand>) -> Self {
        self.outline = outline;
        self
    }

    pub(in crate::backends::typst) fn bounds(&self) -> Bounds {
        // The wall-clock timeout is the real bound; CPU time only backs it
        // up, so it allows every core busy for the whole timeout plus slack.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
        Bounds {
            timeout: self.timeout,
            max_memory_bytes: self.max_memory_bytes,
            max_file_bytes: Some(self.max_output_bytes.saturating_add(1 << 20)),
            cpu_secs: Some(
                self.timeout
                    .as_secs()
                    .saturating_add(1)
                    .saturating_mul(cores)
                    .saturating_add(5),
            ),
        }
    }

    /// Layout of the temp dir: `src/` is typst's `--root` and holds only
    /// [`CompileInputs`]; fonts, the empty package dirs, `HOME` and the output
    /// sit beside it, so a template can neither read them nor plant a fake
    /// package by naming a manifest key after one.
    pub(super) async fn compile(&self, inputs: CompileInputs) -> Result<Vec<u8>> {
        let dir = tempfile::Builder::new()
            .prefix("mdcast-typst-")
            .tempdir()
            .context("create typst compile temp dir")?;
        restrict_to_owner(dir.path())?;
        let root = dir.path().join("src");
        let fonts = dir.path().join("fonts");
        let packages = dir.path().join("packages");
        let home = dir.path().join("home");
        for d in [&root, &fonts, &packages, &home] {
            tokio::fs::create_dir(d)
                .await
                .with_context(|| format!("create {}", d.display()))?;
        }

        write_inputs(&root, &fonts, &inputs).await?;

        let output = dir.path().join(match inputs.format {
            CompileFormat::Pdf => "output.pdf",
            CompileFormat::Html => "output.html",
        });
        let main = safe_join(&root, &inputs.main)?;

        let mut cmd = Command::new(&self.bin);
        // typst prints diagnostic paths relative to its cwd, not `--root`.
        cmd.current_dir(&root)
            .arg("compile")
            .arg("--root")
            .arg(&root)
            .arg("--font-path")
            .arg(&fonts)
            .arg("--ignore-system-fonts")
            .arg("--package-path")
            .arg(&packages)
            .arg("--package-cache-path")
            .arg(&packages)
            .arg("--diagnostic-format")
            .arg("short");
        if inputs.format == CompileFormat::Html {
            cmd.arg("--features")
                .arg("html")
                .arg("--format")
                .arg("html");
        }
        cmd.arg(&main).arg(&output);

        let cache = home.join(".cache");
        let proxy = OsStr::new(NO_NETWORK_PROXY);
        let env = [
            ("HOME", home.as_os_str()),
            ("XDG_CACHE_HOME", cache.as_os_str()),
            ("HTTPS_PROXY", proxy),
            ("HTTP_PROXY", proxy),
            ("ALL_PROXY", proxy),
        ];
        let done = run_bounded(cmd, &env, None, self.bounds(), 0, "typst compile").await?;
        if !done.status.success() {
            let stderr = String::from_utf8_lossy(&done.stderr);
            // clap exits 2 on a usage error, a failed compile exits 1: the
            // stderr shape alone is spoofable by a template's own message.
            if done.status.code() == Some(2) && diagnostics::is_usage_error(&stderr) {
                // The binary rejected mdcast's own flags: a deployment fault.
                return Err(SpawnError {
                    what: "typst compile".into(),
                    program: self.bin.clone(),
                    source: std::io::Error::other(stderr.trim().to_string()),
                }
                .into());
            }
            if let Some(diags) = diagnostics::from_cli_stderr(&stderr, &[&root, dir.path()]) {
                return Err(diags.into());
            }
            bail!("typst compile failed ({}):\n{stderr}", done.status);
        }

        let len = tokio::fs::metadata(&output)
            .await
            .context("stat typst compile output")?
            .len();
        if len > self.max_output_bytes {
            bail!(
                "typst compile output is {len} bytes, over the {}-byte cap",
                self.max_output_bytes
            );
        }
        tokio::fs::read(&output)
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

async fn write_inputs(root: &Path, fonts_dir: &Path, inputs: &CompileInputs) -> Result<()> {
    for (path, content) in &inputs.sources {
        write_file(root, path, content.as_bytes()).await?;
    }
    for (path, bytes) in &inputs.files {
        write_file(root, path, bytes).await?;
    }
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
