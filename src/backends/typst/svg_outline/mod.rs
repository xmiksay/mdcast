//! SVG text → paths before typst ever sees an SVG (issue #92).
//!
//! typst's SVG-text pipeline can deadlock (upstream typst #6052/#7178/#6783/
//! #7279, still present in 0.14): an SVG whose `<text>` needs mixed font
//! fallbacks — a mermaid label containing `↔` was the production trigger —
//! hangs the compile non-deterministically, in both compiler modes. So every
//! SVG headed for a typst compile that may contain text is parsed here with
//! `usvg` (the same parser and version typst uses) and re-serialised; usvg's
//! writer emits `<text>` as outlined `<path>`s, so typst only ever loads
//! text-free SVGs. One that provably has no text is only validated and kept
//! byte-for-byte: re-serialising it would resolve `currentColor` and
//! normalise attributes, breaking the typst recolor idiom
//! (`read("icon.svg").replace("currentColor", ...)`).
//!
//! The font database is built once per render, and only when the render
//! actually carries an SVG that may contain text.

mod font_db;
mod resolve;

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;

use anyhow::Result;

use crate::image_format::{ImageFormat, sniff};
use font_db::{Fonts, font_db};
use resolve::{Budget, href_resolver, is_gzip, no_images, parse, plain};

/// Outlining runs in the server process on an uncancellable blocking thread —
/// outside `cli` mode's timeout and `RLIMIT_AS` — so it gets its own fixed
/// ceilings. usvg inflates `.svgz` with an unbounded `read_to_end`, so a few
/// MiB of gzip bomb would otherwise OOM the whole process; 32 MiB of inflated
/// SVG is far beyond any real diagram.
const MAX_INFLATED_SVG: usize = 32 << 20;
/// The writer re-inlines every linked raster as base64 at every reference, so
/// N `<image>`s pointing at one large sibling multiply it N times.
const MAX_OUTLINED_SVG: usize = 64 << 20;

#[derive(Clone, Copy)]
struct Limits {
    inflated: usize,
    output: usize,
}

const LIMITS: Limits = Limits {
    inflated: MAX_INFLATED_SVG,
    output: MAX_OUTLINED_SVG,
};

/// Files after outlining, plus the virtual paths of SVGs that could not be
/// parsed and were dropped. `fonts` is handed back unchanged — it is moved
/// into the blocking task and the caller still needs it for the compile.
pub(super) struct Outlined {
    pub files: Vec<(String, Vec<u8>)>,
    pub fonts: Vec<Vec<u8>>,
    pub dropped: HashSet<String>,
}

/// Outline every SVG in `files` on a blocking thread (parsing and glyph
/// outlining are CPU-bound).
pub(super) async fn outline_svgs(
    files: Vec<(String, Vec<u8>)>,
    fonts: Vec<Vec<u8>>,
) -> Result<Outlined> {
    super::spawn_compile(move || {
        let (files, dropped) = outline_files(files, &fonts, LIMITS);
        Ok(Outlined {
            files,
            fonts,
            dropped,
        })
    })
    .await
}

enum Outcome {
    Keep,
    Replace(Vec<u8>),
    Drop,
}

enum Failure {
    Parse(usvg::Error),
    Limit(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Parse(e) => e.fmt(f),
            Failure::Limit(msg) => f.write_str(msg),
        }
    }
}

/// An SVG usvg cannot parse is dropped with a warning: typst's own SVG import
/// is usvg too, so it would have failed on the same bytes anyway. The one
/// exception is a file that is SVG only by content sniff (no svg/svgz
/// extension) and fails to parse: it may not be an image at all — a gzipped
/// data file a template `read()`s — so it passes through untouched; if it
/// *is* imaged, typst fails on it exactly as usvg just did. Exceeding a cap
/// always drops: passing it on would hand typst the same bomb.
fn outline_files(
    files: Vec<(String, Vec<u8>)>,
    fonts: &[Vec<u8>],
    limits: Limits,
) -> (Vec<(String, Vec<u8>)>, HashSet<String>) {
    let outcomes: Vec<Outcome> = {
        let lookup: HashMap<&str, &[u8]> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let mut db = None;
        files
            .iter()
            .map(|(path, bytes)| {
                let Some(by_ext) = svg_kind(path, bytes) else {
                    return Outcome::Keep;
                };
                match process(path, bytes, &lookup, &mut db, fonts, limits) {
                    Ok(outcome) => outcome,
                    Err(Failure::Parse(e)) if !by_ext => {
                        tracing::debug!(key = %path, error = %e, "sniffed-as-SVG file did not parse; passing it through");
                        Outcome::Keep
                    }
                    Err(e) => {
                        tracing::warn!(key = %path, error = %e, "SVG could not be parsed; dropping it");
                        Outcome::Drop
                    }
                }
            })
            .collect()
    };

    let mut dropped = HashSet::new();
    let mut out = Vec::with_capacity(files.len());
    for ((path, bytes), outcome) in files.into_iter().zip(outcomes) {
        match outcome {
            Outcome::Keep => out.push((path, bytes)),
            Outcome::Replace(svg) => out.push((path, svg)),
            Outcome::Drop => {
                dropped.insert(path);
            }
        }
    }
    (out, dropped)
}

