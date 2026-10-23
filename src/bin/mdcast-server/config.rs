//! Startup configuration, read from the environment. Every knob is a
//! `MDCAST_`-prefixed variable. The bearer token is optional: unset or empty
//! means the service starts unauthenticated (every `/v1` route is open) —
//! see `main::warn_if_unauthenticated` for the startup warning that fires
//! when that happens.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mdcast::backends::typst::{CliCompiler, DEFAULT_MAX_OUTPUT_BYTES, TypstCompiler};

pub const BIND: &str = "MDCAST_BIND";
pub const TOKEN: &str = "MDCAST_TOKEN";
pub const MAX_CACHE_BYTES: &str = "MDCAST_MAX_CACHE_BYTES";
pub const MAX_UPLOAD_BYTES: &str = "MDCAST_MAX_UPLOAD_BYTES";
pub const CACHE_LIVES: &str = "MDCAST_CACHE_LIVES";
pub const MAX_CACHE_LIVES: &str = "MDCAST_MAX_CACHE_LIVES";
pub const PREFILL_DIR: &str = "MDCAST_PREFILL_DIR";
pub const TYPST: &str = "MDCAST_TYPST";
pub const TYPST_BIN: &str = "MDCAST_TYPST_BIN";
pub const TYPST_TIMEOUT_SECS: &str = "MDCAST_TYPST_TIMEOUT_SECS";
pub const TYPST_MAX_MEMORY_BYTES: &str = "MDCAST_TYPST_MAX_MEMORY_BYTES";
pub const TYPST_MAX_OUTPUT_BYTES: &str = "MDCAST_TYPST_MAX_OUTPUT_BYTES";
pub const MAX_CONCURRENT_RENDERS: &str = "MDCAST_MAX_CONCURRENT_RENDERS";
pub const RENDER_QUEUE_SECS: &str = "MDCAST_RENDER_QUEUE_SECS";
pub const EXPOSE_DIAGNOSTICS: &str = "MDCAST_EXPOSE_DIAGNOSTICS";

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_CACHE_LIVES: u32 = 3;
const DEFAULT_MAX_CACHE_LIVES: u32 = 32;
const DEFAULT_TYPST_BIN: &str = "typst";
const DEFAULT_TYPST_TIMEOUT_SECS: u32 = 30;
const DEFAULT_RENDER_QUEUE_SECS: u32 = 10;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// `None` when `MDCAST_TOKEN` is unset or empty — the router then skips
    /// the bearer-auth layer entirely instead of applying one that would
    /// authorise everything.
    pub token: Option<String>,
    /// Total in-memory budget for the blob cache. Exceeding it evicts
    /// least-recently-used, second-chance-drained blobs on insert.
    pub max_cache_bytes: u64,
    /// Body-size ceiling for *every* request, not just uploads — one knob, so
    /// a caller can't sidestep it by posting a giant render request instead of
    /// a giant blob.
    pub max_upload_bytes: usize,
    /// Eviction-survival chances a newly-cached blob starts with.
    pub cache_lives: u32,
    /// Ceiling repeated hits can grow an entry's lives to.
    pub max_cache_lives: u32,
    /// Directory of predefined assets to pin into the blob store at startup
    /// (issue #72). `None` when `MDCAST_PREFILL_DIR` is unset — prefill is
    /// entirely opt-in.
    pub prefill_dir: Option<PathBuf>,
    /// Which Typst compiler `/v1/render` and `/v1/render/template` use
    /// (issue #73). Defaults to `Cli` (issue #96): only a child process can
    /// be killed on timeout and resource-capped, and the server compiles
    /// templates it did not write. `library` stays selectable.
    pub typst_compiler: TypstCompiler,
    /// Renders allowed to run at once; the rest queue for a slot (issue #96).
    pub max_concurrent_renders: usize,
    /// How long a render waits for a slot before it is answered `503`.
    /// Zero means it does not wait at all.
    pub render_queue: Duration,
    /// Whether a `render_failed` response carries the typst diagnostics as
    /// `detail`. Off by default: the response then names only an incident.
    pub expose_diagnostics: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Environment reads go through a closure so the tests can exercise
    /// parsing and defaulting without mutating the process environment (which
    /// is global, and therefore racy under a parallel test runner).
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let token = get(TOKEN).filter(|t| !t.is_empty());

        let bind: SocketAddr = get(BIND)
            .unwrap_or_else(|| DEFAULT_BIND.to_string())
            .parse()
            .with_context(|| format!("{BIND} must be an ADDR:PORT socket address"))?;

        let cache_lives = parse_u32(get(CACHE_LIVES), CACHE_LIVES)?.unwrap_or(DEFAULT_CACHE_LIVES);
        let max_cache_lives =
            parse_u32(get(MAX_CACHE_LIVES), MAX_CACHE_LIVES)?.unwrap_or(DEFAULT_MAX_CACHE_LIVES);
        if max_cache_lives < cache_lives {
            bail!("{MAX_CACHE_LIVES} ({max_cache_lives}) must be >= {CACHE_LIVES} ({cache_lives})");
        }

        Ok(Self {
            bind,
            token,
            max_cache_bytes: parse_bytes(get(MAX_CACHE_BYTES), MAX_CACHE_BYTES)?
                .unwrap_or(DEFAULT_MAX_CACHE_BYTES),
            max_upload_bytes: parse_bytes(get(MAX_UPLOAD_BYTES), MAX_UPLOAD_BYTES)?
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
                .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES),
            cache_lives,
            max_cache_lives,
            prefill_dir: get(PREFILL_DIR)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            typst_compiler: parse_typst_compiler(&get)?,
            max_concurrent_renders: parse_concurrency(get(MAX_CONCURRENT_RENDERS))?,
            render_queue: Duration::from_secs(
                parse_u32(get(RENDER_QUEUE_SECS), RENDER_QUEUE_SECS)?
                    .unwrap_or(DEFAULT_RENDER_QUEUE_SECS)
                    .into(),
            ),
            expose_diagnostics: parse_flag(get(EXPOSE_DIAGNOSTICS), EXPOSE_DIAGNOSTICS)?,
        })
    }
}

