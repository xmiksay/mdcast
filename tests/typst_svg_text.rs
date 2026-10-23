//! End-to-end coverage for issue #92: SVG `<text>` is outlined to paths
//! before typst sees it, because typst's own SVG-text pipeline can deadlock
//! on mixed font fallbacks (the production trigger was a mermaid label with
//! `↔`). The deadlock is non-deterministic, so the markdown render repeats a
//! few times under a timeout — a hang fails the test instead of wedging it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use mdcast::backends::Registry;
use mdcast::backends::typst::{CliCompiler, TemplateDoc, TypstCompiler, render_template};
use mdcast::{
    AssetProvider, AssetRef, BoxFuture, BrandHandle, BrandSpec, DocMeta, EmbeddedAssets,
    LayeredAssets, Page, PageOrigin, ResolvedDoc, Target, sync_provider,
};

mod common;
use common::LogBuf;

const MERMAID_LIKE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="80" viewBox="0 0 320 80">
<rect x="5" y="5" width="310" height="70" rx="5" fill="#ECECFF" stroke="#9370DB"/>
<text x="160" y="45" text-anchor="middle" font-family="trebuchet ms, verdana, arial, sans-serif" font-size="16">axum handlery: HTTP ↔ doména</text>
</svg>"##;

const TIMEOUT: Duration = Duration::from_secs(60);

fn page(class: &str, body: &str) -> Page {
    Page {
        class: class.into(),
        body: body.into(),
        origin: PageOrigin::Explicit,
    }
}

/// Several pages around the diagram: the reported hang needed mdcast's
/// generated driver (layouts + `eval`) around the SVG, not a bare `#image`.
/// `broken` adds an unparseable SVG — only for the log-capturing test: tracing
/// caches per-callsite interest process-wide, so another test emitting the
/// same warnings concurrently without a subscriber can make them vanish from
/// the capture.
fn doc(broken: bool) -> ResolvedDoc {
    let mut pages = vec![
        page("hero", "# Architecture"),
        page(
            "content",
            "## Flow\n\n![diagram](diagrams/flow.svg)\n\nText ↔ more.",
        ),
        page("content", "## Again\n\n![diagram](diagrams/flow.svg)"),
        page("thanks", "# Thanks"),
    ];
    if broken {
        pages.push(page(
            "content",
            "## Broken\n\n![broken](diagrams/broken.svg)",
        ));
    }
    ResolvedDoc {
        pages,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: vec![AssetRef {
            key: "diagrams/flow.svg".into(),
        }],
        fonts: Vec::new(),
        toc: None,
    }
}

fn assets() -> impl AssetProvider {
    let over = sync_provider(|key: &str| {
        Ok(match key {
            "diagrams/flow.svg" => Some(Bytes::from_static(MERMAID_LIKE.as_bytes())),
            "diagrams/broken.svg" => Some(Bytes::from_static(b"<svg><g></svg>")),
            _ => None,
        })
    });
    LayeredAssets {
        over,
        base: EmbeddedAssets,
    }
}

async fn render_pdf(registry: &Registry, broken: bool) -> Vec<u8> {
    let (doc, assets) = (doc(broken), assets());
    let artifact = tokio::time::timeout(
        TIMEOUT,
        registry.render_to_bytes(Target::Pdf, &doc, &assets),
    )
    .await
    .expect("typst compile hung — SVG text reached typst")
    .unwrap_or_else(|e| panic!("render failed: {e:#}"));
    artifact.primary.to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn markdown_render_with_text_svg_succeeds_repeatedly() {
    let registry = Registry::with_defaults();
    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    for _ in 0..5 {
        assert!(render_pdf(&registry, true).await.starts_with(b"%PDF-"));
    }
    // The unparseable SVG degrades like an unresolved image rather than
    // failing the compile on a missing file.
    let logs = buf.contents();
    assert!(logs.contains("SVG could not be parsed"), "{logs}");
    assert!(logs.contains("diagrams__broken.svg"), "{logs}");
    assert!(logs.contains("image unresolved"), "{logs}");
}

fn typst_available() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn cli_compiler_also_gets_outlined_svgs() {
    if !typst_available() {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    }
    let cli = CliCompiler::new("typst", TIMEOUT);
    let registry = Registry::with_typst_compiler(TypstCompiler::Cli(cli));
    for _ in 0..3 {
        assert!(render_pdf(&registry, false).await.starts_with(b"%PDF-"));
    }
}

/// Real prefix-scanning `list`, so sibling discovery finds the SVG.
struct MapAssets(BTreeMap<&'static str, &'static [u8]>);

impl AssetProvider for MapAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Bytes>>> {
        let v = self.0.get(key).map(|b| Bytes::from_static(b));
        Box::pin(async move { Ok(v) })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<String>>> {
        let out = self
            .0
            .keys()
            .filter(|k| k.starts_with(prefix))
            .map(|k| k.to_string())
            .collect();
        Box::pin(async move { Ok(out) })
    }
}

#[tokio::test]
async fn template_render_with_text_svg_sibling_succeeds() {
    const MAIN: &str =
        "= Report\n#image(\"flow.svg\", width: 8cm)\n#image(\"flow.svg\", width: 4cm)\n";
    let assets = MapAssets(BTreeMap::from([
        ("templates/report.typ", MAIN.as_bytes()),
        ("templates/flow.svg", MERMAID_LIKE.as_bytes()),
    ]));
    let doc = TemplateDoc {
        template: "templates/report.typ".into(),
        data: serde_json::json!({}),
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        fonts: Vec::new(),
    };

    let artifact = tokio::time::timeout(TIMEOUT, render_template(&doc, &assets))
        .await
        .expect("typst compile hung — SVG text reached typst")
        .unwrap_or_else(|e| panic!("render failed: {e:#}"));

    assert!(artifact.primary.starts_with(b"%PDF-"));
}
