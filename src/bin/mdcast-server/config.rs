//! Startup configuration, read from the environment. Every knob is a
//! `MDCAST_`-prefixed variable. The bearer token is optional: unset or empty
//! means the service starts unauthenticated (every `/v1` route is open) —
//! see `main::warn_if_unauthenticated` for the startup warning that fires
//! when that happens.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mdcast::backends::typst::{CliCompiler, TypstCompiler};

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

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_CACHE_LIVES: u32 = 3;
const DEFAULT_MAX_CACHE_LIVES: u32 = 32;
const DEFAULT_TYPST_BIN: &str = "typst";
const DEFAULT_TYPST_TIMEOUT_SECS: u32 = 30;

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
    /// (issue #73). Defaults to `Library` — an existing deployment that sets
    /// none of the `MDCAST_TYPST*` variables sees byte-identical behaviour.
    pub typst_compiler: TypstCompiler,
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

/// `MDCAST_TYPST` selects the compiler; `cli` mode reads the remaining
/// `MDCAST_TYPST_*` variables, all of which are meaningless (and ignored) in
/// `library` mode.
fn parse_typst_compiler(get: &impl Fn(&str) -> Option<String>) -> Result<TypstCompiler> {
    match get(TYPST).as_deref().unwrap_or("library").trim() {
        "" | "library" => Ok(TypstCompiler::Library),
        "cli" => {
            let bin = get(TYPST_BIN)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_TYPST_BIN.to_string());
            let timeout_secs = parse_u32(get(TYPST_TIMEOUT_SECS), TYPST_TIMEOUT_SECS)?
                .unwrap_or(DEFAULT_TYPST_TIMEOUT_SECS);
            let max_memory_bytes =
                parse_bytes(get(TYPST_MAX_MEMORY_BYTES), TYPST_MAX_MEMORY_BYTES)?;
            Ok(TypstCompiler::Cli(
                CliCompiler::new(bin, Duration::from_secs(timeout_secs.into()))
                    .with_max_memory_bytes(max_memory_bytes),
            ))
        }
        other => bail!("{TYPST} must be \"library\" or \"cli\", got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_from(pairs: &[(&str, &str)]) -> Result<Config> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn token_alone_is_enough_and_everything_else_defaults() {
        let cfg = config_from(&[(TOKEN, "s3cret")]).unwrap();

        assert_eq!(cfg.token.as_deref(), Some("s3cret"));
        assert_eq!(cfg.bind.to_string(), DEFAULT_BIND);
        assert_eq!(cfg.max_cache_bytes, DEFAULT_MAX_CACHE_BYTES);
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(cfg.cache_lives, DEFAULT_CACHE_LIVES);
        assert_eq!(cfg.max_cache_lives, DEFAULT_MAX_CACHE_LIVES);
        assert_eq!(cfg.prefill_dir, None);
        assert_eq!(cfg.typst_compiler, TypstCompiler::Library);
    }

    #[test]
    fn a_missing_or_empty_token_starts_unauthenticated_rather_than_erroring() {
        for pairs in [&[][..], &[(TOKEN, "")][..]] {
            let cfg = config_from(pairs).unwrap();
            assert_eq!(cfg.token, None);
        }
    }

    #[test]
    fn every_variable_is_honoured() {
        let cfg = config_from(&[
            (TOKEN, "t"),
            (BIND, "0.0.0.0:9000"),
            (MAX_CACHE_BYTES, "1024"),
            (MAX_UPLOAD_BYTES, "2048"),
            (CACHE_LIVES, "5"),
            (MAX_CACHE_LIVES, "50"),
            (PREFILL_DIR, "/srv/mdcast/prefill"),
            (TYPST, "cli"),
            (TYPST_BIN, "/opt/typst/typst"),
            (TYPST_TIMEOUT_SECS, "5"),
            (TYPST_MAX_MEMORY_BYTES, "1024"),
        ])
        .unwrap();

        assert_eq!(cfg.bind.to_string(), "0.0.0.0:9000");
        assert_eq!(cfg.max_cache_bytes, 1024);
        assert_eq!(cfg.max_upload_bytes, 2048);
        assert_eq!(cfg.cache_lives, 5);
        assert_eq!(cfg.max_cache_lives, 50);
        assert_eq!(cfg.prefill_dir, Some(PathBuf::from("/srv/mdcast/prefill")));
        assert_eq!(
            cfg.typst_compiler,
            TypstCompiler::Cli(
                CliCompiler::new("/opt/typst/typst", Duration::from_secs(5))
                    .with_max_memory_bytes(Some(1024))
            )
        );
    }

    #[test]
    fn an_empty_prefill_dir_is_treated_as_unset() {
        let cfg = config_from(&[(TOKEN, "t"), (PREFILL_DIR, "")]).unwrap();
        assert_eq!(cfg.prefill_dir, None);
    }

    #[test]
    fn typst_library_is_the_default_and_the_explicit_choice() {
        for pairs in [&[(TOKEN, "t")][..], &[(TOKEN, "t"), (TYPST, "library")][..]] {
            let cfg = config_from(pairs).unwrap();
            assert_eq!(cfg.typst_compiler, TypstCompiler::Library);
        }
    }

    #[test]
    fn typst_cli_reads_bin_timeout_and_memory_cap() {
        let cfg = config_from(&[
            (TOKEN, "t"),
            (TYPST, "cli"),
            (TYPST_BIN, "/opt/typst/typst"),
            (TYPST_TIMEOUT_SECS, "5"),
            (TYPST_MAX_MEMORY_BYTES, "1073741824"),
        ])
        .unwrap();

        assert_eq!(
            cfg.typst_compiler,
            TypstCompiler::Cli(
                CliCompiler::new("/opt/typst/typst", Duration::from_secs(5))
                    .with_max_memory_bytes(Some(1024 * 1024 * 1024))
            )
        );
    }

    #[test]
    fn typst_cli_defaults_bin_and_timeout_when_unset() {
        let cfg = config_from(&[(TOKEN, "t"), (TYPST, "cli")]).unwrap();

        assert_eq!(
            cfg.typst_compiler,
            TypstCompiler::Cli(CliCompiler::new(
                DEFAULT_TYPST_BIN,
                Duration::from_secs(DEFAULT_TYPST_TIMEOUT_SECS.into())
            ))
        );
    }

    #[test]
    fn an_unknown_typst_mode_names_the_variable() {
        let err = config_from(&[(TOKEN, "t"), (TYPST, "subprocess")]).unwrap_err();
        assert!(err.to_string().contains(TYPST), "{err}");
    }

    #[test]
    fn a_malformed_value_names_its_variable() {
        let err = config_from(&[(TOKEN, "t"), (BIND, "not-an-address")]).unwrap_err();
        assert!(err.to_string().contains(BIND), "{err}");

        let err = config_from(&[(TOKEN, "t"), (MAX_CACHE_BYTES, "2GiB")]).unwrap_err();
        assert!(err.to_string().contains(MAX_CACHE_BYTES), "{err}");

        let err = config_from(&[(TOKEN, "t"), (CACHE_LIVES, "not-a-number")]).unwrap_err();
        assert!(err.to_string().contains(CACHE_LIVES), "{err}");

        let err = config_from(&[(TOKEN, "t"), (MAX_CACHE_LIVES, "not-a-number")]).unwrap_err();
        assert!(err.to_string().contains(MAX_CACHE_LIVES), "{err}");
    }

    #[test]
    fn max_cache_lives_below_cache_lives_refuses_to_start() {
        let err =
            config_from(&[(TOKEN, "t"), (CACHE_LIVES, "10"), (MAX_CACHE_LIVES, "5")]).unwrap_err();
        assert!(err.to_string().contains(CACHE_LIVES), "{err}");
        assert!(err.to_string().contains(MAX_CACHE_LIVES), "{err}");
    }
}
