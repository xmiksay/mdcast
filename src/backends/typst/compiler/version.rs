//! The startup check for `TypstCompiler::Cli`: the configured binary must run
//! and be new enough for every flag `cli::compile` passes.
//!
//! 0.13 is the floor because it is the first release with `--features html`
//! and `--format html` (HTML template output); `--ignore-system-fonts`,
//! `--package-path`, `--package-cache-path` and `--diagnostic-format` all
//! predate it. Checking at startup turns "every PDF render is a 500" into a
//! deployment that refuses to start.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::CliCompiler;
use crate::process::{Bounds, run_bounded};

/// Oldest `typst` CLI `cli` mode supports.
pub const MIN_TYPST_CLI_VERSION: (u64, u64, u64) = (0, 13, 0);

impl CliCompiler {
    /// Runs `<bin> --version`, fails unless it reports at least
    /// [`MIN_TYPST_CLI_VERSION`], and logs what it found.
    pub(super) async fn check_version(&self) -> Result<()> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--version");
        let bounds = Bounds {
            timeout: Duration::from_secs(10),
            max_memory_bytes: None,
            max_file_bytes: None,
            cpu_secs: None,
        };
        let done = run_bounded(cmd, &[], None, bounds, 4096, "typst --version").await?;
        let stdout = String::from_utf8_lossy(&done.stdout);
        if !done.status.success() {
            bail!(
                "`{} --version` exited with {}: {}",
                self.bin,
                done.status,
                String::from_utf8_lossy(&done.stderr).trim()
            );
        }
        let version = parse_version(&stdout).with_context(|| {
            format!(
                "`{} --version` printed no version: {:?}",
                self.bin,
                stdout.trim()
            )
        })?;
        if version < MIN_TYPST_CLI_VERSION {
            let (major, minor, patch) = MIN_TYPST_CLI_VERSION;
            bail!(
                "`{}` reports {:?} — older than {major}.{minor}.{patch}, the first release \
                 supporting every flag mdcast passes",
                self.bin,
                stdout.trim()
            );
        }
        tracing::info!(bin = %self.bin, version = stdout.trim(), "typst CLI compiler selected");
        Ok(())
    }
}

/// The first `MAJOR.MINOR[.PATCH][-suffix]` token of `typst --version`'s
/// output (`typst 0.14.2 (b33de9de)`).
fn parse_version(output: &str) -> Option<(u64, u64, u64)> {
    output.split_whitespace().find_map(|token| {
        let core = token.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let major = parts.next()??;
        let minor = parts.next()??;
        let patch = parts.next().unwrap_or(Some(0))?;
        parts.next().is_none().then_some((major, minor, patch))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_from_real_and_odd_outputs() {
        assert_eq!(parse_version("typst 0.14.2 (b33de9de)"), Some((0, 14, 2)));
        assert_eq!(parse_version("typst 0.13.0-rc1 (abc)\n"), Some((0, 13, 0)));
        assert_eq!(parse_version("typst 1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("typst nightly"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn the_floor_orders_below_supported_and_above_too_old() {
        assert!(parse_version("typst 0.12.0").unwrap() < MIN_TYPST_CLI_VERSION);
        assert!(parse_version("typst 0.13.1").unwrap() >= MIN_TYPST_CLI_VERSION);
        assert!(parse_version("typst 1.0.0").unwrap() >= MIN_TYPST_CLI_VERSION);
    }

    #[tokio::test]
    async fn a_missing_binary_fails_the_check() {
        let cli = CliCompiler::new("/nonexistent/typst", Duration::from_secs(1));
        let err = cli.check_version().await.unwrap_err();
        assert!(err.downcast_ref::<crate::SpawnError>().is_some(), "{err:#}");
    }

    /// A binary that runs but prints no version fails too.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_binary_without_a_version_fails_the_check() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // Not named `typst`: cli_tests' leak test counts `typst` children.
        let fake = dir.path().join("fake-typst");
        std::fs::write(&fake, "#!/bin/sh\necho typst nightly\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cli = CliCompiler::new(fake.display().to_string(), Duration::from_secs(5));
        let err = cli.check_version().await.unwrap_err();
        assert!(format!("{err:#}").contains("no version"), "{err:#}");
    }
}