/// Byte counts are plain integers — no `MiB` suffix parsing, deliberately: one
/// spelling per value keeps deployment manifests unambiguous.
fn parse_bytes(raw: Option<String>, name: &str) -> Result<Option<u64>> {
    raw.map(|s| {
        s.trim()
            .parse::<u64>()
            .with_context(|| format!("{name} must be a byte count (a plain integer), got {s:?}"))
    })
    .transpose()
}

fn parse_u32(raw: Option<String>, name: &str) -> Result<Option<u32>> {
    raw.map(|s| {
        s.trim()
            .parse::<u32>()
            .with_context(|| format!("{name} must be a non-negative integer, got {s:?}"))
    })
    .transpose()
}

/// Defaults to the machine's parallelism: a render is mostly one busy core
/// (a typst compile, a pandoc run), so more slots than cores only adds
/// memory pressure. Zero would refuse every render, so it fails startup.
fn parse_concurrency(raw: Option<String>) -> Result<usize> {
    match parse_u32(raw, MAX_CONCURRENT_RENDERS)? {
        Some(0) => bail!("{MAX_CONCURRENT_RENDERS} must be at least 1"),
        Some(n) => Ok(n as usize),
        None => Ok(std::thread::available_parallelism().map_or(1, |n| n.get())),
    }
}

fn parse_flag(raw: Option<String>, name: &str) -> Result<bool> {
    match raw.as_deref().map(str::trim) {
        None | Some("" | "0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(other) => bail!("{name} must be 1/true or 0/false, got {other:?}"),
    }
}

/// `MDCAST_TYPST` selects the compiler (`cli` unless set); `cli` mode reads
/// the remaining `MDCAST_TYPST_*` variables, all of which are meaningless
/// (and ignored) in `library` mode.
fn parse_typst_compiler(get: &impl Fn(&str) -> Option<String>) -> Result<TypstCompiler> {
    match get(TYPST).as_deref().unwrap_or("cli").trim() {
        "library" => Ok(TypstCompiler::Library),
        "" | "cli" => {
            let bin = get(TYPST_BIN)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_TYPST_BIN.to_string());
            let timeout_secs = parse_u32(get(TYPST_TIMEOUT_SECS), TYPST_TIMEOUT_SECS)?
                .unwrap_or(DEFAULT_TYPST_TIMEOUT_SECS);
            let max_memory_bytes =
                parse_bytes(get(TYPST_MAX_MEMORY_BYTES), TYPST_MAX_MEMORY_BYTES)?;
            let max_output_bytes =
                parse_bytes(get(TYPST_MAX_OUTPUT_BYTES), TYPST_MAX_OUTPUT_BYTES)?
                    .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);
            Ok(TypstCompiler::Cli(
                CliCompiler::new(bin, Duration::from_secs(timeout_secs.into()))
                    .with_max_memory_bytes(max_memory_bytes)
                    .with_max_output_bytes(max_output_bytes),
            ))
        }
        other => bail!("{TYPST} must be \"library\" or \"cli\", got {other:?}"),
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