/// A text-free SVG is still parsed so an unparseable one is dropped like any
/// other, but kept as the original bytes. Kept gzip is safe to hand on: it
/// just inflated under the cap and is a single layer.
fn process(
    path: &str,
    bytes: &[u8],
    files: &HashMap<&str, &[u8]>,
    db: &mut Option<Fonts>,
    fonts: &[Vec<u8>],
    limits: Limits,
) -> Result<Outcome, Failure> {
    let svg = plain(bytes, limits.inflated)?;
    if !may_contain_text(&svg) {
        let opts = usvg::Options {
            image_href_resolver: no_images(),
            ..Default::default()
        };
        parse(&svg, &opts)?;
        return Ok(Outcome::Keep);
    }
    let db = db.get_or_insert_with(|| font_db(fonts));
    outline(path, &svg, files, db, limits).map(Outcome::Replace)
}

/// Conservative: a false positive only costs a re-serialisation. `data:`
/// (any case — the scheme is case-insensitive) catches a nested SVG whose
/// `<text>` is hidden in base64; `&#` and `<!ENTITY` catch markup spelled
/// through character or entity references.
fn may_contain_text(svg: &[u8]) -> bool {
    let has = |needle: &[u8]| svg.windows(needle.len()).any(|w| w == needle);
    has(b"text")
        || has(b"&#")
        || has(b"<!ENTITY")
        || svg.windows(5).any(|w| w.eq_ignore_ascii_case(b"data:"))
}

/// typst 0.14's own image-format rule, narrowed to "is it SVG": a known image
/// extension decides outright, anything else is sniffed — raster magic first,
/// then gzip magic or the SVG namespace in the first 2 KiB. Mirroring it
/// exactly matters: any SVG typst would decode that this misses reaches
/// typst's text pipeline un-outlined. `Some(true)` = SVG by extension,
/// `Some(false)` = by content only.
fn svg_kind(path: &str, bytes: &[u8]) -> Option<bool> {
    let ext = Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "svg" | "svgz" => Some(true),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf" => None,
        _ => {
            let raster = matches!(
                sniff(bytes),
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp
            );
            let head = &bytes[..bytes.len().min(2048)];
            let svg = is_gzip(bytes) || head.windows(SVG_NS.len()).any(|w| w == SVG_NS.as_bytes());
            (!raster && svg).then_some(false)
        }
    }
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Parse and re-serialise one (already inflated) SVG. Linked `<image href>`s
/// resolve as typst's own resolver would (see `resolve::linked_raster`), so an
/// SVG that links a sibling raster keeps it, embedded as a data URI by the
/// writer. Every image payload counts against the output cap as it resolves,
/// so a pathological SVG is refused before the tree holds N copies of it.
fn outline(
    path: &str,
    svg: &[u8],
    files: &HashMap<&str, &[u8]>,
    fonts: &Fonts,
    limits: Limits,
) -> Result<Vec<u8>, Failure> {
    let budget = Budget::new(limits.output);
    let opts = usvg::Options {
        font_family: fonts.family.clone(),
        fontdb: fonts.db.clone(),
        resources_dir: None,
        image_href_resolver: href_resolver(path, files, &budget, limits.inflated),
        ..Default::default()
    };
    let tree = parse(svg, &opts)?;
    if budget.exceeded() {
        return Err(Failure::Limit(format!(
            "embedded images exceed the {}-byte output limit",
            limits.output
        )));
    }
    let svg = tree.to_string(&usvg::WriteOptions::default()).into_bytes();
    if svg.len() > limits.output {
        return Err(Failure::Limit(format!(
            "outlined SVG exceeds the {}-byte output limit",
            limits.output
        )));
    }
    Ok(svg)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_images;
