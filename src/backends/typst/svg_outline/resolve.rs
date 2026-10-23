//! Bounded gzip inflation and the `<image href>` resolvers used while
//! outlining. Everything here exists so that nothing on the outlining path
//! reaches usvg's unbounded `decompress_svgz` (it runs inside
//! `Tree::from_data` and inside usvg's own nested-SVG loader) and so that a
//! linked image is embedded only when typst itself would have loaded it.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use usvg::{ImageHrefResolver, ImageKind, Options};

use super::Failure;
use crate::image_format::{ImageFormat, sniff};

pub(super) fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// Inflate gzip ourselves under `cap` rather than let usvg `read_to_end` it.
/// A corrupt stream is a parse failure (usvg's `MalformedGZip`, what typst
/// would report); only exceeding `cap` is a limit failure.
fn inflate_capped(data: &[u8], cap: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(cap as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| Failure::Parse(usvg::Error::MalformedGZip))?;
    if out.len() > cap {
        return Err(Failure::Limit(format!(
            "inflates past the {cap}-byte limit"
        )));
    }
    Ok(out)
}

/// One gzip layer inflated (capped), anything else as-is. A second layer is
/// refused rather than handed on: usvg would inflate it again, unbounded, and
/// typst's own import fails on it anyway (it inflates once, then needs UTF-8).
pub(super) fn plain(data: &[u8], cap: usize) -> Result<Cow<'_, [u8]>, Failure> {
    if !is_gzip(data) {
        return Ok(Cow::Borrowed(data));
    }
    let out = inflate_capped(data, cap)?;
    if is_gzip(&out) {
        return Err(Failure::Parse(usvg::Error::NotAnUtf8Str));
    }
    Ok(Cow::Owned(out))
}

/// Parse already-inflated SVG bytes. `Tree::from_str`, never `from_data`, so
/// usvg never gets the chance to inflate anything itself.
pub(super) fn parse(bytes: &[u8], opts: &Options) -> Result<usvg::Tree, Failure> {
    let text = std::str::from_utf8(bytes).map_err(|_| Failure::Parse(usvg::Error::NotAnUtf8Str))?;
    usvg::Tree::from_str(text, opts).map_err(Failure::Parse)
}

/// Running total of image bytes the tree will hold. The resolvers are
/// `Send + Sync` closures, hence atomics.
pub(super) struct Budget {
    cap: usize,
    used: AtomicUsize,
    over: AtomicBool,
}

impl Budget {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            cap,
            used: AtomicUsize::new(0),
            over: AtomicBool::new(false),
        }
    }

    fn charge(&self, len: usize) -> bool {
        let ok = self.used.fetch_add(len, Ordering::Relaxed) + len <= self.cap;
        if !ok {
            self.over.store(true, Ordering::Relaxed);
        }
        ok
    }

    pub(super) fn exceeded(&self) -> bool {
        self.over.load(Ordering::Relaxed)
    }
}

pub(super) fn no_images() -> ImageHrefResolver<'static> {
    ImageHrefResolver {
        resolve_data: Box::new(|_, _, _| None),
        resolve_string: Box::new(|_, _| None),
    }
}

pub(super) fn href_resolver<'a>(
    svg_path: &'a str,
    files: &'a HashMap<&str, &[u8]>,
    budget: &'a Budget,
    cap: usize,
) -> ImageHrefResolver<'a> {
    ImageHrefResolver {
        resolve_data: Box::new(move |mime, data, opts| data_image(mime, data, opts, budget, cap)),
        resolve_string: Box::new(move |href, _| linked_raster(svg_path, href, files, budget)),
    }
}

/// usvg's default data resolver, except that an SVG payload (`image/svg+xml`,
/// or `text/plain` that isn't a raster — the two mimes usvg loads as SVG) is
/// inflated and parsed here, and every payload is charged to `budget`.
fn data_image(
    mime: &str,
    data: Arc<Vec<u8>>,
    opts: &Options,
    budget: &Budget,
    cap: usize,
) -> Option<ImageKind> {
    let svg = match mime {
        "image/svg+xml" => true,
        "text/plain" => raster(&data).is_none(),
        _ => false,
    };
    if !svg {
        return budget
            .charge(data.len())
            .then(|| (ImageHrefResolver::default_data_resolver())(mime, data, opts))
            .flatten();
    }
    let bytes = plain(&data, cap).ok()?;
    if !budget.charge(bytes.len()) {
        return None;
    }
    parse(&bytes, &nested_options(opts))
        .ok()
        .map(ImageKind::SVG)
}

/// usvg's `load_sub_svg` options: a nested SVG may not load images of its
/// own. Keeps our fallback family so its text outlines like the parent's.
fn nested_options<'a>(opts: &'a Options) -> Options<'a> {
    Options {
        dpi: opts.dpi,
        font_family: opts.font_family.clone(),
        font_size: opts.font_size,
        languages: opts.languages.clone(),
        shape_rendering: opts.shape_rendering,
        text_rendering: opts.text_rendering,
        image_rendering: opts.image_rendering,
        default_size: opts.default_size,
        fontdb: opts.fontdb.clone(),
        image_href_resolver: no_images(),
        ..Options::default()
    }
}

/// typst 0.14.2's `ImageResolver::load_or_error`: strip `file://`, refuse
/// absolute paths and URLs, resolve against the SVG's directory, and accept
/// only GIF/JPEG/PNG/WebP — typst rejects a linked SVG or PDF outright, so
/// embedding one here would render something typst never would.
fn linked_raster(
    svg_path: &str,
    href: &str,
    files: &HashMap<&str, &[u8]>,
    budget: &Budget,
) -> Option<ImageKind> {
    let href = href.strip_prefix("file://").unwrap_or(href);
    let data = files.get(resolve_href(svg_path, href)?.as_str())?;
    let kind = linked_kind(href, data)?;
    budget
        .charge(data.len())
        .then(|| kind(Arc::new(data.to_vec())))
}

type Kind = fn(Arc<Vec<u8>>) -> ImageKind;

/// typst's `determine_format_from_path`, falling back to magic bytes.
fn linked_kind(href: &str, data: &[u8]) -> Option<Kind> {
    let ext = Path::new(href)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(ImageKind::PNG),
        "jpg" | "jpeg" => Some(ImageKind::JPEG),
        "gif" => Some(ImageKind::GIF),
        "webp" => Some(ImageKind::WEBP),
        "svg" | "svgz" | "pdf" => None,
        _ => raster(data),
    }
}

pub(super) fn raster(data: &[u8]) -> Option<Kind> {
    match sniff(data) {
        ImageFormat::Png => Some(ImageKind::PNG),
        ImageFormat::Jpeg => Some(ImageKind::JPEG),
        ImageFormat::Gif => Some(ImageKind::GIF),
        ImageFormat::Webp => Some(ImageKind::WEBP),
        _ => None,
    }
}

/// `href` relative to `svg_path`'s directory, normalised to a compile-file
/// path. `None` where typst refuses: an absolute path, a `scheme://` URL, or
/// `..` escaping the root (typst keeps a `/..` component that never exists).
pub(super) fn resolve_href(svg_path: &str, href: &str) -> Option<String> {
    if href.starts_with('/') {
        return None;
    }
    if let Some(pos) = href.find("://") {
        let scheme = &href[..pos];
        if scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return None;
        }
    }
    let dir = svg_path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in href.split('/') {
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
