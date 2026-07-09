//! Post-render patch (issue #56): insert `<a:normAutofit/>` into every body
//! placeholder's `<a:bodyPr>` across `ppt/slides/slide*.xml`, so PowerPoint
//! and LibreOffice shrink overflowing slide text instead of letting it spill
//! off the slide. Pandoc's pptx writer never emits this, and there's no
//! writer option to turn it on — the reference doc can't help either, since
//! autofit lives on each slide's shape, not the layout/master. This is the
//! smallest instance of the `zip` + `quick-xml` template-injection seam
//! `PROJECT_PLAN.md` §10 names for future OOXML patching.
//!
//! Every zip entry outside `ppt/slides/slide*.xml` is copied through via
//! `raw_copy_file` — still-compressed bytes, untouched — so only the slide
//! XML we actually rewrite differs from pandoc's own output.

use std::io::Cursor;

use anyhow::{Context, Result};
use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// Patch a pandoc-produced pptx artifact in place, returning the patched bytes.
pub fn add_autofit(pptx: &[u8]) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(pptx)).context("open pptx as zip archive")?;
    let mut out = ZipWriter::new(Cursor::new(Vec::new()));

    for i in 0..archive.len() {
        let file = archive.by_index(i).context("read zip entry")?;
        let name = file.name().to_string();

        if is_slide_xml(&name) {
            let mut xml = Vec::with_capacity(file.size() as usize);
            let options = SimpleFileOptions::default().compression_method(file.compression());
            std::io::Read::read_to_end(&mut { file }, &mut xml)
                .with_context(|| format!("decompress {name}"))?;
            let patched = patch_slide_xml(&xml).with_context(|| format!("patch {name}"))?;
            out.start_file(&name, options)
                .with_context(|| format!("start zip entry {name}"))?;
            std::io::Write::write_all(&mut out, &patched)
                .with_context(|| format!("write patched {name}"))?;
        } else {
            out.raw_copy_file(file)
                .with_context(|| format!("copy zip entry {name}"))?;
        }
    }

    let cursor = out.finish().context("finalize patched pptx zip")?;
    Ok(cursor.into_inner())
}

/// `ppt/slides/slide<digits>.xml` — excludes `ppt/slides/_rels/...` and the
/// slide-layout/master trees, which don't share this prefix.
fn is_slide_xml(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("ppt/slides/slide")
        .and_then(|r| r.strip_suffix(".xml"))
    else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn is_autofit_name(name: &[u8]) -> bool {
    matches!(name, b"a:normAutofit" | b"a:noAutofit" | b"a:spAutoFit")
}

/// `true` if a `<p:ph .../>` element's `type` attribute marks a title
/// placeholder (`title`/`ctrTitle`) — the only kind left untouched. Any other
/// type (`body`, `subTitle`, ...) or a missing `type` attribute (schema
/// default is `body`) counts as a body placeholder.
fn is_title_ph(e: &BytesStart) -> Result<bool> {
    for attr in e.attributes() {
        let attr = attr.context("parse p:ph attribute")?;
        if attr.key.as_ref() == b"type" {
            let value = attr.value;
            return Ok(matches!(value.as_ref(), b"title" | b"ctrTitle"));
        }
    }
    Ok(false)
}

/// Streaming rewrite of one slide's XML: walk every event, and for each
/// `<a:bodyPr>` belonging to a body-placeholder shape, insert `<a:normAutofit/>`
/// (replacing any existing `a:noAutofit`/`a:normAutofit`/`a:spAutoFit` child).
/// Everything else — including `<a:bodyPr>` on title placeholders and shapes
/// with no placeholder at all — is copied through byte-for-byte.
fn patch_slide_xml(xml: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut sp_is_body = false;
    // While Some, we're inside a body-placeholder's `<a:bodyPr>...</a:bodyPr>`,
    // rewriting its direct children.
    let mut in_body_pr = false;
    // While Some(name), we're skipping a nested subtree of an existing
    // autofit child element (never written back out).
    let mut skipping: Option<(Vec<u8>, u32)> = None;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let event = reader
            .read_event_into(&mut buf)
            .context("parse slide xml")?;
        if matches!(event, Event::Eof) {
            break;
        }

        if let Some((name, depth)) = &mut skipping {
            match &event {
                Event::Start(e) if e.name().as_ref() == name.as_slice() => *depth += 1,
                Event::End(e) if e.name().as_ref() == name.as_slice() => {
                    *depth -= 1;
                    if *depth == 0 {
                        skipping = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        if in_body_pr {
            match &event {
                Event::Start(e) if is_autofit_name(e.name().as_ref()) => {
                    skipping = Some((e.name().as_ref().to_vec(), 1));
                    continue;
                }
                Event::Empty(e) if is_autofit_name(e.name().as_ref()) => {
                    continue;
                }
                Event::End(e) if e.name().as_ref() == b"a:bodyPr" => {
                    writer
                        .write_event(Event::Empty(BytesStart::new("a:normAutofit")))
                        .context("write a:normAutofit")?;
                    writer.write_event(event).context("write slide xml")?;
                    in_body_pr = false;
                    continue;
                }
                _ => {}
            }
            writer.write_event(event).context("write slide xml")?;
            continue;
        }

        match &event {
            Event::Start(e) if e.name().as_ref() == b"p:sp" => {
                sp_is_body = false;
            }
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"p:ph" => {
                sp_is_body = !is_title_ph(e)?;
            }
            Event::Empty(e) if e.name().as_ref() == b"a:bodyPr" && sp_is_body => {
                writer
                    .write_event(Event::Start(e.to_owned()))
                    .context("write a:bodyPr open")?;
                writer
                    .write_event(Event::Empty(BytesStart::new("a:normAutofit")))
                    .context("write a:normAutofit")?;
                writer
                    .write_event(Event::End(quick_xml::events::BytesEnd::new("a:bodyPr")))
                    .context("write a:bodyPr close")?;
                continue;
            }
            Event::Start(e) if e.name().as_ref() == b"a:bodyPr" && sp_is_body => {
                writer
                    .write_event(Event::Start(e.to_owned()))
                    .context("write a:bodyPr open")?;
                in_body_pr = true;
                continue;
            }
            _ => {}
        }

        writer.write_event(event).context("write slide xml")?;
    }

    Ok(writer.into_inner().into_inner())
}

#[cfg(test)]
#[path = "pptx_autofit_tests.rs"]
mod tests;
