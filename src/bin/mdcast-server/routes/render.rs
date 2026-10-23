//! `POST /v1/render` — the markdown pipeline over HTTP.
//!
//! Every step here has a `mdcast render` flag behind it (see
//! `bin/mdcast/main.rs`): frontmatter extraction, the `HtmlImageTags`
//! preprocessor, the mermaid and QR pre-steps, page splitting and classification,
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

    // Blobs over the built-in catalog — also what a `<qr path="…">` tag reads
    // through, so it resolves the same keys the render itself will.
    let base = over_embedded(blobs);
    let (doc, generated) = resolve_doc(&markdown, meta, brand, &options, &base).await;
    let provider = provider_stack(base, generated);

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
/// SVGs the mermaid/QR pre-steps produced, which the caller has to layer over
/// the provider stack for the render to resolve them.
async fn resolve_doc(
    markdown: &str,
    meta: DocMeta,
    brand: Option<BrandSpec>,
    options: &RenderOptions,
    assets: &dyn AssetProvider,
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

    let (markdown, mut generated) = if options.mermaid {
        let rendered = mdcast::mermaid::render_diagrams(&markdown);
        (rendered.markdown, rendered.svgs)
    } else {
        (markdown, Vec::new())
    };
    let markdown = if options.qr {
        let rendered = mdcast::generators::qr::render_qr_codes(&markdown, assets).await;
        generated.extend(rendered.svgs);
        rendered.markdown
    } else {
        markdown
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
    (doc, generated)
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

fn over_embedded(blobs: BlobAssets) -> Box<dyn AssetProvider> {
    Box::new(LayeredAssets {
        over: blobs,
        base: EmbeddedAssets,
    })
}

/// `provider` is the request's blobs over the built-in catalog; the generated
/// (mermaid/QR) SVGs go over both. Layering blobs *over* `EmbeddedAssets` is
/// what lets a request override `reference/reference.docx` or a typst layout
/// while everything it does not mention still resolves to the built-in.
fn provider_stack(
    provider: Box<dyn AssetProvider>,
    generated: Vec<(String, Bytes)>,
) -> Box<dyn AssetProvider> {
    if generated.is_empty() {
        return provider;
    }
    let svgs: HashMap<String, Bytes> = generated.into_iter().collect();
    Box::new(LayeredAssets {
        over: sync_provider(move |key| Ok(svgs.get(key).cloned())),
        base: provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn resolve(
        markdown: &str,
        options: RenderOptions,
    ) -> (ResolvedDoc, Vec<(String, Bytes)>) {
        resolve_doc(
            markdown,
            DocMeta::default(),
            None,
            &options,
            &EmbeddedAssets,
        )
        .await
    }

    async fn doc(markdown: &str, options: RenderOptions) -> ResolvedDoc {
        resolve(markdown, options).await.0
    }

    #[tokio::test]
    async fn frontmatter_wins_over_the_requests_meta() {
        let meta = DocMeta {
            title: Some("from the request".into()),
            author: Some("kept".into()),
            ..DocMeta::default()
        };
        let markdown = "---\ntitle: from the document\n---\n\n# Body";

        let (doc, _) = resolve_doc(
            markdown,
            meta,
            None,
            &RenderOptions::default(),
            &EmbeddedAssets,
        )
        .await;

        assert_eq!(doc.meta.title.as_deref(), Some("from the document"));
        assert_eq!(
            doc.meta.author.as_deref(),
            Some("kept"),
            "a field the frontmatter does not mention must survive"
        );
    }

    #[tokio::test]
    async fn options_land_on_the_resolved_doc() {
        let options = RenderOptions {
            toc_depth: Some(3),
            layout_assets: vec!["branding/logo.svg".into()],
            layout_fonts: vec!["fonts/Brand-Regular.ttf".into()],
            ..RenderOptions::default()
        };

        let doc = doc("# Hi", options).await;

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

    #[tokio::test]
    async fn html_image_tags_is_off_unless_asked_for() {
        let markdown = "<img src=\"img/a.png\" alt=\"A\">";

        assert!(
            doc(markdown, RenderOptions::default()).await.pages[0]
                .body
                .contains("<img")
        );
        let rewritten = doc(
            markdown,
            RenderOptions {
                html_image_tags: true,
                ..RenderOptions::default()
            },
        )
        .await;
        assert!(rewritten.pages[0].body.contains("![A](img/a.png)"));
    }

    #[tokio::test]
    async fn mermaid_fences_become_image_refs_with_their_svgs_alongside() {
        let markdown = "```mermaid\npie\n\"A\" : 1\n```\n";

        let (plain, none) = resolve(markdown, RenderOptions::default()).await;
        assert!(plain.pages[0].body.contains("```mermaid"));
        assert!(none.is_empty());

        let options = RenderOptions {
            mermaid: true,
            ..RenderOptions::default()
        };
        let (rendered, diagrams) = resolve(markdown, options).await;
        assert!(
            rendered.pages[0]
                .body
                .contains("![](mermaid/diagram-0.svg)")
        );
        assert_eq!(diagrams.len(), 1);
        assert_eq!(diagrams[0].0, "mermaid/diagram-0.svg");
    }

    #[tokio::test]
    async fn qr_blocks_render_only_when_asked_and_read_paths_through_the_provider() {
        let markdown = "```qr\nSPD*1.0\n```\n\n<qr path=\"pay.txt\">\n";
        let assets = mdcast::sync_provider(|key| {
            Ok((key == "pay.txt").then(|| Bytes::from_static(b"SPD*1.0*AM:1")))
        });

        let (plain, none) = resolve(markdown, RenderOptions::default()).await;
        assert!(plain.pages[0].body.contains("```qr"));
        assert!(none.is_empty());

        let options = RenderOptions {
            qr: true,
            ..RenderOptions::default()
        };
        let (rendered, svgs) =
            resolve_doc(markdown, DocMeta::default(), None, &options, &assets).await;
        let body = &rendered.pages[0].body;
        assert!(body.contains("![](qr/qr-0.svg)"), "{body}");
        assert!(body.contains("![](qr/qr-1.svg)"), "{body}");
        let keys: Vec<_> = svgs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["qr/qr-0.svg", "qr/qr-1.svg"]);
    }

    #[tokio::test]
    async fn the_provider_stack_prefers_diagrams_then_blobs_then_embedded() {
        let store = Arc::new(crate::blobs::BlobStore::new(1 << 20, 3, 32).unwrap());
        let digest = crate::blobs::store::sha256(b"OVERRIDDEN");
        store.put(&digest, b"OVERRIDDEN").unwrap();
        let manifest = mdcast::mdcast_api::AssetManifest::from_iter([(
            "reference/reference.docx".to_string(),
            digest,
        )]);
        let blobs = BlobAssets::resolve(store, manifest).await.ok().unwrap();

        let provider = provider_stack(
            over_embedded(blobs),
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
