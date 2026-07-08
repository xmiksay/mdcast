//! `render-template` subcommand: user-supplied typst template + a JSON data
//! file → PDF, no markdown involved. Thin wrapper over
//! `mdcast::backends::typst::render_template` — mirrors `load_doc`'s
//! `--assets`/`--brand` wiring in `main.rs`, but there's no `ResolvedDoc` to
//! build here (see `TemplateDoc`'s module docs for why).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use mdcast::backends::typst::{TemplateDoc, render_template};
use mdcast::{Artifact, BrandHandle, DocMeta, EmbeddedAssets, LayeredAssets};

use crate::fs_assets::FsAssets;
use crate::load_brand;

pub async fn run(
    template: String,
    data: PathBuf,
    out: PathBuf,
    brand: Option<PathBuf>,
    assets: Option<PathBuf>,
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
            render_template(&doc, &provider).await?
        }
        None => render_template(&doc, &EmbeddedAssets).await?,
    };
    rendered.write_to(&out).await
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
}
