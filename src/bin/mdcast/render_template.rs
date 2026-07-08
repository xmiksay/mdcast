//! `render-template` subcommand: user-supplied typst template + a JSON data
//! file → PDF or (behind the `typst-html` feature) HTML, no markdown
//! involved. Thin wrapper over `mdcast::backends::typst::render_template`/
//! `render_template_html` — mirrors `load_doc`'s `--assets`/`--brand` wiring
//! in `main.rs`, but there's no `ResolvedDoc` to build here (see
//! `TemplateDoc`'s module docs for why).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
#[cfg(feature = "typst-html")]
use mdcast::backends::typst::render_template_html;
use mdcast::backends::typst::{TemplateDoc, render_template};
use mdcast::{
    Artifact, AssetProvider, BrandHandle, DocMeta, EmbeddedAssets, LayeredAssets, RenderedArtifact,
};

use crate::fs_assets::FsAssets;
use crate::load_brand;

/// Output format for `render-template`. `Html` only exists when the crate is
/// built with the `typst-html` feature — clap simply won't accept `--format
/// html` as a valid value otherwise.
#[derive(Clone, Copy, Default, clap::ValueEnum)]
pub enum Format {
    #[default]
    Pdf,
    #[cfg(feature = "typst-html")]
    Html,
}

pub async fn run(
    template: String,
    data: PathBuf,
    out: PathBuf,
    brand: Option<PathBuf>,
    assets: Option<PathBuf>,
    format: Format,
) -> Result<Artifact> {
    let brand_spec = load_brand(brand.as_deref()).await?;
    let data = load_data(&data).await?;
    let doc = TemplateDoc {
        template,
        data,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(brand_spec)),
    };

    let rendered = match assets {
        Some(dir) => {
            let provider = LayeredAssets {
                over: FsAssets(dir),
                base: EmbeddedAssets,
            };
            render(&doc, &provider, format).await?
        }
        None => render(&doc, &EmbeddedAssets, format).await?,
    };
    rendered.write_to(&out).await
}

async fn render(
    doc: &TemplateDoc,
    provider: &dyn AssetProvider,
    format: Format,
) -> Result<RenderedArtifact> {
    match format {
        Format::Pdf => render_template(doc, provider).await,
        #[cfg(feature = "typst-html")]
        Format::Html => render_template_html(doc, provider).await,
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

    /// End-to-end through the `--format html` flag: `run` picks
    /// `render_template_html` over `render_template` and the file written to
    /// `--out` is real HTML, not a PDF.
    #[cfg(feature = "typst-html")]
    #[tokio::test]
    async fn run_dispatches_to_html_export_when_format_is_html() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("templates"))
            .await
            .unwrap();
        tokio::fs::write(
            dir.path().join("templates/invoice.typ"),
            "#let invoice = json(\"/data.json\")\n= Invoice #invoice.number",
        )
        .await
        .unwrap();
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
        )
        .await
        .unwrap_or_else(|e| panic!("run failed: {e:#}"));

        let bytes = tokio::fs::read(&artifact.primary).await.unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(html.contains("<html"), "not an html document: {html}");
        assert!(html.contains("Invoice INV-042"), "{html}");
    }
}
