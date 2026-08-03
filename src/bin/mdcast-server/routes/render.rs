//! `POST /v1/render` — the markdown pipeline over HTTP.
//!
//! Every step here has a `mdcast render` flag behind it (see
//! `bin/mdcast/main.rs`): frontmatter extraction, the `HtmlImageTags`
//! preprocessor, the mermaid pre-step, page splitting and classification,
//! layout assets/fonts, TOC depth. Nothing the CLI cannot do is reachable
//! through the wire protocol, and nothing it does is skipped.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::response::Response;
use bytes::Bytes;
use mdcast::mdcast_api::wire::{RenderMarkdownRequest, RenderOptions};
use mdcast::mdcast_api::{BrandSpec, DocMeta};
use mdcast::pages::auto::classify;
use mdcast::{
    AssetProvider, AssetRef, BrandHandle, DefaultSplitter, EmbeddedAssets, HtmlImageTags, Identity,
    LayeredAssets, MarkdownPreprocessor, PageSplitter, ResolvedDoc, sync_provider,
};

use crate::blobs::BlobAssets;
use crate::error::{ApiError, json_body};

use super::{AppState, artifact_response, content_type};

pub async fn markdown(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RenderMarkdownRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let RenderMarkdownRequest {
        markdown,
        meta,
        brand,
        target,
        assets,
        options,
    } = json_body(body)?;

    if let Some(depth) = options.toc_depth
        && !(1..=6).contains(&depth)
    {
        return Err(ApiError::BadRequest(format!(
            "toc_depth must be between 1 and 6, got {depth}"
        )));
    }

    // The manifest is checked in full before any work happens, so a caller
    // missing three blobs learns about all three from one response.
    let blobs = BlobAssets::resolve(state.store.clone(), assets)
        .await
        .map_err(ApiError::MissingBlobs)?;

    let (doc, diagrams) = resolve_doc(&markdown, meta, brand, &options);
    let provider = provider_stack(blobs, diagrams);

    tracing::info!(
        target = target.as_str(),
        pages = doc.pages.len(),
        "rendering"
    );
    let artifact = state
        .registry
        .render_to_bytes(target, &doc, &provider)
        .await
        .map_err(ApiError::RenderFailed)?;

    Ok(artifact_response(content_type(target), artifact))
}

/// The CLI's `load_doc`, minus the filesystem. Returns the document plus any
/// mermaid SVGs the pre-step produced, which the caller has to layer over the
/// provider stack for the render to resolve them.
fn resolve_doc(
    markdown: &str,
    meta: DocMeta,
    brand: Option<BrandSpec>,
    options: &RenderOptions,
) -> (ResolvedDoc, Vec<(String, Bytes)>) {
    // Frontmatter in the submitted body wins over the request's `meta` field:
    // it travels with the document, so it is the more specific statement.
    let (front_matter, markdown) = mdcast::frontmatter::extract(markdown);
    let meta = merge_meta(meta, front_matter);
    let brand = brand.unwrap_or_default();

    let preprocessor: Box<dyn MarkdownPreprocessor> = if options.html_image_tags {
        Box::new(HtmlImageTags)
    } else {
        Box::new(Identity)
    };
    let markdown = preprocessor.preprocess(&markdown);

    let (markdown, diagrams) = if options.mermaid {
        let rendered = mdcast::mermaid::render_diagrams(&markdown);
        (rendered.markdown, rendered.svgs)
    } else {
        (markdown, Vec::new())
    };

    let pages = classify(DefaultSplitter.split(&markdown), &brand.auto_layout);
    let doc = ResolvedDoc {
        pages,
        meta,
        brand: BrandHandle(Arc::new(brand)),
        assets: as_refs(&options.layout_assets),
        fonts: as_refs(&options.layout_fonts),
        toc: options.toc_depth,
    };
    (doc, diagrams)
}

fn as_refs(keys: &[String]) -> Vec<AssetRef> {
    keys.iter()
        .map(|key| AssetRef { key: key.clone() })
        .collect()
}

fn merge_meta(base: DocMeta, front_matter: DocMeta) -> DocMeta {
    let mut extra = base.extra;
    extra.extend(front_matter.extra);
    DocMeta {
        title: front_matter.title.or(base.title),
        author: front_matter.author.or(base.author),
        date: front_matter.date.or(base.date),
        extra,
    }
}

