//! Pandoc must never read a host file the provider did not hand it. Every
//! syntax that names a resource — markdown images in all their forms, raw
//! HTML, raw-attribute blocks, slide background attributes, a reference
//! definition on another page, and the html-reveal CSS layers — points at a
//! canary file (and at `/etc/hostname`, `/proc/self/environ`), and no
//! artifact may carry those bytes in any encoding. Skips gracefully without
//! `pandoc` on `PATH`, like `render_smoke.rs`.
#![cfg(feature = "pandoc")]

use std::io::Read;
use std::sync::Arc;

use bytes::Bytes;
use mdcast::backends::Registry;
use mdcast::pages::auto::classify;
use mdcast::{
    AutoLayout, BrandHandle, BrandSpec, DefaultSplitter, DocMeta, EmbeddedAssets, HtmlImageTags,
    LayeredAssets, MarkdownPreprocessor, PageSplitter, ResolvedDoc, Target, sync_provider,
};

const ENV_CANARY: &str = "mdcast-env-canary-7f3a9c";
const FILE_CANARY: &str = "mdcast-file-canary-51d2e8";
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn pandoc_available() -> bool {
    std::process::Command::new("pandoc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn markdown(canary: &str) -> String {
    format!(
        r#"<page class="hero">
# Cover

# Background {{data-background-image="{canary}"}}

![](/proc/self/environ) ![](/etc/hostname) ![t]({canary} "title") ![](<{canary}>)
</page>

::: {{.callout}}
<img src="/etc/hostname"> <img src="{canary}"> <img src="img/ok.png" alt="ok">

![x][r] ![y][far]

[r]: /etc/hostname
:::

---

# Raw

```{{=html}}
<img src="{canary}">
```

`<img src="{canary}">`{{=html}}

# Shorthand {{background-image="{canary}"}}

| cell |
|------|
| ![]({canary}) |

---

# Far

[far]: {canary}
"#
    )
}

fn doc(canary: &str) -> ResolvedDoc {
    // `--html-image-tags` turns the `<img>` tags into markdown images first,
    // which must then be dropped (or, for `img/ok.png`, resolved) like any other.
    let markdown = HtmlImageTags.preprocess(&markdown(canary));
    ResolvedDoc {
        pages: classify(DefaultSplitter.split(&markdown), &AutoLayout::default()),
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        assets: Vec::new(),
        fonts: Vec::new(),
        toc: None,
    }
}

/// Every file of a zip artifact decompressed, or the artifact itself.
fn contents(target: Target, artifact: &[u8]) -> Vec<Vec<u8>> {
    if target == Target::HtmlReveal {
        return vec![artifact.to_vec()];
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(artifact)).unwrap();
    (0..zip.len())
        .map(|i| {
            let mut out = Vec::new();
            zip.by_index(i).unwrap().read_to_end(&mut out).unwrap();
            out
        })
        .collect()
}

/// `needle` raw, and base64-encoded at each of the three byte alignments it
/// could start at inside a `data:` URI (minus the edge characters that also
/// encode the neighbouring bytes).
fn encodings(needle: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![needle.to_vec()];
    for pad in 0..3 {
        let mut bytes = vec![0u8; pad];
        bytes.extend_from_slice(needle);
        let encoded = base64(&bytes);
        let inner = &encoded[if pad == 0 { 0 } else { 4 }..encoded.len() - 4];
        out.push(inner.as_bytes().to_vec());
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn no_pandoc_target_embeds_a_host_file() {
    if !pandoc_available() {
        eprintln!("pandoc not on PATH; skipping");
        return;
    }
    // SAFETY: the only test in this binary, set before anything spawns a
    // thread or a child that could read the environment concurrently.
    unsafe { std::env::set_var("MDCAST_LEAK_CANARY", ENV_CANARY) };

    let dir = tempfile::tempdir().unwrap();
    let canary_path = dir.path().join("canary.txt");
    std::fs::write(&canary_path, FILE_CANARY).unwrap();
    let canary = canary_path.display().to_string();
    let brand_css = format!(
        "a {{ background: url({canary}); }} b {{ background: url('file://{canary}'); }} \
         @import \"{canary}\";"
    );
    let dist_css = format!(".reveal {{ background: url(\"{canary}\"); }}");
    let overrides = sync_provider(move |key| {
        Ok(match key {
            "revealjs/brand.css" => Some(Bytes::from(brand_css.clone())),
            "revealjs/dist/theme/black.css" => Some(Bytes::from(dist_css.clone())),
            "img/ok.png" => Some(Bytes::from_static(PNG_1X1)),
            _ => None,
        })
    });
    let assets = LayeredAssets {
        over: overrides,
        base: EmbeddedAssets,
    };

    let mut needles = vec![
        ENV_CANARY.as_bytes().to_vec(),
        FILE_CANARY.as_bytes().to_vec(),
    ];
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim().len() >= 6 {
        needles.push(hostname.trim().as_bytes().to_vec());
    }

    let doc = doc(&canary);
    let registry = Registry::with_defaults();
    for target in [Target::Docx, Target::Odt, Target::Pptx, Target::HtmlReveal] {
        let artifact = registry
            .render_to_bytes(target, &doc, &assets)
            .await
            .unwrap_or_else(|e| panic!("render {target:?}: {e:#}"));
        let files = contents(target, &artifact.primary);
        for needle in &needles {
            for encoded in encodings(needle) {
                assert!(
                    !files.iter().any(|f| contains(f, &encoded)),
                    "{target:?} embeds {:?}",
                    String::from_utf8_lossy(needle)
                );
            }
        }
        // The provider-resolved image still makes it in, so the scrubbing is
        // not just dropping every image.
        assert!(
            files.iter().any(|f| contains(f, PNG_1X1))
                || files
                    .iter()
                    .any(|f| contains(f, &base64(PNG_1X1).as_bytes()[..40])),
            "{target:?} lost the provider-resolved image"
        );
    }

    // Page markup still projects classes once raw HTML is off.
    let html = registry
        .render_to_bytes(Target::HtmlReveal, &doc, &assets)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&html.primary);
    assert!(html.contains("hero"), "the <page class> wrapper was lost");
    assert!(html.contains("callout"), "the ::: {{.X}} wrapper was lost");
}
