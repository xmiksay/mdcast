//! End-to-end coverage for issue #54: an image reference the typst path
//! can't resolve (missing provider key, or — with the `remote-images`
//! feature off — a remote URL) must warn and disappear from the rendered
//! PDF, never leak `[image unresolved: ...]` prose into the artifact.

use std::sync::Arc;

use mdcast::backends::Registry;
use mdcast::{
    BrandHandle, BrandSpec, DocMeta, EmbeddedAssets, Page, PageOrigin, RenderRequest, ResolvedDoc,
    Target,
};

mod common;
use common::LogBuf;

fn doc_with_image(url: &str) -> ResolvedDoc {
    ResolvedDoc {
        pages: vec![Page {
            class: "content".into(),
            body: format!("![alt]({url})"),
            origin: PageOrigin::Explicit,
        }],
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
        fonts: Vec::new(),
        toc: None,
    }
}

#[tokio::test]
async fn unresolved_image_warns_and_renders_without_placeholder_text() {
    let doc = doc_with_image("https://img.invalid/missing.png");
    let assets = EmbeddedAssets;

    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let bytes = {
        let _guard = tracing::subscriber::set_default(subscriber);
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.pdf");
        let req = RenderRequest {
            doc: &doc,
            assets: &assets,
            out: &out,
        };
        Registry::with_defaults()
            .render(Target::Pdf, &req)
            .await
            .unwrap_or_else(|e| panic!("render failed: {e:#}"));
        std::fs::read(&out).unwrap()
    };

    assert!(
        bytes.starts_with(b"%PDF-"),
        "an unresolved image must not fail the render"
    );
    assert!(
        buf.contents().contains("image unresolved"),
        "expected a warning for the unresolved image, got:\n{}",
        buf.contents()
    );
}
