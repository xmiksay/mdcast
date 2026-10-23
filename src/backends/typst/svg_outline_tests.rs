use super::*;
use crate::backends::typst::compiler::{CompileFormat, CompileInputs, TypstCompiler};

/// The shape of the production trigger: mermaid-style labels mixing Latin
/// text with a glyph (`↔`) that needs a different font than the text around it.
const MERMAID_LIKE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="60" viewBox="0 0 300 60">
<rect width="300" height="60" fill="#eee"/>
<text x="10" y="35" font-family="trebuchet ms, verdana, arial, sans-serif" font-size="16">axum handlery: HTTP ↔ doména</text>
</svg>"##;

const NO_TEXT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="2cm" height="1cm" viewBox="0 0 20 10"><rect width="20" height="10" fill="red"/></svg>"#;

fn outline_one(path: &str, svg: &str) -> (Vec<(String, Vec<u8>)>, HashSet<String>) {
    outline_files(vec![(path.to_string(), svg.as_bytes().to_vec())], &[])
}

fn as_str(files: &[(String, Vec<u8>)], i: usize) -> &str {
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
    let (files, dropped) = outline_files(files, &[]);
    assert_eq!(dropped, HashSet::from(["bad.svg".to_string()]));
    assert_eq!(files, vec![("data.json".to_string(), b"{}".to_vec())]);
}

#[test]
fn non_svg_files_pass_through_untouched() {
    let png = b"\x89PNG\r\n\x1a\nnot really".to_vec();
    let (files, dropped) = outline_files(vec![("x.png".to_string(), png.clone())], &[]);
    assert!(dropped.is_empty());
    assert_eq!(files, vec![("x.png".to_string(), png)]);
}

#[test]
fn svg_is_detected_by_content_or_extension() {
    assert!(is_svg("blob", NO_TEXT.as_bytes()));
    assert!(is_svg("logo.SVG", b"\x1f\x8b"));
    assert!(is_svg("logo.svgz", b"\x1f\x8b"));
    assert!(!is_svg("data.json", b"{}"));
}

#[test]
fn relative_image_hrefs_resolve_against_the_svg_dir() {
    assert_eq!(
        resolve_href("t/img/a.svg", "b.png").as_deref(),
        Some("t/img/b.png")
    );
    assert_eq!(
        resolve_href("t/img/a.svg", "../b.png").as_deref(),
        Some("t/b.png")
    );
    assert_eq!(
        resolve_href("t/a.svg", "/root.png").as_deref(),
        Some("root.png")
    );
    assert_eq!(resolve_href("a.svg", "../escape.png"), None);
    assert_eq!(resolve_href("a.svg", "https://x/y.png"), None);
}

#[test]
fn declared_fonts_are_added_to_the_bundled_set() {
    let bundled = bundled_font_db().len();
    let extra = typst_assets::fonts()
        .next()
        .expect("bundled fonts")
        .to_vec();
    assert_eq!(font_db(&[]).len(), bundled);
    assert!(font_db(&[extra]).len() > bundled);
}

#[tokio::test]
async fn text_free_svg_keeps_its_size_and_typst_renders_it() {
    let (files, _) = outline_one("img.svg", NO_TEXT);
    assert_eq!(tree_size(NO_TEXT.as_bytes()), tree_size(&files[0].1));
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
    assert_eq!(tree_size(svg.as_bytes()), tree_size(&files[0].1));
}