/// Innermost first: the built-in catalog, the request's blobs over it, and the
/// mermaid SVGs over both. Layering blobs *over* `EmbeddedAssets` is what lets
/// a request override `reference/reference.docx` or a typst layout while
/// everything it does not mention still resolves to the built-in.
fn provider_stack(blobs: BlobAssets, diagrams: Vec<(String, Bytes)>) -> Box<dyn AssetProvider> {
    let provider: Box<dyn AssetProvider> = Box::new(LayeredAssets {
        over: blobs,
        base: EmbeddedAssets,
    });
    if diagrams.is_empty() {
        return provider;
    }
    let svgs: HashMap<String, Bytes> = diagrams.into_iter().collect();
    Box::new(LayeredAssets {
        over: sync_provider(move |key| Ok(svgs.get(key).cloned())),
        base: provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(markdown: &str, options: RenderOptions) -> ResolvedDoc {
        resolve_doc(markdown, DocMeta::default(), None, &options).0
    }

    #[test]
    fn frontmatter_wins_over_the_requests_meta() {
        let meta = DocMeta {
            title: Some("from the request".into()),
            author: Some("kept".into()),
            ..DocMeta::default()
        };
        let markdown = "---\ntitle: from the document\n---\n\n# Body";

        let (doc, _) = resolve_doc(markdown, meta, None, &RenderOptions::default());

        assert_eq!(doc.meta.title.as_deref(), Some("from the document"));
        assert_eq!(
            doc.meta.author.as_deref(),
            Some("kept"),
            "a field the frontmatter does not mention must survive"
        );
    }

    #[test]
    fn options_land_on_the_resolved_doc() {
        let options = RenderOptions {
            toc_depth: Some(3),
            layout_assets: vec!["branding/logo.svg".into()],
            layout_fonts: vec!["fonts/Brand-Regular.ttf".into()],
            ..RenderOptions::default()
        };

        let doc = doc("# Hi", options);

        assert_eq!(doc.toc, Some(3));
        assert_eq!(
            doc.assets
                .iter()
                .map(|a| a.key.as_str())
                .collect::<Vec<_>>(),
            vec!["branding/logo.svg"]
        );
        assert_eq!(
            doc.fonts.iter().map(|a| a.key.as_str()).collect::<Vec<_>>(),
            vec!["fonts/Brand-Regular.ttf"]
        );
    }

    #[test]
    fn html_image_tags_is_off_unless_asked_for() {
        let markdown = "<img src=\"img/a.png\" alt=\"A\">";

        assert!(
            doc(markdown, RenderOptions::default()).pages[0]
                .body
                .contains("<img")
        );
        let rewritten = doc(
            markdown,
            RenderOptions {
                html_image_tags: true,
                ..RenderOptions::default()
            },
        );
        assert!(rewritten.pages[0].body.contains("![A](img/a.png)"));
    }

    #[test]
    fn mermaid_fences_become_image_refs_with_their_svgs_alongside() {
        let markdown = "```mermaid\npie\n\"A\" : 1\n```\n";

        let (plain, none) = resolve_doc(
            markdown,
            DocMeta::default(),
            None,
            &RenderOptions::default(),
        );
        assert!(plain.pages[0].body.contains("```mermaid"));
        assert!(none.is_empty());

        let (rendered, diagrams) = resolve_doc(
            markdown,
            DocMeta::default(),
            None,
            &RenderOptions {
                mermaid: true,
                ..RenderOptions::default()
            },
        );
        assert!(
            rendered.pages[0]
                .body
                .contains("![](mermaid/diagram-0.svg)")
        );
        assert_eq!(diagrams.len(), 1);
        assert_eq!(diagrams[0].0, "mermaid/diagram-0.svg");
    }

    #[tokio::test]
    async fn the_provider_stack_prefers_diagrams_then_blobs_then_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            crate::blobs::BlobStore::open(dir.path().to_path_buf(), 1 << 20)
                .await
                .unwrap(),
        );
        let digest = crate::blobs::store::sha256(b"OVERRIDDEN");
        store.put(&digest, b"OVERRIDDEN").await.unwrap();
        let manifest = mdcast::mdcast_api::AssetManifest::from_iter([(
            "reference/reference.docx".to_string(),
            digest,
        )]);
        let blobs = BlobAssets::resolve(store, manifest).await.ok().unwrap();

        let provider = provider_stack(
            blobs,
            vec![(
                "mermaid/diagram-0.svg".into(),
                Bytes::from_static(b"<svg/>"),
            )],
        );

        assert_eq!(
            provider
                .get("mermaid/diagram-0.svg")
                .await
                .unwrap()
                .unwrap(),
            &b"<svg/>"[..]
        );
        assert_eq!(
            provider
                .get("reference/reference.docx")
                .await
                .unwrap()
                .unwrap(),
            &b"OVERRIDDEN"[..],
            "a manifest entry must shadow the embedded catalog"
        );
        assert!(
            provider
                .get("typst/layouts/pdf/hero.typ")
                .await
                .unwrap()
                .is_some(),
            "anything the manifest does not name still resolves to the built-in catalog"
        );
    }
}
