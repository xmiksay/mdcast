//! Startup configuration, read from the environment. Every knob is a
//! `MDCAST_`-prefixed variable; only the bearer token has no default, because
//! a render service that silently accepts every request when the operator
//! forgot one env var is the worst possible failure mode here.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub const BIND: &str = "MDCAST_BIND";
pub const TOKEN: &str = "MDCAST_TOKEN";
pub const BLOB_DIR: &str = "MDCAST_BLOB_DIR";
pub const MAX_CACHE_BYTES: &str = "MDCAST_MAX_CACHE_BYTES";
pub const MAX_UPLOAD_BYTES: &str = "MDCAST_MAX_UPLOAD_BYTES";

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_BLOB_DIR: &str = "/var/lib/mdcast/blobs";
const DEFAULT_MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub token: String,
    pub blob_dir: PathBuf,
    /// Total on-disk budget for the blob cache. Exceeding it evicts the
    /// least-recently-used blobs after an upload.
    pub max_cache_bytes: u64,
    /// Body-size ceiling for *every* request, not just uploads — one knob, so
    /// a caller can't sidestep it by posting a giant render request instead of
    /// a giant blob.
    pub max_upload_bytes: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Environment reads go through a closure so the tests can exercise
    /// parsing and defaulting without mutating the process environment (which
    /// is global, and therefore racy under a parallel test runner).
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let token = get(TOKEN).unwrap_or_default();
        if token.is_empty() {
            bail!(
                "{TOKEN} is unset or empty — refusing to start an unauthenticated render service"
            );
        }

        let bind: SocketAddr = get(BIND)
            .unwrap_or_else(|| DEFAULT_BIND.to_string())
            .parse()
            .with_context(|| format!("{BIND} must be an ADDR:PORT socket address"))?;

        Ok(Self {
            bind,
            token,
            blob_dir: get(BLOB_DIR)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_BLOB_DIR)),
            max_cache_bytes: parse_bytes(get(MAX_CACHE_BYTES), MAX_CACHE_BYTES)?
                .unwrap_or(DEFAULT_MAX_CACHE_BYTES),
            max_upload_bytes: parse_bytes(get(MAX_UPLOAD_BYTES), MAX_UPLOAD_BYTES)?
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
                .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES),
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

        assert_eq!(cfg.token, "s3cret");
        assert_eq!(cfg.bind.to_string(), DEFAULT_BIND);
        assert_eq!(cfg.blob_dir, PathBuf::from(DEFAULT_BLOB_DIR));
        assert_eq!(cfg.max_cache_bytes, DEFAULT_MAX_CACHE_BYTES);
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
    }

    #[test]
    fn a_missing_or_empty_token_refuses_to_start() {
        for pairs in [&[][..], &[(TOKEN, "")][..]] {
            let err = config_from(pairs).unwrap_err();
            assert!(
                err.to_string().contains(TOKEN),
                "startup error should name the variable: {err}"
            );
        }
    }

    #[test]
    fn every_variable_is_honoured() {
        let cfg = config_from(&[
            (TOKEN, "t"),
            (BIND, "0.0.0.0:9000"),
            (BLOB_DIR, "/srv/blobs"),
            (MAX_CACHE_BYTES, "1024"),
            (MAX_UPLOAD_BYTES, "2048"),
        ])
        .unwrap();

        assert_eq!(cfg.bind.to_string(), "0.0.0.0:9000");
        assert_eq!(cfg.blob_dir, PathBuf::from("/srv/blobs"));
        assert_eq!(cfg.max_cache_bytes, 1024);
        assert_eq!(cfg.max_upload_bytes, 2048);
    }

    #[test]
    fn a_malformed_value_names_its_variable() {
        let err = config_from(&[(TOKEN, "t"), (BIND, "not-an-address")]).unwrap_err();
        assert!(err.to_string().contains(BIND), "{err}");

        let err = config_from(&[(TOKEN, "t"), (MAX_CACHE_BYTES, "2GiB")]).unwrap_err();
        assert!(err.to_string().contains(MAX_CACHE_BYTES), "{err}");
    }
}
