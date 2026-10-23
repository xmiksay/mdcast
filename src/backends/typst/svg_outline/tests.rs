use super::font_db::{DEFAULT_FAMILY, MONOSPACE_FAMILY, bundled_font_db, family_name};
use super::*;
use crate::backends::typst::compiler::{CompileFormat, CompileInputs, TypstCompiler};
use usvg::fontdb;

/// The shape of the production trigger: mermaid-style labels mixing Latin
/// text with a glyph (`↔`) that needs a different font than the text around it.
const MERMAID_LIKE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="60" viewBox="0 0 300 60">
<rect width="300" height="60" fill="#eee"/>
<text x="10" y="35" font-family="trebuchet ms, verdana, arial, sans-serif" font-size="16">axum handlery: HTTP ↔ doména</text>
</svg>"##;

pub(super) const NO_TEXT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="2cm" height="1cm" viewBox="0 0 20 10"><rect width="20" height="10" fill="red"/></svg>"#;

pub(super) fn outline_one(path: &str, svg: &str) -> (Vec<(String, Vec<u8>)>, HashSet<String>) {
    outline_files(
        vec![(path.to_string(), svg.as_bytes().to_vec())],
        &[],
        LIMITS,
    )
}

pub(super) fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(data).expect("in-memory write");
    enc.finish().expect("in-memory gzip")
}

/// A valid 1x1 RGB PNG.
pub(super) const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// An SVG with a label (so it is outlined, not kept) and `refs` `<image>`s
/// all pointing at `href`.
pub(super) fn linking_svg(refs: usize, href: &str) -> String {
    let images: String = (0..refs)
        .map(|i| format!(r#"<image x="{i}" width="1" height="1" href="{href}"/>"#))
        .collect();
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10"><text y="5">a</text>{images}</svg>"#
    )
}

pub(super) fn as_str(files: &[(String, Vec<u8>)], i: usize) -> &str {
    std::str::from_utf8(&files[i].1).expect("usvg writes UTF-8")
}

fn tree_size(svg: &[u8]) -> (f32, f32) {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).expect("parses");
    (tree.size().width(), tree.size().height())
}

/// Compile a page sized exactly to `img.svg`, so the PDF's MediaBox *is* the
/// image's rendered size.
async fn compile_sized_to(svg: Vec<u8>) -> Vec<u8> {
    let src = "#set page(width: auto, height: auto, margin: 0pt)\n#image(\"img.svg\")\n";
    TypstCompiler::Library
        .compile(CompileInputs {
            main: "main.typ".to_string(),
            sources: vec![("main.typ".to_string(), src.to_string())],
            files: vec![("img.svg".to_string(), svg)],
            fonts: Vec::new(),
            format: CompileFormat::Pdf,
        })
        .await
        .expect("compiles")
}

fn media_box(pdf: &[u8]) -> String {
    let text = String::from_utf8_lossy(pdf);
    let at = text.find("/MediaBox").expect("PDF has a MediaBox");
    let rest = &text[at..];
    rest[..rest.find(']').expect("MediaBox closes") + 1].to_string()
}

#[test]
fn text_becomes_paths() {
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="40"><text x="5" y="25">Hello</text></svg>"#;
    let (files, dropped) = outline_one("a.svg", svg);
    let out = as_str(&files, 0);
    assert!(dropped.is_empty());
    assert!(!out.contains("<text"), "{out}");
    assert!(out.contains("<path") && out.contains(" d=\""), "{out}");
}

#[test]
fn mixed_fallback_text_is_outlined() {
    let (files, dropped) = outline_one("images/diagram.svg", MERMAID_LIKE);
    let out = as_str(&files, 0);
    assert!(dropped.is_empty());
    assert!(!out.contains("<text"), "{out}");
    // One label, many glyphs: the outline must carry substantial path data,
    // not an empty group from a font that matched nothing.
    let path_data: usize = out.split(" d=\"").skip(1).map(str::len).sum();
    assert!(path_data > 1000, "too little path data:\n{out}");

    // The arrow itself resolves through fallback to a bundled font that has it.
    let arrow = MERMAID_LIKE.replace("axum handlery: HTTP ↔ doména", "↔");
    let (files, _) = outline_one("arrow.svg", &arrow);
    assert!(as_str(&files, 0).contains(" d=\""), "↔ was not outlined");
}

