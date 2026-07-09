#![cfg(feature = "remote-images")]
//! End-to-end coverage for issue #54 step 2: with the off-by-default
//! `remote-images` feature on, an `http://` image reference is fetched once
//! through `images::collect_images` and flows into both engines the same
//! way a provider-resolved image already does — the typst path registers it
//! as a virtual file, the pandoc path materialises it to a temp dir and
//! rewrites the markdown, so pandoc's own (subprocess-side) network fetch is
//! never exercised. Kept in its own file, gated on the feature, rather than
//! folded into `render_smoke.rs` (already past the project's 400-line file
//! cap) — mirrors `typst_template_html.rs`'s pattern for `typst-html`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mdcast::backends::Registry;
use mdcast::{
    BrandHandle, BrandSpec, DocMeta, EmbeddedAssets, Page, PageOrigin, RenderRequest, ResolvedDoc,
    Target,
};

fn pandoc_available() -> bool {
    std::process::Command::new("pandoc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A minimal valid 1x1 PNG — small enough to hardcode, real enough for both
/// engines to recognise as image data.
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xF7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// Binds a local HTTP server that replies `body` to every request until the
/// process exits, counting hits — lets a test assert `collect_images`'s
/// existing dedup fired (one fetch per unique URL) rather than one per
/// reference.
fn serve_forever(body: &'static [u8]) -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = hits.clone();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { break };
            hits_c.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.write_all(body);
        }
    });
    (format!("http://{addr}/pic.png"), hits)
}

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

async fn render(target: Target, doc: &ResolvedDoc) -> Vec<u8> {
    let assets = EmbeddedAssets;
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out.bin");
    let req = RenderRequest {
        doc,
        assets: &assets,
        out: &out,
    };
    Registry::with_defaults()
        .render(target, &req)
        .await
        .unwrap_or_else(|e| panic!("render {target:?} failed: {e:#}"));
    std::fs::read(&out).unwrap()
}

#[tokio::test]
async fn typst_pdf_fetches_remote_image_exactly_once() {
    let (url, hits) = serve_forever(PNG_1X1);
    let doc = doc_with_image(&url);

    let bytes = render(Target::Pdf, &doc).await;

    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "expected exactly one fetch of the remote image"
    );
}

#[tokio::test]
async fn pandoc_docx_embeds_fetched_remote_image_without_pandoc_touching_the_network() {
    if !pandoc_available() {
        eprintln!("pandoc not on PATH; skipping");
        return;
    }
    let (url, hits) = serve_forever(PNG_1X1);
    let doc = doc_with_image(&url);

    let bytes = render(Target::Docx, &doc).await;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let has_media = (0..archive.len()).any(|i| {
        archive
            .by_index(i)
            .unwrap()
            .name()
            .starts_with("word/media/")
    });
    assert!(
        has_media,
        "expected the fetched remote image under word/media/ in the docx"
    );
    // A single hit proves mdcast fetched it (and rewrote the markdown to a
    // local path) rather than leaving the URL for pandoc's own subprocess to
    // fetch a second time.
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "expected exactly one fetch of the remote image"
    );
}
