//! Bytes-first render API (issue #3): a server embedder should be able to
//! render straight into memory, with no temp dir of its own to mint or clean
//! up. Exercises both backend families — pandoc (subprocess boundary) and
//! typst (in-process) — and checks the path-based API still agrees with it.

use std::sync::Arc;

use mdcast::backends::Registry;
use mdcast::pages::{Page, PageOrigin};
use mdcast::{BrandHandle, BrandSpec, DocMeta, EmbeddedAssets, RenderRequest, ResolvedDoc, Target};

fn doc() -> ResolvedDoc {
    ResolvedDoc {
        pages: vec![Page {
            class: "content".into(),
            body: "# Hello\n\nWorld".into(),
            origin: PageOrigin::AutoDefault,
        }],
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
    }
}

#[tokio::test]
async fn pandoc_target_renders_to_bytes() {
    let registry = Registry::with_defaults();
    let artifact = registry
        .render_to_bytes(Target::HtmlReveal, &doc(), &EmbeddedAssets)
        .await
        .expect("render html-reveal to bytes");

    assert_eq!(artifact.filename, "output.html");
    assert!(artifact.extras.is_empty());
    let html = String::from_utf8(artifact.primary.to_vec()).unwrap();
    assert!(html.contains("<html"), "expected standalone HTML, got: {html}");
}

#[tokio::test]
async fn typst_target_renders_to_bytes() {
    let registry = Registry::with_defaults();
    let artifact = registry
        .render_to_bytes(Target::Pdf, &doc(), &EmbeddedAssets)
        .await
        .expect("render pdf to bytes");

    assert_eq!(artifact.filename, "output.pdf");
    assert!(artifact.extras.is_empty());
    assert!(artifact.primary.starts_with(b"%PDF"), "not a PDF: {:?}", &artifact.primary[..8.min(artifact.primary.len())]);
}

#[tokio::test]
async fn path_based_render_agrees_with_bytes_render() {
    let registry = Registry::with_defaults();
    let d = doc();

    let bytes_artifact = registry
        .render_to_bytes(Target::HtmlReveal, &d, &EmbeddedAssets)
        .await
        .unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let out = tmp.path().join("out.html");
    let req = RenderRequest { doc: &d, assets: &EmbeddedAssets, out: &out };
    let path_artifact = registry.render(Target::HtmlReveal, &req).await.unwrap();

    let written = tokio::fs::read(&path_artifact.primary).await.unwrap();
    assert_eq!(written, bytes_artifact.primary.to_vec());
    assert!(path_artifact.extras.is_empty());
}
