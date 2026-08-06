//! `render-template` subcommand: user-supplied typst template + a JSON data
//! file → PDF or HTML, no markdown involved. Thin wrapper over
//! `mdcast::backends::typst::render_template_with`/`render_template_html_with`
//! — mirrors `load_doc`'s `--assets`/`--brand` wiring in `main.rs`, but
//! there's no `ResolvedDoc` to build here (see `TemplateDoc`'s module docs
//! for why).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use mdcast::backends::typst::{
    TemplateDoc, TypstCompiler, render_template_html_with, render_template_with,
};
use mdcast::{
    Artifact, AssetProvider, BrandHandle, DocMeta, EmbeddedAssets, LayeredAssets, RenderedArtifact,
};

use crate::fs_assets::FsAssets;
use crate::load_brand;

/// Output format for `render-template`. Both variants are always available —
/// selecting `Html` with `--typst-compiler library` (the default) still
/// needs the `typst-html` cargo feature and fails at compile time otherwise
/// (from `mdcast::backends::typst::render_template_html_with`); with
/// `--typst-compiler cli` it needs no such build feature (issue #73).
#[derive(Clone, Copy, Default, clap::ValueEnum)]
pub enum Format {
    #[default]
    Pdf,
    Html,
}

pub async fn run(
    template: String,
    data: PathBuf,
    out: PathBuf,
    brand: Option<PathBuf>,
    assets: Option<PathBuf>,
    format: Format,
    compiler: &TypstCompiler,
) -> Result<Artifact> {
    let brand_spec = load_brand(brand.as_deref()).await?;
    let data = load_data(&data).await?;
    let doc = TemplateDoc {
        template,
        data,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(brand_spec)),
        // The CLI reads templates from a real filesystem, where host font
        // search already sees any face the user cares about — no flag yet.
        fonts: Vec::new(),
    };

    let rendered = match assets {
        Some(dir) => {
            let provider = LayeredAssets {
                over: FsAssets(dir),
                base: EmbeddedAssets,
            };
            render(&doc, &provider, format, compiler).await?
        }
        None => render(&doc, &EmbeddedAssets, format, compiler).await?,
    };
    rendered.write_to(&out).await
}

async fn render(
    doc: &TemplateDoc,
    provider: &dyn AssetProvider,
    format: Format,
    compiler: &TypstCompiler,
) -> Result<RenderedArtifact> {
    match format {
        Format::Pdf => render_template_with(doc, provider, compiler).await,
        Format::Html => render_template_html_with(doc, provider, compiler).await,
    }
}

async fn load_data(path: &Path) -> Result<serde_json::Value> {
    let s = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parse {} as JSON", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_data_parses_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        tokio::fs::write(&path, r#"{"number": "INV-042", "total": 12.5}"#)
            .await
            .unwrap();

        let value = load_data(&path).await.unwrap();

        assert_eq!(value["number"], "INV-042");
        assert_eq!(value["total"], 12.5);
    }

    #[tokio::test]
    async fn load_data_rejects_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        tokio::fs::write(&path, "not json").await.unwrap();

        let err = load_data(&path).await.unwrap_err();
        assert!(err.to_string().contains("data.json"));
    }

    fn invoice_template() -> &'static str {
        "#let invoice = json(\"/data.json\")\n= Invoice #invoice.number"
    }

    async fn invoice_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("templates"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("templates/invoice.typ"), invoice_template())
            .await
            .unwrap();
        dir
    }

    /// End-to-end through the `--format html` flag: `run` picks
    /// `render_template_html_with` over `render_template_with` and the file
    /// written to `--out` is real HTML, not a PDF.
    #[cfg(feature = "typst-html")]
    #[tokio::test]
    async fn run_dispatches_to_html_export_when_format_is_html() {
        let dir = invoice_dir().await;
        let data_path = dir.path().join("data.json");
        tokio::fs::write(&data_path, r#"{"number": "INV-042"}"#)
            .await
            .unwrap();
        let out = dir.path().join("out.html");

        let artifact = run(
            "templates/invoice.typ".to_string(),
            data_path,
            out,
            None,
            Some(dir.path().to_path_buf()),
            Format::Html,
            &TypstCompiler::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("run failed: {e:#}"));

        let bytes = tokio::fs::read(&artifact.primary).await.unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(html.contains("<html"), "not an html document: {html}");
        assert!(html.contains("Invoice INV-042"), "{html}");
    }

    /// The same `--format html` flag works with `--typst-compiler cli`
    /// without the `typst-html` cargo feature (issue #73) — a real `typst`
    /// binary supports `--features html` regardless of how mdcast itself was
    /// built. Skips gracefully when no `typst` binary is on `PATH`, matching
    /// the pandoc convention in `tests/render_smoke.rs`.
    #[tokio::test]
    async fn run_dispatches_to_html_export_in_cli_mode_without_the_feature() {
        let Some(bin) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("typst"))
                .find(|p| p.is_file())
                .map(|_| "typst".to_string())
        }) else {
            eprintln!("skipping: no `typst` binary on PATH");
            return;
        };

        let dir = invoice_dir().await;
        let data_path = dir.path().join("data.json");
        tokio::fs::write(&data_path, r#"{"number": "INV-042"}"#)
            .await
            .unwrap();
        let out = dir.path().join("out.html");
        let compiler = TypstCompiler::Cli(mdcast::backends::typst::CliCompiler::new(
            bin,
            std::time::Duration::from_secs(20),
        ));

        let artifact = run(
            "templates/invoice.typ".to_string(),
            data_path,
            out,
            None,
            Some(dir.path().to_path_buf()),
            Format::Html,
            &compiler,
        )
        .await
        .unwrap_or_else(|e| panic!("run failed: {e:#}"));

        let bytes = tokio::fs::read(&artifact.primary).await.unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(html.contains("<html"), "not an html document: {html}");
        assert!(html.contains("Invoice INV-042"), "{html}");
    }
}
