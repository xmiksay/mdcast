//! Startup probe for pandoc availability (issue #81): the pandoc-backed
//! targets (docx/odt/pptx/html-reveal) only appear in
//! `GET /v1/capabilities` when a working `pandoc` binary was found at
//! startup — the same shape of check `CliCompiler::log_version` already does
//! for `typst` in `cli` mode.

use tokio::process::Command;

/// Runs `pandoc --version` once. `false` on any failure to spawn or a
/// non-zero exit — the same treatment a missing `typst` binary gets.
pub async fn probe_pandoc() -> bool {
    match Command::new("pandoc").arg("--version").output().await {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            tracing::warn!(
                status = %out.status,
                "`pandoc --version` did not succeed; docx/odt/pptx/html-reveal targets \
                 will report as unavailable"
            );
            false
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "could not run `pandoc`; docx/odt/pptx/html-reveal targets will report as \
                 unavailable"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same convention as `tests/render_smoke.rs`'s `pandoc_available()`: no
    /// `#[ignore]`, just agree with a direct probe either way, so this test
    /// passes with or without `pandoc` on `PATH`.
    #[tokio::test]
    async fn matches_a_direct_probe() {
        let expected = std::process::Command::new("pandoc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        assert_eq!(probe_pandoc().await, expected);
    }
}
