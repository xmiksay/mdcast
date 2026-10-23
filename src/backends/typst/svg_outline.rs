//! SVG text → paths before typst ever sees an SVG (issue #92).
//!
//! typst's SVG-text pipeline can deadlock (upstream typst #6052/#7178/#6783/
//! #7279, still present in 0.14): an SVG whose `<text>` needs mixed font
//! fallbacks — a mermaid label containing `↔` was the production trigger —
//! hangs the compile non-deterministically, in both compiler modes. So every
//! SVG headed for a typst compile is parsed here with `usvg` (the same parser
//! and version typst uses) and re-serialised; usvg's writer emits `<text>` as
//! outlined `<path>`s, so typst only ever loads text-free SVGs.
//!
//! Glyphs are outlined with typst's own bundled fonts plus the render's
//! declared fonts — never system fonts, so output doesn't depend on the host.
//! The bundled database is identical on every render, so it is built once per
//! process; the per-render database is built once per render, and only when
//! the render actually carries an SVG.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Result, bail};
use usvg::fontdb;

use crate::image_format::{ImageFormat, sniff};

/// typst's default text font, and the family usvg falls back to — its
/// default selector always appends the generic `serif` family to a query, so
/// pointing `serif` here means a `font-family` naming nothing we have (Arial,
/// trebuchet ms, ...) still outlines instead of silently vanishing, the same
/// way typst falls back to its own default font.
const DEFAULT_FAMILY: &str = "Libertinus Serif";
const MONOSPACE_FAMILY: &str = "DejaVu Sans Mono";

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

/// An SVG usvg cannot parse is dropped with a warning: typst's own SVG import
/// is usvg too, so it would have failed on the same bytes anyway. The one
/// exception is a file that is SVG only by content sniff (no svg/svgz
/// extension) and fails to parse: it may not be an image at all — a gzipped
/// data file a template `read()`s — so it passes through untouched; if it
/// *is* imaged, typst fails on it exactly as usvg just did.
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
                let db = db.get_or_insert_with(|| font_db(fonts));
                match outline(path, bytes, &lookup, db, limits) {
                    Ok(svg) => Outcome::Replace(svg),
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

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// Inflate gzip ourselves under `cap` rather than let usvg `read_to_end` it.
fn inflate_capped(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(cap as u64 + 1)
        .read_to_end(&mut out)?;
    if out.len() > cap {
        bail!("inflates past the {cap}-byte limit");
    }
    Ok(out)
}

/// A gzip body inflated (capped), anything else as-is.
fn plain(data: Arc<Vec<u8>>, cap: usize) -> Result<Arc<Vec<u8>>> {
    if is_gzip(&data) {
        Ok(Arc::new(inflate_capped(&data, cap)?))
    } else {
        Ok(data)
    }
}

/// The per-render font set: the database plus the family unmatched and
/// generic `font-family` names resolve to.
struct Fonts {
    db: Arc<fontdb::Database>,
    family: String,
}

/// typst's SVG text falls back to the document's active text font, so with
/// declared (brand) fonts the first one stands in for every generic family
/// and the no-match fallback — otherwise a branded render's diagram labels
/// would come out in Libertinus Serif.
fn font_db(fonts: &[Vec<u8>]) -> Fonts {
    let bundled = bundled_font_db();
    let Some(first) = fonts.first() else {
        return Fonts {
            db: bundled.clone(),
            family: DEFAULT_FAMILY.to_string(),
        };
    };
    let mut db = (**bundled).clone();
    for font in fonts {
        db.load_font_data(font.clone());
    }
    let family = family_name(first).unwrap_or_else(|| DEFAULT_FAMILY.to_string());
    db.set_serif_family(family.clone());
    db.set_sans_serif_family(family.clone());
    db.set_cursive_family(family.clone());
    db.set_fantasy_family(family.clone());
    Fonts {
        db: Arc::new(db),
        family,
    }
}

fn family_name(font: &[u8]) -> Option<String> {
    let mut db = fontdb::Database::new();
    db.load_font_data(font.to_vec());
    let face = db.faces().next()?;
    face.families
        .iter()
        .find(|f| f.1 == fontdb::Language::English_UnitedStates)
        .or(face.families.first())
        .map(|f| f.0.clone())
}

fn bundled_font_db() -> &'static Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        for data in typst_assets::fonts() {
            db.load_font_source(fontdb::Source::Binary(Arc::new(data)));
        }
        db.set_serif_family(DEFAULT_FAMILY);
        db.set_monospace_family(MONOSPACE_FAMILY);
        Arc::new(db)
    })
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

/// Parse and re-serialise one SVG. Relative `<image href>`s resolve against
/// the other compile files, mirroring typst's own resolver, so an SVG that
/// links a sibling raster keeps it (embedded as a data URI by the writer).
/// Nested gzip (a linked `.svgz`, a gzip data URI) is inflated under the same
/// cap, and linked bytes count against the output cap as they resolve, so a
/// pathological SVG is refused before the tree holds N copies of a raster.
fn outline(
    path: &str,
    bytes: &[u8],
    files: &HashMap<&str, &[u8]>,
    fonts: &Fonts,
    limits: Limits,
) -> Result<Vec<u8>, Failure> {
    let bytes: Cow<[u8]> = if is_gzip(bytes) {
        Cow::Owned(
            inflate_capped(bytes, limits.inflated).map_err(|e| Failure::Limit(format!("{e:#}")))?,
        )
    } else {
        Cow::Borrowed(bytes)
    };
    let linked = AtomicUsize::new(0);
    let over_budget = AtomicBool::new(false);
    let budget = |len: usize| {
        let ok = linked.fetch_add(len, Ordering::Relaxed) + len <= limits.output;
        if !ok {
            over_budget.store(true, Ordering::Relaxed);
        }
        ok
    };
    let opts = usvg::Options {
        font_family: fonts.family.clone(),
        fontdb: fonts.db.clone(),
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|mime, data, opts| {
                let data = plain(data, limits.inflated).ok()?;
                (usvg::ImageHrefResolver::default_data_resolver())(mime, data, opts)
            }),
            resolve_string: Box::new(|href, opts| {
                let data = files.get(resolve_href(path, href)?.as_str())?;
                if !budget(data.len()) {
                    return None;
                }
                let data = plain(Arc::new(data.to_vec()), limits.inflated).ok()?;
                // "text/plain" makes the default resolver sniff the format.
                (usvg::ImageHrefResolver::default_data_resolver())("text/plain", data, opts)
            }),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(&bytes, &opts).map_err(Failure::Parse)?;
    if over_budget.load(Ordering::Relaxed) {
        return Err(Failure::Limit(format!(
            "linked images exceed the {}-byte output limit",
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

/// `href` relative to `svg_path`'s directory (or the root, if it starts with
/// `/`), normalised to a compile-file path. `None` for URLs and for `..`
/// escaping the root.
fn resolve_href(svg_path: &str, href: &str) -> Option<String> {
    if href.contains(':') {
        return None;
    }
    let (mut parts, rel): (Vec<&str>, &str) = match href.strip_prefix('/') {
        Some(rel) => (Vec::new(), rel),
        None => {
            let dir = svg_path.rsplit_once('/').map_or("", |(d, _)| d);
            (dir.split('/').filter(|s| !s.is_empty()).collect(), href)
        }
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
#[path = "svg_outline_tests.rs"]
mod tests;
