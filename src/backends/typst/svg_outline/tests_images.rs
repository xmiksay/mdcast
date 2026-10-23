//! Gzip layering, data URIs, linked files and the text-free fast path.

use super::resolve::resolve_href;
use super::tests::{NO_TEXT, PNG_1X1, as_str, gzip, linking_svg, outline_one};
use super::*;

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize])
            } else {
                '='
            });
        }
    }
    out
}

fn svg_with(body: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10">{body}</svg>"#
    )
}

fn data_image(mime: &str, payload: &[u8]) -> String {
    format!(
        r#"<image width="1" height="1" href="data:{mime};base64,{}"/>"#,
        base64(payload)
    )
}

fn has_text(group: &usvg::Group) -> bool {
    group.children().iter().any(|node| match node {
        usvg::Node::Text(_) => true,
        usvg::Node::Group(g) => has_text(g),
        usvg::Node::Image(img) => match img.kind() {
            usvg::ImageKind::SVG(tree) => has_text(tree.root()),
            _ => false,
        },
        usvg::Node::Path(_) => false,
    })
}

fn tree_has_text(svg: &[u8]) -> bool {
    let opts = usvg::Options {
        fontdb: super::font_db::bundled_font_db().clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(svg, &opts).expect("parses");
    has_text(tree.root())
}

fn files(entries: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    entries
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_vec()))
        .collect()
}

/// Inflates to 1 MiB, compresses to a few hundred bytes.
fn padded(svg: &str) -> Vec<u8> {
    let mut bomb = svg.as_bytes().to_vec();
    bomb.resize(1 << 20, b' ');
    bomb
}

const SMALL_INFLATE: Limits = Limits {
    inflated: 64 << 10,
    ..LIMITS
};

#[test]
fn gzip_of_gzip_is_refused_without_inflating_the_inner_layer() {
    // The inner layer would blow the cap; the outer inflates fine.
    let double = gzip(&gzip(&padded(NO_TEXT)));
    let (out, dropped) = outline_files(files(&[("x.svgz", &double)]), &[], SMALL_INFLATE);
    assert!(out.is_empty());
    assert!(dropped.contains("x.svgz"));
    let lookup: HashMap<&str, &[u8]> = HashMap::new();
    let err = process("x.svgz", &double, &lookup, &mut None, &[], SMALL_INFLATE);
    assert!(matches!(
        err,
        Err(Failure::Parse(usvg::Error::NotAnUtf8Str))
    ));
    // Sniff-only, it is a parse failure like any other: passed through as is.
    let (out, dropped) = outline_files(files(&[("blob", &double)]), &[], SMALL_INFLATE);
    assert!(dropped.is_empty());
    assert_eq!(out[0].1, double);
}

#[test]
fn corrupt_gzip_is_a_parse_failure_not_a_limit() {
    let corrupt: &[u8] = &[0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0xff, 0xde, 0xad];
    let (out, dropped) = outline_files(
        files(&[("blob", corrupt), ("c.svgz", corrupt)]),
        &[],
        LIMITS,
    );
    assert_eq!(out, files(&[("blob", corrupt)]));
    assert_eq!(dropped, HashSet::from(["c.svgz".to_string()]));
}

#[test]
fn raster_data_uris_are_never_inflated() {
    // Gzip behind a raster mime is charged at its raw size: inflated, three
    // of these would blow a 64 KiB budget (or the 64 KiB inflate cap).
    let payload = gzip(&padded(NO_TEXT));
    let images: String = (0..3).map(|_| data_image("image/png", &payload)).collect();
    let svg = svg_with(&format!("<text>a</text>{images}"));
    let limits = Limits {
        output: 64 << 10,
        ..SMALL_INFLATE
    };
    let (out, dropped) = outline_files(files(&[("a.svg", svg.as_bytes())]), &[], limits);
    assert!(dropped.is_empty());
    assert_eq!(out.len(), 1);
}

#[test]
fn inflated_svg_data_uris_are_charged_to_the_output_budget() {
    let nested = gzip(&padded(NO_TEXT));
    let images: String = (0..20)
        .map(|_| data_image("image/svg+xml", &nested))
        .collect();
    let svg = svg_with(&images);
    let limits = Limits {
        output: 4 << 20,
        ..LIMITS
    };
    let (out, dropped) = outline_files(files(&[("a.svg", svg.as_bytes())]), &[], limits);
    assert!(out.is_empty(), "20 MiB of inflated SVG fit a 4 MiB budget");
    assert!(dropped.contains("a.svg"));
    // Raster payloads are charged too, at their raw size.
    let images: String = (0..20).map(|_| data_image("image/png", PNG_1X1)).collect();
    let svg = svg_with(&images);
    let tiny = Limits {
        output: 200,
        ..LIMITS
    };
    let (_, dropped) = outline_files(files(&[("a.svg", svg.as_bytes())]), &[], tiny);
    assert!(dropped.contains("a.svg"));
}

