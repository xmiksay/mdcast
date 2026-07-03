//! End-to-end smoke tests: drive the *real* engines (in-process typst,
//! subprocess pandoc) to a genuine artifact per `Target`. The rest of the
//! suite is unit tests around page splitting / classification / provider
//! plumbing and never invokes an engine — an engine regression (pandoc CLI
//! change, typst crate bump, a broken embedded layout) would otherwise
//! surface first in a downstream consumer, not here.
//!
//! Fixture mirrors README.md's "A minimal markdown example" section — keep
//! the two in sync if that section changes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use mdcast::backends::Registry;
use mdcast::pages::auto::classify;
use mdcast::{
    AssetProvider, AutoLayout, BrandHandle, BrandSpec, DefaultSplitter, DocMeta, EmbeddedAssets,
    LayeredAssets, Page, PageOrigin, PageSplitter, RenderRequest, ResolvedDoc, Target,
    sync_provider,
};

const README_EXAMPLE: &str = include_str!("golden/readme-example.md");

fn resolved_doc(md: &str) -> ResolvedDoc {
    let raw = DefaultSplitter.split(md);
    let pages = classify(raw, &AutoLayout::default());
    ResolvedDoc {
        pages,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
    }
}

/// Layers a fake `charts/revenue.svg` (referenced by the README fixture) over
/// the built-in catalog, so the render exercises the image-resolution path
/// for real instead of leaving the reference dangling.
fn assets_with_chart() -> impl AssetProvider {
    let chart = sync_provider(|key: &str| {
        if key == "charts/revenue.svg" {
            Ok(Some(Bytes::from_static(
                br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#,
            )))
        } else {
            Ok(None)
        }
    });
    LayeredAssets {
        over: chart,
        base: EmbeddedAssets,
    }
}

fn pandoc_available() -> bool {
    std::process::Command::new("pandoc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ext_for(target: Target) -> &'static str {
    match target {
        Target::Docx => "docx",
        Target::Odt => "odt",
        Target::Pdf => "pdf",
        Target::PdfPresentation => "pdf",
        Target::Pptx => "pptx",
        Target::HtmlReveal => "html",
    }
}

async fn render(target: Target, doc: &ResolvedDoc) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join(format!("out.{}", ext_for(target)));
    let assets = assets_with_chart();
    let req = RenderRequest {
        doc,
        assets: &assets,
        out: &out,
    };
    let registry = Registry::with_defaults();
    registry
        .render(target, &req)
        .await
        .unwrap_or_else(|e| panic!("render {target:?} failed: {e:#}"));
    (tmp, out)
}

/// Read one entry out of a zip-based document (docx/odt/pptx are all zip
/// containers) as a UTF-8 string.
fn zip_entry_to_string(bytes: &[u8], entry: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .unwrap_or_else(|e| panic!("{entry} archive should be a valid zip: {e}"));
    let mut file = archive
        .by_name(entry)
        .unwrap_or_else(|e| panic!("{entry} missing from archive: {e}"));
    let mut s = String::new();
    std::io::Read::read_to_string(&mut file, &mut s).unwrap();
    s
}