#[test]
fn malformed_svg_is_dropped_and_others_kept() {
    let files = vec![
        ("bad.svg".to_string(), b"<svg><g></svg>".to_vec()),
        ("data.json".to_string(), b"{}".to_vec()),
    ];
    let (files, dropped) = outline_files(files, &[], LIMITS);
    assert_eq!(dropped, HashSet::from(["bad.svg".to_string()]));
    assert_eq!(files, vec![("data.json".to_string(), b"{}".to_vec())]);
}

#[test]
fn non_svg_files_pass_through_untouched() {
    let png = b"\x89PNG\r\n\x1a\nnot really".to_vec();
    let (files, dropped) = outline_files(vec![("x.png".to_string(), png.clone())], &[], LIMITS);
    assert!(dropped.is_empty());
    assert_eq!(files, vec![("x.png".to_string(), png)]);
}

#[test]
fn svg_detection_mirrors_typst() {
    // Known image extensions decide outright, whatever the bytes.
    assert_eq!(svg_kind("logo.SVG", b"\x1f\x8b"), Some(true));
    assert_eq!(svg_kind("logo.svgz", b"\x1f\x8b"), Some(true));
    assert_eq!(svg_kind("chart.png", NO_TEXT.as_bytes()), None);
    // Anything else is sniffed: the namespace anywhere in the first 2 KiB...
    let commented = format!("<!-- c -->\n{NO_TEXT}");
    assert_eq!(svg_kind("img/flow", commented.as_bytes()), Some(false));
    assert_eq!(svg_kind("data.json", b"{}"), None);
    // ...or gzip magic, extensionless or behind an unknown extension.
    assert_eq!(svg_kind("img/flow", &gzip(NO_TEXT.as_bytes())), Some(false));
    assert_eq!(svg_kind("chart.svg.gz", b"\x1f\x8b"), Some(false));
    // Raster magic wins over a namespace string inside the raster.
    let mut png = PNG_1X1.to_vec();
    png.extend_from_slice(b"http://www.w3.org/2000/svg");
    assert_eq!(svg_kind("img/pic", &png), None);
}

#[test]
fn extensionless_commented_and_gzipped_svgs_are_outlined() {
    let commented = format!("<!-- c -->\n{MERMAID_LIKE}");
    let files = vec![
        ("images/img__flow".to_string(), commented.into_bytes()),
        ("chart.svg.gz".to_string(), gzip(MERMAID_LIKE.as_bytes())),
    ];
    let (files, dropped) = outline_files(files, &[], LIMITS);
    assert!(dropped.is_empty());
    for i in 0..2 {
        let out = as_str(&files, i);
        assert!(!out.contains("<text") && out.contains(" d=\""), "{out}");
    }
}

#[test]
fn sniffed_non_svg_gzip_passes_through() {
    let gz = gzip(b"{\"not\": \"an svg\"}");
    let (files, dropped) = outline_files(vec![("data.gz".to_string(), gz.clone())], &[], LIMITS);
    assert!(dropped.is_empty());
    assert_eq!(files, vec![("data.gz".to_string(), gz)]);
}

#[test]
fn gzip_inflating_past_the_cap_is_dropped() {
    // Compresses to a few hundred bytes, inflates to 1 MiB.
    let mut bomb = NO_TEXT.as_bytes().to_vec();
    bomb.resize(1 << 20, b' ');
    let limits = Limits {
        inflated: 64 << 10,
        ..LIMITS
    };
    for path in ["bomb.svgz", "bomb"] {
        let (files, dropped) = outline_files(vec![(path.to_string(), gzip(&bomb))], &[], limits);
        assert!(files.is_empty(), "{path} survived");
        assert!(dropped.contains(path));
    }
    // Under the cap the same file outlines.
    let (_, dropped) = outline_files(vec![("ok.svgz".into(), gzip(&bomb))], &[], LIMITS);
    assert!(dropped.is_empty());
}

#[test]
fn output_past_the_cap_is_dropped() {
    let files = || {
        vec![
            ("d/pic.png".to_string(), PNG_1X1.to_vec()),
            (
                "d/a.svg".to_string(),
                linking_svg(10, "pic.png").into_bytes(),
            ),
        ]
    };
    let limits = Limits {
        output: 400,
        ..LIMITS
    };
    let (out, dropped) = outline_files(files(), &[], limits);
    assert_eq!(dropped, HashSet::from(["d/a.svg".to_string()]));
    assert_eq!(out.len(), 1);
    let (_, dropped) = outline_files(files(), &[], LIMITS);
    assert!(dropped.is_empty());
}

