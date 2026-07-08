#![cfg(feature = "typst-html")]
//! HTML export for `render_template` (issue #53) — the same template + data
//! `render_template`/`render_template_html` share (`template.rs`'s
//! `assemble`) compiled against the real in-process typst engine, not just
//! `template.rs`'s own unit tests. Kept in its own file, gated on the
//! off-by-default `typst-html` feature, rather than folded into
//! `typst_template.rs`, which stays exercisable without it.

use std::sync::Arc;

use bytes::Bytes;
use mdcast::backends::typst::{TemplateDoc, render_template, render_template_html};
use mdcast::{AssetProvider, BoxFuture, BrandHandle, BrandSpec, DocMeta};

mod common;
use common::LogBuf;

/// A tiny in-memory provider — no siblings to discover, so `list` is always
/// empty (unlike `typst_template.rs`'s `MapAssets`, which needs real prefix
/// scanning for its partial/logo).
struct SingleFile(&'static str, &'static str);

impl AssetProvider for SingleFile {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Bytes>>> {
        let v = (key == self.0).then(|| Bytes::from_static(self.1.as_bytes()));
        Box::pin(async move { Ok(v) })
    }

    fn list<'a>(&'a self, _prefix: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<String>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

/// Branches on `target()` — the "writing a dual-target template" pattern
/// documented in the README. The web branch renders a plain heading; the
/// print branch adds a `#place`d header only meaningful on a paged layout.
/// One template, two outputs, no duplication.
const DUAL_TARGET_TEMPLATE: &str = r#"
#let invoice = json("/data.json")

#context if target() == "html" [
  = Invoice #invoice.number (web)
] else [
  #place(top, text(size: 8pt)[Printed copy])
  = Invoice #invoice.number
]
"#;

fn doc() -> TemplateDoc {
    TemplateDoc {
        template: "templates/invoice.typ".to_string(),
        data: serde_json::json!({"number": "INV-042"}),
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
    }
}

/// Acceptance criterion: the same template + data renders to both a PDF and
/// an HTML page from one fixture, with `target()` actually branching the
/// body — the html-only heading text shows up in the HTML export and the
/// paged-only `#place`d header (never evaluated under the html branch) does
/// not.
#[tokio::test]
async fn same_template_renders_to_pdf_and_html() {
    let assets = SingleFile("templates/invoice.typ", DUAL_TARGET_TEMPLATE);

    let pdf = render_template(&doc(), &assets)
        .await
        .unwrap_or_else(|e| panic!("PDF render failed: {e:#}"));
    assert!(
        pdf.primary.starts_with(b"%PDF-"),
        "not a PDF: {:?}",
        &pdf.primary[..pdf.primary.len().min(16)]
    );

    let html = render_template_html(&doc(), &assets)
        .await
        .unwrap_or_else(|e| panic!("HTML render failed: {e:#}"));
    assert_eq!(html.filename, "output.html");
    let html_str = std::str::from_utf8(&html.primary).expect("html export should be valid UTF-8");
    assert!(
        html_str.contains("<html"),
        "not an html document: {html_str}"
    );
    assert!(
        html_str.contains("Invoice INV-042 (web)"),
        "expected the html-only branch's text in the export: {html_str}"
    );
    assert!(
        !html_str.contains("Printed copy"),
        "the paged-only #place header is behind the `else` arm of the \
         target() branch and must never be evaluated under the html target: \
         {html_str}"
    );
}

/// Acceptance criterion: a compiler warning from an HTML-unsupported
/// construct surfaces through `tracing::warn!` (the same path
/// `render_template`'s PDF export warnings already use) rather than being
/// silently swallowed. `#place` has no HTML-export rule, so typst warns
/// "place was ignored during HTML export" and still produces output.
#[tokio::test]
async fn html_unsupported_construct_warning_is_not_swallowed() {
    const TEMPLATE: &str = r#"
#place(top, [Ignored on the web])
= Hello
"#;
    let assets = SingleFile("templates/page.typ", TEMPLATE);
    let doc = TemplateDoc {
        template: "templates/page.typ".to_string(),
        data: serde_json::json!({}),
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
    };

    let log_buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log_buf.clone())
        .with_ansi(false)
        .finish();
    let html = {
        let _guard = tracing::subscriber::set_default(subscriber);
        render_template_html(&doc, &assets)
            .await
            .unwrap_or_else(|e| panic!("HTML render failed: {e:#}"))
    };
    assert!(
        std::str::from_utf8(&html.primary)
            .unwrap()
            .contains("<html"),
        "should still produce real HTML despite the unsupported construct"
    );

    let logs = log_buf.contents();
    assert!(
        logs.contains("was ignored during HTML export"),
        "expected typst's HTML-unsupported-construct warning to surface via \
         tracing, got:\n{logs}"
    );
}
