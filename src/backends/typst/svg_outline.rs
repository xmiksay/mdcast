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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use usvg::fontdb;

use crate::image_format::{ImageFormat, sniff};

/// typst's default text font, and the family usvg falls back to — its
/// default selector always appends the generic `serif` family to a query, so
/// pointing `serif` here means a `font-family` naming nothing we have (Arial,
/// trebuchet ms, ...) still outlines instead of silently vanishing, the same
/// way typst falls back to its own default font.
const DEFAULT_FAMILY: &str = "Libertinus Serif";
const MONOSPACE_FAMILY: &str = "DejaVu Sans Mono";

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
        let (files, dropped) = outline_files(files, &fonts);
        Ok(Outlined {
            files,
            fonts,
            dropped,
        })
    })
    .await
}

/// An SVG usvg cannot parse is dropped with a warning: typst's own SVG import
/// is usvg too, so it would have failed on the same bytes anyway.
fn outline_files(
    files: Vec<(String, Vec<u8>)>,
    fonts: &[Vec<u8>],
) -> (Vec<(String, Vec<u8>)>, HashSet<String>) {
    let mut replaced: HashMap<usize, Option<Vec<u8>>> = HashMap::new();
    {
        let lookup: HashMap<&str, &[u8]> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let mut db = None;
        for (i, (path, bytes)) in files.iter().enumerate() {
            if !is_svg(path, bytes) {
                continue;
            }
            let db = db.get_or_insert_with(|| font_db(fonts));
            match outline(path, bytes, &lookup, db) {
                Ok(svg) => replaced.insert(i, Some(svg)),
                Err(e) => {
                    tracing::warn!(key = %path, error = %e, "SVG could not be parsed; dropping it");
                    replaced.insert(i, None)
                }
            };
        }
    }

    let mut dropped = HashSet::new();
    let mut out = Vec::with_capacity(files.len());
    for (i, (path, bytes)) in files.into_iter().enumerate() {
        match replaced.remove(&i) {
            None => out.push((path, bytes)),
            Some(Some(svg)) => out.push((path, svg)),
            Some(None) => {
                dropped.insert(path);
            }
        }
    }
    (out, dropped)
}

/// Content sniff first; the extension catches what the sniff can't see — a
/// gzipped `.svgz`, or a root `<svg` beyond the sniff's 1 KiB window — since
/// typst picks the SVG decoder by extension too.
fn is_svg(path: &str, bytes: &[u8]) -> bool {
    let lower = path.to_ascii_lowercase();
    sniff(bytes) == ImageFormat::Svg || lower.ends_with(".svg") || lower.ends_with(".svgz")
}

fn font_db(fonts: &[Vec<u8>]) -> Arc<fontdb::Database> {
    let bundled = bundled_font_db();
    if fonts.is_empty() {
        return bundled.clone();
    }
    let mut db = (**bundled).clone();
    for font in fonts {
        db.load_font_data(font.clone());
    }
    Arc::new(db)
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

/// Parse and re-serialise one SVG. Relative `<image href>`s resolve against
/// the other compile files, mirroring typst's own resolver, so an SVG that
/// links a sibling raster keeps it (embedded as a data URI by the writer).
fn outline(
    path: &str,
    bytes: &[u8],
    files: &HashMap<&str, &[u8]>,
    db: &Arc<fontdb::Database>,
) -> Result<Vec<u8>, usvg::Error> {
    let opts = usvg::Options {
        font_family: DEFAULT_FAMILY.to_string(),
        fontdb: db.clone(),
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|href, opts| {
                let data = files.get(resolve_href(path, href)?.as_str())?;
                // "text/plain" makes the default resolver sniff the format.
                (usvg::ImageHrefResolver::default_data_resolver())(
                    "text/plain",
                    Arc::new(data.to_vec()),
                    opts,
                )
            }),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &opts)?;
    Ok(tree.to_string(&usvg::WriteOptions::default()).into_bytes())
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