#[tokio::test]
async fn linked_raster_is_embedded_and_typst_renders_it() {
    let files = vec![
        ("pic.png".to_string(), PNG_1X1.to_vec()),
        (
            "img.svg".to_string(),
            linking_svg(1, "pic.png").into_bytes(),
        ),
    ];
    let (files, dropped) = outline_files(files, &[], LIMITS);
    assert!(dropped.is_empty());
    let svg = files
        .into_iter()
        .find(|(p, _)| p == "img.svg")
        .expect("svg kept")
        .1;
    let text = std::str::from_utf8(&svg).expect("UTF-8");
    assert!(text.contains("data:image/png;base64,"), "{text}");
    assert!(compile_sized_to(svg).await.starts_with(b"%PDF-"));
}

/// A bundled face that isn't the default family, standing in for a brand font.
fn brand_font() -> (Vec<u8>, String) {
    typst_assets::fonts()
        .map(<[u8]>::to_vec)
        .find_map(|f| {
            let family = family_name(&f)?;
            (family != DEFAULT_FAMILY && family != MONOSPACE_FAMILY).then_some((f, family))
        })
        .expect("a non-default bundled family")
}

#[test]
fn declared_fonts_are_added_to_the_bundled_set() {
    let bundled = bundled_font_db().len();
    let (extra, _) = brand_font();
    assert_eq!(font_db(&[]).db.len(), bundled);
    assert!(font_db(&[extra.as_slice()]).db.len() > bundled);
}

#[test]
fn declared_font_becomes_the_fallback_family() {
    let (font, family) = brand_font();
    let fonts = font_db(&[font.as_slice()]);
    assert_eq!(fonts.family, family);
    for generic in [fontdb::Family::Serif, fontdb::Family::SansSerif] {
        assert_eq!(fonts.db.family_name(&generic), family);
    }
    assert_eq!(font_db(&[]).family, DEFAULT_FAMILY);

    // Unmatched `trebuchet ms, ..., sans-serif` now outlines in the brand face.
    let plain = outline_one("a.svg", MERMAID_LIKE).0;
    let file = vec![("a.svg".to_string(), MERMAID_LIKE.as_bytes().to_vec())];
    let branded = outline_files(file, &[font], LIMITS).0;
    assert_ne!(plain, branded);
}

#[tokio::test]
async fn text_free_svg_is_kept_byte_identical_and_typst_renders_it() {
    let (files, dropped) = outline_one("img.svg", NO_TEXT);
    assert!(dropped.is_empty());
    assert_eq!(files[0].1, NO_TEXT.as_bytes());
    let pdf = compile_sized_to(files[0].1.clone()).await;
    assert!(pdf.starts_with(b"%PDF-"));
}

#[cfg(feature = "qr")]
#[tokio::test]
async fn qr_svg_physical_size_is_unchanged_in_the_pdf() {
    use crate::generators::{Size, qr};
    let svg = qr::svg(
        "https://example.com",
        mdcast_api::wire::QrEcLevel::M,
        Size::Normal,
    )
    .expect("encodes");
    assert!(svg.contains("cm\""), "QR SVG is sized in cm: {svg}");
    let (files, _) = outline_one("img.svg", &svg);
    assert_eq!(
        files[0].1,
        svg.as_bytes(),
        "text-free QR must not be rewritten"
    );

    let before = media_box(&compile_sized_to(svg.into_bytes()).await);
    let after = media_box(&compile_sized_to(files[0].1.clone()).await);
    assert_eq!(before, after);
}

#[cfg(feature = "chess")]
#[test]
fn chess_svg_physical_size_is_unchanged() {
    use crate::generators::{Size, chess};
    let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    let svg = chess::fen_svg(start, &Default::default(), Size::Normal).expect("renders");
    let (files, dropped) = outline_one("board.svg", &svg);
    assert!(dropped.is_empty());
    assert!(!as_str(&files, 0).contains("<text"));
    // Board labels are text, so the board is rewritten — at the same size.
    assert!(may_contain_text(svg.as_bytes()));
    assert_eq!(tree_size(svg.as_bytes()), tree_size(&files[0].1));
}
