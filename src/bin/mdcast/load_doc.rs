//! Markdown file → `ResolvedDoc`: frontmatter, the string preprocessors, the
//! image-producing pre-steps (mermaid, QR), splitting and classification.
//! Shared by `render` and `explain`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use mdcast::pages::auto::classify;
use mdcast::pages::splitter::DefaultSplitter;
use mdcast::{
    AssetProvider, AssetRef, BrandHandle, BrandSpec, HtmlImageTags, Identity, MarkdownPreprocessor,
    PageSplitter, ResolvedDoc,
};

/// The opt-in rewrites that run before splitting — flattened onto both
/// `render` and `explain`, so the classifier `explain` reports on sees the
/// same document `render` would.
#[derive(clap::Args, Debug, Default, Clone, Copy)]
pub struct PreSteps {
    /// Enable the built-in HtmlImageTags preprocessor: `<img src="X">` /
    /// `<image path="X">` become standard `![](X)` before splitting.
    #[arg(long, default_value_t = false)]
    pub html_image_tags: bool,
    /// Render ```mermaid fenced code blocks to SVG diagrams (pure Rust,
    /// no Node/Chromium) before splitting; the SVGs resolve through the
    /// asset pipeline like any other image. Needs the `mermaid` feature
    /// (on by default). A diagram that fails to render warns and stays a
    /// code block.
    #[arg(long, default_value_t = false)]
    pub mermaid: bool,
    /// Render ```qr fences and `<qr>…</qr>` / `<qr path="KEY">` tags to SVG
    /// QR codes before splitting (`ec="L|M|Q|H"`, `size="small|large"`).
    /// Needs the `qr` feature (on by default). A block that fails warns and
    /// stays as written.
    #[arg(long, default_value_t = false)]
    pub qr: bool,
}

/// Shared by `load_doc` (markdown pipeline) and `render_template::run`
/// (data-driven template pipeline) — both accept the same `--brand brand.toml`.
pub async fn load_brand(brand: Option<&Path>) -> Result<BrandSpec> {
    match brand {
        Some(p) => {
            let s = tokio::fs::read_to_string(p)
                .await
                .with_context(|| format!("read {}", p.display()))?;
            mdcast::brand::from_toml(&s).with_context(|| format!("parse {}", p.display()))
        }
        None => Ok(BrandSpec::default()),
    }
}

/// Returns the document plus the SVGs the pre-steps produced, for the caller
/// to layer over `assets`. `assets` is also what `<qr path="…">` reads.
pub async fn load_doc(
    input: &Path,
    brand: Option<&Path>,
    steps: PreSteps,
    assets: &dyn AssetProvider,
    toc_depth: Option<u8>,
    layout_assets: Vec<String>,
    layout_fonts: Vec<String>,
) -> Result<(ResolvedDoc, Vec<(String, Bytes)>)> {
    let md = tokio::fs::read_to_string(input)
        .await
        .with_context(|| format!("read {}", input.display()))?;
    let (meta, md) = mdcast::frontmatter::extract(&md);
    let brand_spec = load_brand(brand).await?;
    // Preprocessor stage: rewrites the whole document before any other pipeline
    // step sees it. The CLI exposes only the built-in HtmlImageTags via flag;
    // library callers compose their own MarkdownPreprocessor.
    let preprocessor: Box<dyn MarkdownPreprocessor> = if steps.html_image_tags {
        Box::new(HtmlImageTags)
    } else {
        Box::new(Identity)
    };
    let md = preprocessor.preprocess(&md);

    // Image-producing pre-steps: after the plain-string preprocessors (an
    // <img> tag rewrite can't affect a fence), before splitting, so the
    // classifier and both engines see each result as a real image node.
    #[cfg_attr(not(any(feature = "mermaid", feature = "qr")), allow(unused_mut))]
    let mut svgs = Vec::new();
    #[cfg(feature = "mermaid")]
    let md = if steps.mermaid {
        let rendered = mdcast::mermaid::render_diagrams(&md);
        svgs.extend(rendered.svgs);
        rendered.markdown
    } else {
        md
    };
    #[cfg(not(feature = "mermaid"))]
    anyhow::ensure!(
        !steps.mermaid,
        "this mdcast build does not include the `mermaid` feature"
    );
    #[cfg(feature = "qr")]
    let md = if steps.qr {
        let rendered = mdcast::generators::qr::render_qr_codes(&md, assets).await;
        svgs.extend(rendered.svgs);
        rendered.markdown
    } else {
        md
    };
    #[cfg(not(feature = "qr"))]
    {
        let _ = assets;
        anyhow::ensure!(
            !steps.qr,
            "this mdcast build does not include the `qr` feature"
        );
    }

    let raw = DefaultSplitter.split(&md);
    let pages = classify(raw, &brand_spec.auto_layout);
    let doc = ResolvedDoc {
        pages,
        meta,
        brand: BrandHandle(Arc::new(brand_spec)),
        assets: layout_assets
            .into_iter()
            .map(|key| AssetRef { key })
            .collect(),
        fonts: layout_fonts
            .into_iter()
            .map(|key| AssetRef { key })
            .collect(),
        toc: toc_depth,
    };
    Ok((doc, svgs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdcast::EmbeddedAssets;

    async fn load(
        markdown: &str,
        steps: PreSteps,
        assets: Vec<String>,
        fonts: Vec<String>,
    ) -> (ResolvedDoc, Vec<(String, Bytes)>) {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.md");
        tokio::fs::write(&input, markdown).await.unwrap();
        load_doc(&input, None, steps, &EmbeddedAssets, None, assets, fonts)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn load_doc_turns_layout_asset_flags_into_asset_refs() {
        let assets = vec!["branding/logo.svg".to_string(), "bg.png".to_string()];
        let (doc, _) = load("# Hi", PreSteps::default(), assets, Vec::new()).await;

        let keys: Vec<&str> = doc.assets.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["branding/logo.svg", "bg.png"]);
    }

    #[tokio::test]
    async fn load_doc_turns_layout_font_flags_into_font_refs() {
        let fonts = vec!["fonts/Brand-Regular.ttf".to_string()];
        let (doc, _) = load("# Hi", PreSteps::default(), Vec::new(), fonts).await;

        let keys: Vec<&str> = doc.fonts.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["fonts/Brand-Regular.ttf"]);
    }

    #[cfg(feature = "qr")]
    #[tokio::test]
    async fn qr_flag_turns_qr_fences_into_image_refs() {
        let md = "```qr\nhello\n```\n";
        let (plain, none) = load(md, PreSteps::default(), Vec::new(), Vec::new()).await;
        assert!(plain.pages[0].body.contains("```qr"));
        assert!(none.is_empty());

        let steps = PreSteps {
            qr: true,
            ..PreSteps::default()
        };
        let (doc, svgs) = load(md, steps, Vec::new(), Vec::new()).await;
        assert!(doc.pages[0].body.contains("![](qr/qr-0.svg)"));
        assert_eq!(svgs[0].0, "qr/qr-0.svg");
    }
}
