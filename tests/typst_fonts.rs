//! End-to-end coverage for issue #50: a consumer-supplied font
//! (`ResolvedDoc.fonts`, resolved via `AssetProvider`) must let a typst
//! layout's `#set text(font: "...")` resolve to it with no host install.
//! Kept in its own file rather than added to `render_smoke.rs`, which is
//! already past the project's 400-line file cap.
//!
//! Drives the real typst engine against a hand-built, minimal TrueType font
//! (just the `head`/`hhea`/`maxp`/`name` tables — the mandatory set plus a
//! `FAMILY` name record) instead of a binary fixture: no glyph outlines are
//! needed because typst's "unknown font family" check runs at `#set
//! text(font: ...)` evaluation time, before anything is shaped.

use std::sync::Arc;

use bytes::Bytes;
use mdcast::backends::Registry;
use mdcast::{
    AssetProvider, AssetRef, BrandHandle, BrandSpec, DocMeta, EmbeddedAssets, LayeredAssets, Page,
    PageOrigin, RenderRequest, ResolvedDoc, Target, sync_provider,
};

mod common;
use common::LogBuf;

/// All-lowercase on purpose: typst lowercases both a requested `font:` family
/// and every registered `FontBook` family before comparing, so keeping this
/// pre-lowercased sidesteps any case-folding edge cases in the assertions.
const FONT_FAMILY: &str = "mdcastbrandfonttest";
const FONT_KEY: &str = "fonts/mdcast-brand-font.ttf";

const FONT_LAYOUT: &str = r#"
#let layout(body) = [
  #set page(margin: 2cm)
  #set text(font: "mdcastbrandfonttest")
  #eval(body, mode: "markup")
]
"#;

/// Builds a minimal, valid TrueType font in memory with a single `name`-table
/// FAMILY (id 1) record. Covers exactly what `ttf_parser::Face::parse`
/// requires (`head`, `hhea`, `maxp`) plus what typst's `FontInfo::from_ttf`
/// needs to register a family (a `FAMILY` name) — no `cmap`/`glyf`/`hmtx`,
/// since nothing here is ever shaped.
fn minimal_ttf(family: &str) -> Vec<u8> {
    // `head`: fixed 54-byte layout (mandatory table); only unitsPerEm matters.
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // version
    head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    // indexToLocFormat (offset 50) left at 0 ("short") — unused, no glyf/loca.

    // `hhea`: fixed 36-byte layout (mandatory table); zeroed metrics are fine.
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // version

    // `maxp`: version 0.5 (CFF-style, no `glyf`), 6 bytes (mandatory table).
    let mut maxp = Vec::with_capacity(6);
    maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp.extend_from_slice(&1u16.to_be_bytes()); // numGlyphs (must be non-zero)

    // `name`: one FAMILY (id 1) record, Windows/Unicode-BMP/en-US, UTF-16BE —
    // the encoding `ttf_parser::Name::to_string` decodes.
    let utf16: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut name = Vec::new();
    name.extend_from_slice(&0u16.to_be_bytes()); // format 0
    name.extend_from_slice(&1u16.to_be_bytes()); // count
    name.extend_from_slice(&18u16.to_be_bytes()); // storageOffset (6 + 1*12)
    name.extend_from_slice(&3u16.to_be_bytes()); // platformID: Windows
    name.extend_from_slice(&1u16.to_be_bytes()); // encodingID: Unicode BMP
    name.extend_from_slice(&0x0409u16.to_be_bytes()); // languageID: en-US
    name.extend_from_slice(&1u16.to_be_bytes()); // nameID: FAMILY
    name.extend_from_slice(&(utf16.len() as u16).to_be_bytes()); // length
    name.extend_from_slice(&0u16.to_be_bytes()); // offset into storage
    name.extend_from_slice(&utf16);

    // Table directory requires ascending tag order for spec-conformant readers.
    let tables: [(&[u8; 4], &[u8]); 4] = [
        (b"head", &head),
        (b"hhea", &hhea),
        (b"maxp", &maxp),
        (b"name", &name),
    ];

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfntVersion: TrueType
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes()); // numTables
    out.extend_from_slice(&[0; 6]); // searchRange/entrySelector/rangeShift (unchecked)

    let mut offset = (12 + tables.len() * 16) as u32;
    for (tag, data) in &tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checkSum (unchecked by ttf_parser)
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
    }
    out
}

