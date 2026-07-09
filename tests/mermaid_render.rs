//! End-to-end test for the mermaid pre-step (`mermaid` feature): a
//! ```mermaid fence renders to SVG, flows through the provider/images
//! pipeline, and the in-process typst engine embeds it in a real PDF.

#![cfg(all(feature = "mermaid", feature = "typst"))]

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use mdcast::backends::Registry;
use mdcast::mermaid::render_diagrams;
use mdcast::pages::auto::classify;
use mdcast::{
    AutoLayout, BrandHandle, BrandSpec, DefaultSplitter, DocMeta, EmbeddedAssets, LayeredAssets,
    PageSplitter, ResolvedDoc, Target, sync_provider,
};

const MD: &str = "# Report\n\nBefore the diagram.\n\n```mermaid\npie\n\"A\" : 1\n\"B\" : 2\n```\n\nAfter the diagram.\n";

#[tokio::test]
async fn mermaid_fence_renders_into_a_pdf() {
    let rendered = render_diagrams(MD);
    assert_eq!(rendered.svgs.len(), 1);

    let raw = DefaultSplitter.split(&rendered.markdown);
    let pages = classify(raw, &AutoLayout::default());
    let doc = ResolvedDoc {
        pages,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
        fonts: Vec::new(),
        toc: None,
    };

    let svgs: HashMap<String, Bytes> = rendered.svgs.into_iter().collect();
    let provider = LayeredAssets {
        over: sync_provider(move |k| Ok(svgs.get(k).cloned())),
        base: EmbeddedAssets,
    };

    let artifact = Registry::with_defaults()
        .render_to_bytes(Target::Pdf, &doc, &provider)
        .await
        .unwrap();

    assert!(artifact.primary.starts_with(b"%PDF-"), "expected a PDF");
}

#[test]
fn diagram_only_page_classifies_image_full() {
    let md = "# Deck\n\n---\n\n```mermaid\npie\n\"A\" : 1\n```\n";
    let rendered = render_diagrams(md);
    let raw = DefaultSplitter.split(&rendered.markdown);
    let pages = classify(raw, &AutoLayout::default());
    // Second page holds only the rewritten image ref, so the content-shape
    // rule (SingleImageOnly) kicks in.
    assert_eq!(pages[1].class, "image-full");
}
