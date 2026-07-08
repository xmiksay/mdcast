//! End-to-end coverage for issue #52: a user-supplied typst template + a
//! `serde_json::Value` → PDF, with no markdown involved. Kept in its own
//! file rather than added to `render_smoke.rs`, which is already past the
//! project's 400-line file cap (see `typst_fonts.rs` for the same reasoning).
//!
//! Drives the real in-process typst compiler against an invoice-shaped
//! template: line items, a total, a partial the main template `#import`s,
//! and a logo it `#image`s — proving sibling discovery (`AssetProvider::list`
//! scoped to the template's own directory), `/data.json`, and `/context.typ`
//! all compose for real, not just in the library's own unit tests.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use mdcast::backends::typst::{TemplateDoc, render_template};
use mdcast::{AssetProvider, BoxFuture, BrandHandle, BrandSpec, DocMeta};

const INVOICE_TEMPLATE: &str = r#"
#import "/context.typ": doc-meta, brand-color
#import "partials/header.typ": letterhead
#let invoice = json("/data.json")

#letterhead
#image("logo.svg", width: 2cm)

= Invoice #invoice.number
#doc-meta.title

#table(
  columns: (1fr, auto, auto),
  [*Description*], [*Qty*], [*Total*],
  ..invoice.items.map(it => (
    [#it.description], [#str(it.qty)], [#str(it.total)],
  )).flatten(),
)

#text(fill: brand-color("accent", default: black))[Total due: #invoice.total]
"#;

const HEADER_PARTIAL: &str = r#"#let letterhead = [#text(weight: "bold")[Acme Corp]]"#;

const LOGO_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;

/// A tiny in-memory provider with a real prefix-scanning `list` (unlike
/// `sync_provider`, whose `list` always returns empty) — needed so sibling
/// discovery under `templates/` actually finds the partial and the logo.
struct MapAssets(BTreeMap<&'static str, &'static [u8]>);

impl AssetProvider for MapAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Bytes>>> {
        let v = self.0.get(key).map(|b| Bytes::from_static(b));
        Box::pin(async move { Ok(v) })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<String>>> {
        let out: Vec<String> = self
            .0
            .keys()
            .filter(|k| k.starts_with(prefix))
            .map(|k| k.to_string())
            .collect();
        Box::pin(async move { Ok(out) })
    }
}

fn invoice_assets() -> MapAssets {
    MapAssets(BTreeMap::from([
        ("templates/invoice.typ", INVOICE_TEMPLATE.as_bytes()),
        ("templates/partials/header.typ", HEADER_PARTIAL.as_bytes()),
        ("templates/logo.svg", LOGO_SVG),
    ]))
}

fn invoice_data() -> serde_json::Value {
    serde_json::json!({
        "number": "INV-2026-0042",
        "total": "129.99",
        "items": [
            {"description": "Consulting", "qty": 3, "total": "99.99"},
            {"description": "Support", "qty": 1, "total": "30.00"},
        ],
    })
}

#[tokio::test]
async fn renders_invoice_shaped_pdf_from_template_and_data() {
    let brand = BrandSpec {
        name: "Acme".into(),
        palette: BTreeMap::from([("accent".to_string(), "#2E5AAC".to_string())]),
        ..Default::default()
    };
    let doc = TemplateDoc {
        template: "templates/invoice.typ".to_string(),
        data: invoice_data(),
        meta: DocMeta {
            title: Some("Q3 Services Invoice".to_string()),
            ..Default::default()
        },
        brand: BrandHandle(Arc::new(brand)),
    };

    let artifact = render_template(&doc, &invoice_assets())
        .await
        .unwrap_or_else(|e| panic!("render_template failed: {e:#}"));

    assert!(
        artifact.primary.starts_with(b"%PDF-"),
        "not a PDF: {:?}",
        &artifact.primary[..artifact.primary.len().min(16)]
    );
    assert!(
        artifact.primary.len() > 500,
        "suspiciously small PDF: {} bytes",
        artifact.primary.len()
    );
}

#[tokio::test]
async fn missing_template_key_names_it_in_the_error() {
    let doc = TemplateDoc {
        template: "templates/does-not-exist.typ".to_string(),
        data: serde_json::json!({}),
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
    };

    let err = render_template(&doc, &invoice_assets()).await.unwrap_err();

    assert!(
        err.to_string().contains("templates/does-not-exist.typ"),
        "expected the missing key in the error, got: {err:#}"
    );
}