#[test]
fn nested_base64_svg_with_text_is_outlined() {
    let nested = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><text y="8">Hi</text></svg>"#;
    for (mime, payload) in [
        ("image/svg+xml", nested.as_bytes().to_vec()),
        ("image/svg+xml", gzip(nested.as_bytes())),
        ("text/plain", nested.as_bytes().to_vec()),
    ] {
        let svg = svg_with(&data_image(mime, &payload));
        assert!(tree_has_text(svg.as_bytes()), "fixture must carry text");
        let (out, dropped) = outline_one("a.svg", &svg);
        assert!(dropped.is_empty());
        assert!(!tree_has_text(&out[0].1), "{mime}: nested text survived");
    }
}

#[test]
fn text_free_svg_is_kept_byte_identical() {
    let icon = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M0 0h24v24H0z"/></svg>"#;
    assert_eq!(outline_one("icon.svg", icon).0[0].1, icon.as_bytes());
    // Gzipped too: the original stream, not its inflation.
    let gz = gzip(icon.as_bytes());
    let (out, _) = outline_files(files(&[("icon.svgz", &gz)]), &[], LIMITS);
    assert_eq!(out[0].1, gz);
    // Still validated: a malformed text-free SVG is dropped.
    assert!(
        outline_one("bad.svg", "<svg><g></svg>")
            .1
            .contains("bad.svg")
    );
}

#[test]
fn text_detection_is_conservative() {
    assert!(!may_contain_text(NO_TEXT.as_bytes()));
    for hint in [
        "<text>",
        "<svg:textPath",
        "DATA:image/png",
        "&#116;",
        "<!ENTITY e \"x\">",
    ] {
        assert!(may_contain_text(hint.as_bytes()), "{hint}");
    }
}

fn outlined_with(link: &str, sibling: (&str, &[u8])) -> String {
    let svg = linking_svg(1, link);
    let (out, dropped) = outline_files(files(&[sibling, ("d/a.svg", svg.as_bytes())]), &[], LIMITS);
    assert!(dropped.is_empty());
    as_str(&out, 1).to_string()
}

#[test]
fn linked_files_resolve_exactly_as_typst_does() {
    let embedded = |out: &str| out.contains("data:image/png;base64,");
    // Relative, and with `file://` stripped.
    assert!(embedded(&outlined_with("pic.png", ("d/pic.png", PNG_1X1))));
    assert!(embedded(&outlined_with(
        "file://pic.png",
        ("d/pic.png", PNG_1X1)
    )));
    // The extension decides; without one, raster magic does.
    assert!(embedded(&outlined_with("pic", ("d/pic", PNG_1X1))));
    // Absolute hrefs are refused, even when the file exists at the root.
    assert!(!outlined_with("/d/pic.png", ("d/pic.png", PNG_1X1)).contains("data:"));
    // Linked SVG (typst: "not supported yet") is never loaded or inflated.
    let bomb = gzip(&padded(NO_TEXT));
    for (link, bytes) in [
        ("n.svg", NO_TEXT.as_bytes()),
        ("n.svgz", &bomb),
        ("n", NO_TEXT.as_bytes()),
    ] {
        let key = format!("d/{link}");
        assert!(
            !outlined_with(link, (&key, bytes)).contains("data:"),
            "{link}"
        );
    }
}

#[test]
fn relative_image_hrefs_resolve_against_the_svg_dir() {
    let r = |svg: &str, href: &str| resolve_href(svg, href);
    assert_eq!(r("t/img/a.svg", "b.png").as_deref(), Some("t/img/b.png"));
    assert_eq!(r("t/img/a.svg", "../b.png").as_deref(), Some("t/b.png"));
    assert_eq!(r("a.svg", "c:d.png").as_deref(), Some("c:d.png"));
    assert_eq!(r("t/a.svg", "/root.png"), None);
    assert_eq!(r("a.svg", "../escape.png"), None);
    assert_eq!(r("a.svg", "https://x/y.png"), None);
}
