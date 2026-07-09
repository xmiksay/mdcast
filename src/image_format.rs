//! Magic-byte image format sniffing for the per-target support-matrix
//! warning (issue #55). Detection only — never transcodes, never fails a
//! render; a hit just becomes a `tracing::warn!` in `images::collect_images`.

use crate::Target;

/// Formats the sniffer can tell apart. `Unknown` covers anything not
/// recognised by magic bytes — kept distinct from "recognised and known
/// unsupported" so an unrecognised blob never triggers a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Svg,
    Webp,
    Pdf,
    Bmp,
    Tiff,
    Avif,
    Heic,
    Unknown,
}

impl ImageFormat {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Gif => "GIF",
            ImageFormat::Svg => "SVG",
            ImageFormat::Webp => "WebP",
            ImageFormat::Pdf => "PDF",
            ImageFormat::Bmp => "BMP",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Avif => "AVIF",
            ImageFormat::Heic => "HEIC",
            ImageFormat::Unknown => "unknown",
        }
    }
}

/// Detect an image format from its leading bytes. Binary magic prefixes are
/// checked first since they're unambiguous; SVG has no fixed prefix (it's
/// XML) so it falls back to a text sniff.
pub(crate) fn sniff(bytes: &[u8]) -> ImageFormat {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return ImageFormat::Png;
    }
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return ImageFormat::Jpeg;
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return ImageFormat::Gif;
    }
    if bytes.starts_with(b"%PDF-") {
        return ImageFormat::Pdf;
    }
    if bytes.starts_with(b"BM") {
        return ImageFormat::Bmp;
    }
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return ImageFormat::Tiff;
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return ImageFormat::Webp;
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return match &bytes[8..12] {
            b"avif" | b"avis" => ImageFormat::Avif,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => {
                ImageFormat::Heic
            }
            _ => ImageFormat::Unknown,
        };
    }
    if looks_like_svg(bytes) {
        return ImageFormat::Svg;
    }
    ImageFormat::Unknown
}

/// SVG sniff by content rather than a fixed prefix: skip a UTF-8 BOM/leading
/// whitespace and look for the root `<svg` element, tolerating a leading
/// `<?xml ...?>` prolog ahead of it.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let Ok(text) = std::str::from_utf8(head) else {
        return false;
    };
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && text.contains("<svg"))
}

/// Targets a format is known-unsupported on, per the matrix documented in
/// the README's "Image formats" section. Anything not listed either works or
/// is unverified/version-dependent (e.g. TIFF in html-reveal depends on the
/// browser) and is deliberately left off rather than guessed at.
fn unsupported_targets(format: ImageFormat) -> &'static [Target] {
    use Target::*;
    match format {
        ImageFormat::Webp => &[Docx, Pptx],
        ImageFormat::Pdf => &[Docx, Odt, Pptx, HtmlReveal],
        ImageFormat::Bmp | ImageFormat::Tiff => &[Pdf, PdfPresentation],
        ImageFormat::Avif => &[Docx, Odt, Pptx, Pdf, PdfPresentation],
        ImageFormat::Heic => &[Docx, Odt, Pptx, Pdf, PdfPresentation, HtmlReveal],
        ImageFormat::Png
        | ImageFormat::Jpeg
        | ImageFormat::Gif
        | ImageFormat::Svg
        | ImageFormat::Unknown => &[],
    }
}

/// Sniff `bytes` and, if the detected format is known-unsupported on
/// `target`, emit one `tracing::warn!` naming the image key, the detected
/// format, and the target. Never fails the render — the embed may still be
/// intentional (e.g. a document destined only for LibreOffice).
pub(crate) fn warn_if_unsupported(key: &str, bytes: &[u8], target: Target) {
    let format = sniff(bytes);
    if unsupported_targets(format).contains(&target) {
        tracing::warn!(
            key,
            format = format.label(),
            target = target.as_str(),
            "image format is known-unsupported for this target; see README's Image formats section"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_png() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), ImageFormat::Png);
    }

    #[test]
    fn sniffs_jpeg() {
        assert_eq!(sniff(b"\xFF\xD8\xFFrest"), ImageFormat::Jpeg);
    }

    #[test]
    fn sniffs_gif() {
        assert_eq!(sniff(b"GIF89arest"), ImageFormat::Gif);
    }

    #[test]
    fn sniffs_svg() {
        assert_eq!(sniff(b"<svg xmlns=\"...\"></svg>"), ImageFormat::Svg);
        assert_eq!(
            sniff(b"<?xml version=\"1.0\"?>\n<svg></svg>"),
            ImageFormat::Svg
        );
    }

    #[test]
    fn sniffs_webp() {
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 rest"), ImageFormat::Webp);
    }

    #[test]
    fn sniffs_pdf() {
        assert_eq!(sniff(b"%PDF-1.7\nrest"), ImageFormat::Pdf);
    }

    #[test]
    fn sniffs_bmp() {
        assert_eq!(sniff(b"BMrestofheader"), ImageFormat::Bmp);
    }

    #[test]
    fn sniffs_tiff() {
        assert_eq!(sniff(b"II*\0restofheader"), ImageFormat::Tiff);
        assert_eq!(sniff(b"MM\0*restofheader"), ImageFormat::Tiff);
    }

    #[test]
    fn sniffs_avif() {
        assert_eq!(sniff(b"\0\0\0\x18ftypavifrest"), ImageFormat::Avif);
    }

    #[test]
    fn sniffs_heic() {
        assert_eq!(sniff(b"\0\0\0\x18ftypheicrest"), ImageFormat::Heic);
    }

    #[test]
    fn unknown_blob_is_unknown() {
        assert_eq!(sniff(b"not an image at all"), ImageFormat::Unknown);
        assert_eq!(sniff(b""), ImageFormat::Unknown);
    }

    #[test]
    fn webp_unsupported_on_docx_and_pptx_only() {
        assert!(unsupported_targets(ImageFormat::Webp).contains(&Target::Docx));
        assert!(unsupported_targets(ImageFormat::Webp).contains(&Target::Pptx));
        assert!(!unsupported_targets(ImageFormat::Webp).contains(&Target::Pdf));
        assert!(!unsupported_targets(ImageFormat::Webp).contains(&Target::Odt));
        assert!(!unsupported_targets(ImageFormat::Webp).contains(&Target::HtmlReveal));
    }

    #[test]
    fn bmp_and_tiff_unsupported_on_typst_targets_only() {
        for format in [ImageFormat::Bmp, ImageFormat::Tiff] {
            assert!(unsupported_targets(format).contains(&Target::Pdf));
            assert!(unsupported_targets(format).contains(&Target::PdfPresentation));
            assert!(!unsupported_targets(format).contains(&Target::Docx));
            assert!(!unsupported_targets(format).contains(&Target::Odt));
            assert!(!unsupported_targets(format).contains(&Target::Pptx));
        }
    }

    #[test]
    fn common_formats_unsupported_nowhere() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::Svg,
        ] {
            assert!(unsupported_targets(format).is_empty());
        }
    }
}