fn has_external_resource_ref(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    [
        "src=\"http://",
        "src=\"https://",
        "href=\"http://",
        "href=\"https://",
        "url(http://",
        "url(https://",
        "url(\"http://",
        "url(\"https://",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[tokio::test]
async fn typst_pdf_smoke() {
    let doc = resolved_doc(README_EXAMPLE);
    let (_tmp, out) = render(Target::Pdf, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-"),
        "not a PDF: {:?}",
        &bytes[..bytes.len().min(16)]
    );
    assert!(
        bytes.len() > 500,
        "suspiciously small PDF: {} bytes",
        bytes.len()
    );
}

#[tokio::test]
async fn typst_pdf_presentation_smoke() {
    let doc = resolved_doc(README_EXAMPLE);
    let (_tmp, out) = render(Target::PdfPresentation, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes.starts_with(b"%PDF-"),
        "not a PDF: {:?}",
        &bytes[..bytes.len().min(16)]
    );
    assert!(
        bytes.len() > 500,
        "suspiciously small PDF: {} bytes",
        bytes.len()
    );
}

#[tokio::test]
async fn pandoc_docx_smoke() {
    if !pandoc_available() {
        eprintln!("skipping pandoc_docx_smoke: `pandoc` not on PATH");
        return;
    }
    let doc = resolved_doc(README_EXAMPLE);
    let (_tmp, out) = render(Target::Docx, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK"), "docx should be a zip container");
    assert!(
        bytes.len() > 200,
        "suspiciously small docx: {} bytes",
        bytes.len()
    );

    // README_EXAMPLE has 5 pages — the writer must have emitted 4 real page
    // breaks, not just a raw-LaTeX marker pandoc silently drops.
    let document_xml = zip_entry_to_string(&bytes, "word/document.xml");
    let break_count = document_xml.matches(r#"<w:br w:type="page"/>"#).count();
    assert_eq!(
        break_count, 4,
        "expected 4 page breaks in word/document.xml, found {break_count}:\n{document_xml}"
    );

    // Each page's class should resolve to a real paragraph style defined in
    // reference.docx, not silently fall back to pandoc's default styling.
    // README_EXAMPLE only exercises "hero", "content" (the image page falls
    // through auto-classification to the default class, since it carries a
    // heading alongside the image) and "thanks" this way — see
    // `docx_custom_styles_apply_to_plain_paragraphs` for the other three,
    // which pandoc's docx writer never applies to headings/blockquotes
    // regardless of the enclosing custom-style (documented in the manual's
    // "Custom Styles" section).
    for class in ["hero", "content", "thanks"] {
        let needle = format!(r#"<w:pStyle w:val="{class}" />"#);
        assert!(
            document_xml.contains(&needle),
            "expected {needle:?} in word/document.xml (reference.docx style \
             not applied for class {class:?}):\n{document_xml}"
        );
    }

    // README_EXAMPLE's blockquote page always renders with pandoc's built-in
    // BlockText style (pandoc never lets a custom-style override a
    // blockquote's own styling) — reference.docx brands BlockText itself so
    // that page still looks intentional rather than stock pandoc grey.
    let styles_xml = zip_entry_to_string(&bytes, "word/styles.xml");
    assert!(
        styles_xml.contains("2E5AAC"),
        "expected reference.docx's brand accent color in word/styles.xml"
    );
}

/// Pandoc's docx/odt writers never apply a div's `custom-style` to headings,
/// blockquotes, code blocks, or links — they always keep their own built-in
/// style regardless (documented under "Custom Styles" in the pandoc manual).
/// That means `section-divider` (assigned to heading-only pages) and
/// `callout` (assigned to blockquote-only pages) never show up via the
/// README fixture's auto-classified shapes. Exercise them directly against
/// plain-paragraph bodies instead, where the exception doesn't apply.
fn all_classes_doc() -> ResolvedDoc {
    let pages = vec![
        Page {
            class: "hero".into(),
            body: "Hero body text.".into(),
            origin: PageOrigin::Explicit,
        },
        Page {
            class: "content".into(),
            body: "Content body text.".into(),
            origin: PageOrigin::Explicit,
        },
        Page {
            class: "thanks".into(),
            body: "Thanks body text.".into(),
            origin: PageOrigin::Explicit,
        },
        Page {
            class: "image-full".into(),
            body: "Image-full body text.".into(),
            origin: PageOrigin::Explicit,
        },
        Page {
            class: "section-divider".into(),
            body: "Section-divider body text.".into(),
            origin: PageOrigin::Explicit,
        },
        Page {
            class: "callout".into(),
            body: "Callout body text.".into(),
            origin: PageOrigin::Explicit,
        },
    ];
    ResolvedDoc {
        pages,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
    }
}

#[tokio::test]
async fn docx_custom_styles_apply_to_plain_paragraphs() {
    if !pandoc_available() {
        eprintln!("skipping docx_custom_styles_apply_to_plain_paragraphs: `pandoc` not on PATH");
        return;
    }
    let doc = all_classes_doc();
    let (_tmp, out) = render(Target::Docx, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    let document_xml = zip_entry_to_string(&bytes, "word/document.xml");
    for class in [
        "hero",
        "content",
        "thanks",
        "image-full",
        "section-divider",
        "callout",
    ] {
        let needle = format!(r#"<w:pStyle w:val="{class}" />"#);
        assert!(
            document_xml.contains(&needle),
            "expected {needle:?} in word/document.xml:\n{document_xml}"
        );
    }
}

#[tokio::test]
async fn odt_custom_styles_apply_to_plain_paragraphs() {
    if !pandoc_available() {
        eprintln!("skipping odt_custom_styles_apply_to_plain_paragraphs: `pandoc` not on PATH");
        return;
    }
    let doc = all_classes_doc();
    let (_tmp, out) = render(Target::Odt, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    let content_xml = zip_entry_to_string(&bytes, "content.xml");
    for class in [
        "hero",
        "content",
        "thanks",
        "image-full",
        "section-divider",
        "callout",
    ] {
        let needle = format!(r#"text:style-name="{class}""#);
        assert!(
            content_xml.contains(&needle),
            "expected {needle:?} in content.xml:\n{content_xml}"
        );
    }
}

#[tokio::test]
async fn pandoc_odt_smoke() {
    if !pandoc_available() {
        eprintln!("skipping pandoc_odt_smoke: `pandoc` not on PATH");
        return;
    }
    let doc = resolved_doc(README_EXAMPLE);
    let (_tmp, out) = render(Target::Odt, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK"), "odt should be a zip container");
    assert!(
        bytes.len() > 200,
        "suspiciously small odt: {} bytes",
        bytes.len()
    );

    // README_EXAMPLE has 5 pages — the writer must have emitted 4 real page
    // breaks, referencing a style that's actually defined with
    // fo:break-before="page" (not just a dangling style-name reference).
    let content_xml = zip_entry_to_string(&bytes, "content.xml");
    let break_count = content_xml
        .matches(r#"<text:p text:style-name="PageBreak"/>"#)
        .count();
    assert_eq!(
        break_count, 4,
        "expected 4 page breaks in content.xml, found {break_count}:\n{content_xml}"
    );
    let styles_xml = zip_entry_to_string(&bytes, "styles.xml");
    assert!(
        styles_xml.contains(r#"style:name="PageBreak""#)
            && styles_xml.contains(r#"fo:break-before="page""#),
        "reference.odt's PageBreak style (fo:break-before=\"page\") should be \
         carried into the output styles.xml"
    );

    // Each page's class should resolve to a real paragraph style defined in
    // reference.odt, not silently fall back to pandoc's default styling. See
    // `odt_custom_styles_apply_to_plain_paragraphs` for "image-full",
    // "section-divider" and "callout" — pandoc never applies a custom style
    // to headings/blockquotes, so README_EXAMPLE's auto-classified shapes
    // don't exercise those three.
    for class in ["hero", "content", "thanks"] {
        let needle = format!(r#"text:style-name="{class}""#);
        assert!(
            content_xml.contains(&needle),
            "expected {needle:?} in content.xml (reference.odt style not \
             applied for class {class:?}):\n{content_xml}"
        );
    }

    // Same reasoning as the docx blockquote check above: README_EXAMPLE's
    // blockquote page always renders with the built-in Quotations style, so
    // reference.odt brands Quotations itself.
    assert!(
        styles_xml.contains("#2e5aac"),
        "expected reference.odt's brand accent color in styles.xml"
    );
}

#[tokio::test]
async fn pandoc_pptx_smoke() {
    if !pandoc_available() {
        eprintln!("skipping pandoc_pptx_smoke: `pandoc` not on PATH");
        return;
    }
    let doc = resolved_doc(README_EXAMPLE);
    let (_tmp, out) = render(Target::Pptx, &doc).await;
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK"), "pptx should be a zip container");
    assert!(
        bytes.len() > 200,
        "suspiciously small pptx: {} bytes",
        bytes.len()
    );

    // reference.pptx's accent color should be present in the output theme —
    // proof the real reference doc was used, not pandoc's bundled default
    // (which ships accent1 = 4F81BD).
    let theme_xml = zip_entry_to_string(&bytes, "ppt/theme/theme1.xml");
    assert!(
        theme_xml.contains("2E5AAC"),
        "expected reference.pptx's brand accent color in ppt/theme/theme1.xml:\n{theme_xml}"
    );
}

#[tokio::test]
async fn pandoc_html_reveal_smoke() {
    if !pandoc_available() {
        eprintln!("skipping pandoc_html_reveal_smoke: `pandoc` not on PATH");
        return;
    }
    let doc = resolved_doc(README_EXAMPLE);
    let (_tmp, out) = render(Target::HtmlReveal, &doc).await;
    let html = std::fs::read_to_string(&out).unwrap();
    assert!(html.contains("<html"), "not an html document");
    assert!(
        html.len() > 1000,
        "suspiciously small html: {} bytes",
        html.len()
    );
    assert!(
        !has_external_resource_ref(&html),
        "html-reveal output should be self-contained (--embed-resources default) — found an external resource reference"
    );
}

/// Shared in-memory sink for a scoped `tracing` subscriber, so a test can
/// assert on log output without touching the process-global subscriber.
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl LogBuf {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn typst_unknown_class_falls_back_to_content_with_warning() {
    let doc = ResolvedDoc {
        pages: vec![Page {
            class: "definitely-not-a-real-class".into(),
            body: "# Hi\n\nSome body text.".into(),
            origin: PageOrigin::Explicit,
        }],
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
    };

    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let bytes = {
        let _guard = tracing::subscriber::set_default(subscriber);
        let (_tmp, out) = render(Target::Pdf, &doc).await;
        std::fs::read(&out).unwrap()
    };
    assert!(
        bytes.starts_with(b"%PDF-"),
        "fallback render should still produce a real PDF"
    );

    let logs = buf.contents();
    assert!(
        logs.contains("falling back to content"),
        "expected the documented fallback warning in logs, got:\n{logs}"
    );
}
