//! `Config::from_lookup` tests. Split out of `config.rs` to keep it under the
//! size cap.

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
    assert_eq!(
        cfg.typst_compiler,
        TypstCompiler::Cli(CliCompiler::new(
            DEFAULT_TYPST_BIN,
            Duration::from_secs(DEFAULT_TYPST_TIMEOUT_SECS.into())
        ))
    );
    let cores = std::thread::available_parallelism().unwrap().get();
    assert_eq!(cfg.max_concurrent_renders, cores);
    assert_eq!(cfg.render_queue, Duration::from_secs(10));
    assert!(!cfg.expose_diagnostics);
    assert_eq!(cfg.pandoc_timeout, Duration::from_secs(60));
    assert_eq!(cfg.pandoc_max_memory_bytes, None);
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
        (TYPST_MAX_OUTPUT_BYTES, "4096"),
        (MAX_CONCURRENT_RENDERS, "3"),
        (RENDER_QUEUE_SECS, "0"),
        (EXPOSE_DIAGNOSTICS, "1"),
        (PANDOC_TIMEOUT_SECS, "7"),
        (PANDOC_MAX_MEMORY_BYTES, "4096"),
    ])
    .unwrap();

    assert_eq!(cfg.pandoc_timeout, Duration::from_secs(7));
    assert_eq!(cfg.pandoc_max_memory_bytes, Some(4096));

    assert_eq!(cfg.max_concurrent_renders, 3);
    assert_eq!(cfg.render_queue, Duration::ZERO);
    assert!(cfg.expose_diagnostics);
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
                .with_max_output_bytes(4096)
        )
    );
}

#[test]
fn an_empty_prefill_dir_is_treated_as_unset() {
    let cfg = config_from(&[(TOKEN, "t"), (PREFILL_DIR, "")]).unwrap();
    assert_eq!(cfg.prefill_dir, None);
}

#[test]
fn typst_cli_is_the_default_and_library_stays_selectable() {
    for pairs in [&[(TOKEN, "t")][..], &[(TOKEN, "t"), (TYPST, "")][..]] {
        let cfg = config_from(pairs).unwrap();
        assert!(matches!(cfg.typst_compiler, TypstCompiler::Cli(_)));
    }
    let cfg = config_from(&[(TOKEN, "t"), (TYPST, "library")]).unwrap();
    assert_eq!(cfg.typst_compiler, TypstCompiler::Library);
}

#[test]
fn zero_concurrent_renders_refuses_to_start() {
    let err = config_from(&[(TOKEN, "t"), (MAX_CONCURRENT_RENDERS, "0")]).unwrap_err();
    assert!(err.to_string().contains(MAX_CONCURRENT_RENDERS), "{err}");
}

#[test]
fn expose_diagnostics_accepts_only_boolean_spellings() {
    for (raw, expected) in [("0", false), ("false", false), ("", false), ("true", true)] {
        let cfg = config_from(&[(TOKEN, "t"), (EXPOSE_DIAGNOSTICS, raw)]).unwrap();
        assert_eq!(cfg.expose_diagnostics, expected, "{raw:?}");
    }
    let err = config_from(&[(TOKEN, "t"), (EXPOSE_DIAGNOSTICS, "yes")]).unwrap_err();
    assert!(err.to_string().contains(EXPOSE_DIAGNOSTICS), "{err}");
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
