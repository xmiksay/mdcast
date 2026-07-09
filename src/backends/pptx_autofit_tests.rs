use super::*;
use std::io::Write;

/// Build a minimal single-slide pptx zip with the given `ppt/slides/slide1.xml`
/// body, plus one untouched sibling entry — enough to exercise the patcher
/// without depending on pandoc being installed.
fn make_pptx(slide_xml: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let options = SimpleFileOptions::default();
        zip.start_file("ppt/slides/slide1.xml", options).unwrap();
        zip.write_all(slide_xml.as_bytes()).unwrap();
        zip.start_file("ppt/presentation.xml", options).unwrap();
        zip.write_all(b"<p:presentation/>").unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn slide1_xml(patched: &[u8]) -> String {
    let mut archive = ZipArchive::new(Cursor::new(patched)).unwrap();
    let mut file = archive.by_name("ppt/slides/slide1.xml").unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut file, &mut s).unwrap();
    s
}

const TITLE_AND_BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1" /><p:nvPr><p:ph type="title" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr /><a:p><a:r><a:t>Title</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2" /><p:nvPr><p:ph idx="1" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr /><a:p><a:r><a:t>bullet</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;

#[test]
fn inserts_normautofit_into_self_closing_body_placeholder_bodypr() {
    let pptx = make_pptx(TITLE_AND_BODY);
    let patched = add_autofit(&pptx).unwrap();
    let xml = slide1_xml(&patched);

    // Body placeholder's bodyPr gained the element (the trailing space before
    // `>` is preserved from the original self-closing `<a:bodyPr />` bytes).
    assert!(
        xml.contains(r#"<p:ph idx="1" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr ><a:normAutofit/></a:bodyPr>"#),
        "{xml}"
    );
    // ...but the title placeholder's bodyPr did not.
    assert!(
        xml.contains(
            r#"<p:ph type="title" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr /><a:p>"#
        ),
        "{xml}"
    );
}

#[test]
fn expanded_bodypr_gains_normautofit_child() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2" /><p:nvPr><p:ph idx="1" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr wrap="square"><a:prstTxWarp prst="textNoShape" /></a:bodyPr><a:p><a:r><a:t>bullet</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
    let pptx = make_pptx(xml);

    let patched = add_autofit(&pptx).unwrap();
    let out = slide1_xml(&patched);

    assert!(
        out.contains(r#"<a:bodyPr wrap="square"><a:prstTxWarp prst="textNoShape" /><a:normAutofit/></a:bodyPr>"#),
        "{out}"
    );
}

#[test]
fn existing_autofit_element_is_replaced() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2" /><p:nvPr><p:ph idx="1" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr><a:noAutofit /></a:bodyPr><a:p><a:r><a:t>bullet</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
    let pptx = make_pptx(xml);

    let patched = add_autofit(&pptx).unwrap();
    let out = slide1_xml(&patched);

    assert!(
        out.contains("<a:bodyPr><a:normAutofit/></a:bodyPr>"),
        "{out}"
    );
    assert!(!out.contains("noAutofit"), "{out}");
}

#[test]
fn existing_spautofit_with_fontscale_is_replaced() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2" /><p:nvPr><p:ph idx="1" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr><a:normAutofit fontScale="62500" lnSpcReduction="20000" /></a:bodyPr><a:p><a:r><a:t>bullet</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
    let pptx = make_pptx(xml);

    let patched = add_autofit(&pptx).unwrap();
    let out = slide1_xml(&patched);

    assert!(
        out.contains("<a:bodyPr><a:normAutofit/></a:bodyPr>"),
        "{out}"
    );
    assert!(!out.contains("fontScale"), "{out}");
}

#[test]
fn title_and_non_placeholder_shapes_are_untouched() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1" /><p:nvPr><p:ph type="title" /></p:nvPr></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr /><a:p><a:r><a:t>Title</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="4" name="TextBox 3" /><p:nvPr /></p:nvSpPr><p:spPr /><p:txBody><a:bodyPr /><a:p><a:r><a:t>free text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
    let pptx = make_pptx(xml);

    let patched = add_autofit(&pptx).unwrap();
    let out = slide1_xml(&patched);

    assert_eq!(out, xml, "no placeholder shape should gain normAutofit");
}

#[test]
fn non_slide_zip_entries_are_byte_identical() {
    let pptx = make_pptx(TITLE_AND_BODY);
    let patched = add_autofit(&pptx).unwrap();

    let mut before = ZipArchive::new(Cursor::new(&pptx)).unwrap();
    let mut after = ZipArchive::new(Cursor::new(&patched)).unwrap();
    let mut before_bytes = Vec::new();
    let mut after_bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut before.by_name("ppt/presentation.xml").unwrap(),
        &mut before_bytes,
    )
    .unwrap();
    std::io::Read::read_to_end(
        &mut after.by_name("ppt/presentation.xml").unwrap(),
        &mut after_bytes,
    )
    .unwrap();

    assert_eq!(before_bytes, after_bytes);
}

#[test]
fn malformed_zip_errors_instead_of_panicking() {
    let err = add_autofit(b"not a zip file").unwrap_err();
    assert!(err.to_string().contains("zip"), "{err}");
}

#[test]
fn malformed_slide_xml_errors_instead_of_panicking() {
    let pptx = make_pptx("<p:sld><unclosed");
    let err = add_autofit(&pptx).unwrap_err();
    assert!(err.to_string().contains("slide1.xml"), "{err}");
}

#[test]
fn is_slide_xml_matches_only_top_level_numbered_slides() {
    assert!(is_slide_xml("ppt/slides/slide1.xml"));
    assert!(is_slide_xml("ppt/slides/slide42.xml"));
    assert!(!is_slide_xml("ppt/slides/_rels/slide1.xml.rels"));
    assert!(!is_slide_xml("ppt/slideLayouts/slideLayout1.xml"));
    assert!(!is_slide_xml("ppt/slideMasters/slideMaster1.xml"));
    assert!(!is_slide_xml("ppt/slides/slide.xml"));
}
