use bytes::Bytes;
use mdcast_api::wire::{GeneratedSpec, QrEcLevel};

use super::qr::{render_qr_codes, svg};
use super::*;
use crate::sync_provider;

fn provider() -> impl AssetProvider {
    sync_provider(|key| match key {
        "pay.txt" => Ok(Some(Bytes::from_static(b"SPD*1.0*AM:100\n"))),
        "binary.bin" => Ok(Some(Bytes::from_static(&[0xff, 0xfe]))),
        _ => Ok(None),
    })
}

async fn qr(markdown: &str) -> Generated {
    render_qr_codes(markdown, &provider()).await
}

/// The `viewBox` side length — grows with QR version, so it tells a denser
/// code from a sparser one without decoding anything.
fn side(svg: &str) -> usize {
    let vb = svg.split("viewBox=\"0 0 ").nth(1).unwrap();
    vb.split(' ').next().unwrap().parse().unwrap()
}

#[tokio::test]
async fn fence_becomes_an_image_ref_with_its_svg() {
    let r = qr("# Pay\n\n```qr\nSPD*1.0\n```\n\nThanks.\n").await;
    assert_eq!(r.markdown, "# Pay\n\n![](qr/qr-0.svg)\n\nThanks.\n");
    assert_eq!(r.svgs.len(), 1);
    assert_eq!(r.svgs[0].0, "qr/qr-0.svg");
    assert!(r.svgs[0].1.starts_with(b"<svg"));
}

#[tokio::test]
async fn inline_tag_is_replaced_in_place() {
    let r = qr("Scan <qr>https://example.com</qr> now.\n").await;
    assert_eq!(r.markdown, "Scan ![](qr/qr-0.svg) now.\n");
    assert_eq!(r.svgs.len(), 1);
}

#[tokio::test]
async fn tag_body_is_trimmed() {
    let a = qr("<qr>\n  SPD*1.0\n</qr>\n").await;
    let b = qr("<qr>SPD*1.0</qr>\n").await;
    assert_eq!(a.svgs[0].1, b.svgs[0].1);
}

#[tokio::test]
async fn path_attribute_reads_through_the_provider() {
    let from_path = qr("<qr path=\"pay.txt\">\n").await;
    let inline = qr("<qr>SPD*1.0*AM:100</qr>\n").await;
    assert_eq!(from_path.markdown, "![](qr/qr-0.svg)\n");
    assert_eq!(
        from_path.svgs[0].1, inline.svgs[0].1,
        "file content is trimmed of its trailing newline"
    );
}

#[tokio::test]
async fn path_tag_swallows_an_adjacent_close_tag() {
    let r = qr("<qr path=\"pay.txt\"></qr> after\n").await;
    assert_eq!(r.markdown, "![](qr/qr-0.svg) after\n");
}

#[tokio::test]
async fn missing_or_non_utf8_path_keeps_the_tag() {
    let md = "<qr path=\"nope.txt\">\n\n<qr path=\"binary.bin\">\n";
    let r = qr(md).await;
    assert_eq!(r.markdown, md);
    assert!(r.svgs.is_empty());
}

#[tokio::test]
async fn tags_inside_code_are_left_alone() {
    let md = "`<qr>a</qr>`\n\n```html\n<qr>b</qr>\n```\n\n    <qr>c</qr>\n";
    let r = qr(md).await;
    assert_eq!(r.markdown, md);
    assert!(r.svgs.is_empty());
}

#[tokio::test]
async fn other_fences_are_untouched() {
    let md = "```rust\nfn main() {}\n```\n";
    assert_eq!(qr(md).await.markdown, md);
}

#[tokio::test]
async fn fences_and_tags_are_keyed_in_document_order() {
    let r = qr("<qr>one</qr>\n\n```qr\ntwo\n```\n\n<qr>three</qr>\n").await;
    assert_eq!(
        r.markdown,
        "![](qr/qr-0.svg)\n\n![](qr/qr-1.svg)\n\n![](qr/qr-2.svg)\n"
    );
    let keys: Vec<_> = r.svgs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["qr/qr-0.svg", "qr/qr-1.svg", "qr/qr-2.svg"]);
}

#[tokio::test]
async fn invalid_block_stays_and_does_not_shift_later_keys() {
    let r = qr("```qr ec=X\nbad\n```\n\n<qr>ok</qr>\n").await;
    assert!(r.markdown.starts_with("```qr ec=X\nbad\n```\n"));
    assert!(r.markdown.ends_with("![](qr/qr-1.svg)\n"));
    assert_eq!(r.svgs.len(), 1);
    assert_eq!(r.svgs[0].0, "qr/qr-1.svg");
}

#[tokio::test]
async fn unclosed_or_empty_tags_are_left_alone() {
    for md in ["<qr>never closed\n", "<qr/>\n", "<qr></qr>\n"] {
        let r = qr(md).await;
        assert_eq!(r.markdown, md);
        assert!(r.svgs.is_empty());
    }
}

#[tokio::test]
async fn fence_attributes_and_case_are_tolerated() {
    let r = qr("```QR ec=\"H\" size=large\ndata\n```\n").await;
    assert_eq!(r.markdown, "![](qr/qr-0.svg)\n");
    let svg = std::str::from_utf8(&r.svgs[0].1).unwrap();
    assert!(svg.contains("width=\"6cm\""), "{svg}");
}

#[tokio::test]
async fn tag_attributes_set_ec_and_size() {
    let r = qr("<QR ec=\"L\" size=\"small\">data</QR>\n").await;
    let svg = std::str::from_utf8(&r.svgs[0].1).unwrap();
    assert!(svg.contains("width=\"2cm\""), "{svg}");
}

#[test]
fn higher_ec_needs_a_denser_code() {
    let data = "SPD*1.0*ACC:CZ6508000000192000145399*AM:480.55*CC:CZK*MSG:INV-042";
    let low = svg(data, QrEcLevel::L, Size::Normal).unwrap();
    let high = svg(data, QrEcLevel::H, Size::Normal).unwrap();
    assert!(side(&high) > side(&low));
}

#[test]
fn svg_has_quiet_zone_and_intrinsic_size() {
    let s = svg("x", QrEcLevel::M, Size::Normal).unwrap();
    // Version 1 is 21 modules wide, plus 4 quiet modules each side.
    assert_eq!(side(&s), 29);
    assert!(s.contains("width=\"4cm\" height=\"4cm\""));
    assert!(
        s.contains("<path d=\"M4 4h7v1h-7z"),
        "finder pattern row: {s}"
    );
}

#[test]
fn empty_or_oversized_data_is_an_error() {
    assert!(svg("", QrEcLevel::M, Size::Normal).is_err());
    assert!(svg(&"x".repeat(4000), QrEcLevel::M, Size::Normal).is_err());
}

#[test]
fn render_spec_renders_a_qr_entry() {
    let spec = GeneratedSpec::Qr {
        data: "SPD*1.0".into(),
        ec: QrEcLevel::M,
    };
    let bytes = render_spec(&spec).unwrap();
    assert!(bytes.starts_with(b"<svg"));
    let empty = GeneratedSpec::Qr {
        data: String::new(),
        ec: QrEcLevel::M,
    };
    assert!(render_spec(&empty).is_err());
}

#[test]
fn unknown_size_is_an_error() {
    let attrs = Attrs::from([("size".to_string(), "huge".to_string())]);
    assert!(Size::from_attrs(&attrs).is_err());
}