fn font_layout_doc(declare_font: bool) -> ResolvedDoc {
    ResolvedDoc {
        pages: vec![Page {
            class: "content".into(),
            body: "Body text.".into(),
            origin: PageOrigin::Explicit,
        }],
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
        fonts: if declare_font {
            vec![AssetRef {
                key: FONT_KEY.to_string(),
            }]
        } else {
            Vec::new()
        },
        toc: None,
    }
}

/// Layers a `content` layout that sets `font: "mdcastbrandfonttest"` over the
/// built-in catalog, plus (optionally) the font bytes themselves at
/// `FONT_KEY`.
fn assets_with_font_layout(font_bytes: Option<Vec<u8>>) -> impl AssetProvider {
    let over = sync_provider(move |key: &str| match key {
        "typst/layouts/pdf/content.typ" => Ok(Some(Bytes::from_static(FONT_LAYOUT.as_bytes()))),
        k if k == FONT_KEY => Ok(font_bytes.clone().map(Bytes::from)),
        _ => Ok(None),
    });
    LayeredAssets {
        over,
        base: EmbeddedAssets,
    }
}

async fn render_capturing_logs(doc: &ResolvedDoc, assets: impl AssetProvider) -> (Vec<u8>, String) {
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
            doc,
            assets: &assets,
            out: &out,
        };
        Registry::with_defaults()
            .render(Target::Pdf, &req)
            .await
            .unwrap_or_else(|e| panic!("render failed: {e:#}"));
        std::fs::read(&out).unwrap()
    };
    (bytes, buf.contents())
}

/// Baseline: with no brand font declared and none installed on the host, the
/// requested family doesn't resolve — typst warns but still produces a PDF
/// (falls back to its default). Establishes what "no host install" looks
/// like *without* this feature, so the next test's silence is meaningful.
#[tokio::test]
async fn typst_undeclared_font_family_warns_and_still_renders() {
    let doc = font_layout_doc(false);
    let assets = assets_with_font_layout(None);

    let (bytes, logs) = render_capturing_logs(&doc, assets).await;

    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    assert!(
        logs.contains("unknown font family") && logs.contains(FONT_FAMILY),
        "expected an unknown-font-family warning in logs, got:\n{logs}"
    );
}

/// The acceptance criterion: a font declared via `ResolvedDoc.fonts` and
/// resolved through the `AssetProvider` lets `#set text(font: "...")` find it
/// with no host install — proven by the absence of the warning the baseline
/// test above establishes fires whenever the family can't be found.
#[tokio::test]
async fn typst_layout_resolves_declared_brand_font_with_no_host_install() {
    let doc = font_layout_doc(true);
    let assets = assets_with_font_layout(Some(minimal_ttf(FONT_FAMILY)));

    let (bytes, logs) = render_capturing_logs(&doc, assets).await;

    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    assert!(
        !logs.contains("unknown font family"),
        "brand font should have resolved with no warning, got:\n{logs}"
    );
}

/// A declared font key that resolves to non-font bytes must degrade to the
/// same "not found" behaviour as an unresolved family, not crash the render —
/// provider-supplied bytes are external input, same as any other asset.
#[tokio::test]
async fn typst_malformed_font_bytes_do_not_crash_render() {
    let doc = font_layout_doc(true);
    let assets = assets_with_font_layout(Some(b"not a font".to_vec()));

    let (bytes, logs) = render_capturing_logs(&doc, assets).await;

    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    assert!(
        logs.contains("unknown font family"),
        "malformed font bytes should be skipped, leaving the family unresolved:\n{logs}"
    );
}
