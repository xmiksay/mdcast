//! QR codes (`qr` feature): ```` ```qr ```` fences / `<qr>` tags in markdown,
//! and `GeneratedSpec::Qr` entries on the template path.
//!
//! Attributes: `ec="L|M|Q|H"` (default `M`) and `size="small|large"`. The SVG
//! is hand-built from the module matrix — one rect per dark run plus a white
//! background with the spec's 4-module quiet zone — so `qrcode` is used with
//! its default features off and pulls in nothing else.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use mdcast_api::wire::QrEcLevel;
use qrcode::{Color, EcLevel, QrCode};

use super::{Attrs, Generated, Size, expand};
use crate::AssetProvider;

/// Asset-key prefix: `qr/qr-N.svg`, numbered in document order.
const KEY_PREFIX: &str = "qr/qr";

/// Intrinsic edge length at `Size::Normal` — large enough to scan off paper
/// with the quiet zone included.
const BASE_CM: f32 = 4.0;

/// Quiet zone width in modules, as the QR spec requires.
const QUIET: usize = 4;

/// Replace every `qr` fence and `<qr>` tag in `markdown` with an image
/// reference to its rendered SVG.
pub async fn render_qr_codes(markdown: &str, assets: &dyn AssetProvider) -> Generated {
    expand(markdown, "qr", KEY_PREFIX, assets, |data, attrs| {
        svg(data, ec_from_attrs(attrs)?, Size::from_attrs(attrs)?)
    })
    .await
}

fn ec_from_attrs(attrs: &Attrs) -> Result<QrEcLevel> {
    match attrs.get("ec").map(|s| s.to_ascii_uppercase()).as_deref() {
        None | Some("M") => Ok(QrEcLevel::M),
        Some("L") => Ok(QrEcLevel::L),
        Some("Q") => Ok(QrEcLevel::Q),
        Some("H") => Ok(QrEcLevel::H),
        Some(other) => bail!("unknown ec {other:?} (expected L, M, Q or H)"),
    }
}

/// Encode `data` as a QR code SVG.
pub(crate) fn svg(data: &str, ec: QrEcLevel, size: Size) -> Result<String> {
    if data.is_empty() {
        bail!("QR code has no content");
    }
    let ec = match ec {
        QrEcLevel::L => EcLevel::L,
        QrEcLevel::M => EcLevel::M,
        QrEcLevel::Q => EcLevel::Q,
        QrEcLevel::H => EcLevel::H,
    };
    let code = QrCode::with_error_correction_level(data.as_bytes(), ec)
        .map_err(|e| anyhow::anyhow!("cannot encode QR code: {e}"))?;
    let width = code.width();
    let colors = code.to_colors();
    let side = width + 2 * QUIET;
    let cm = size.scale(BASE_CM);

    let mut path = String::new();
    for (y, row) in colors.chunks(width).enumerate() {
        let mut x = 0;
        while x < width {
            if row[x] == Color::Dark {
                let run = row[x..].iter().take_while(|c| **c == Color::Dark).count();
                let _ = write!(path, "M{} {}h{run}v1h-{run}z", x + QUIET, y + QUIET);
                x += run;
            } else {
                x += 1;
            }
        }
    }
    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{cm}cm" height="{cm}cm" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><rect width="{side}" height="{side}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    ))
}
