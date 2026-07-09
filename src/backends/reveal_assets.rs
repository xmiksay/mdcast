//! Materialise html-reveal's provider-sourced pieces — the reveal.js dist
//! subtree and the brand include files (issue #57) — into pandoc's owned
//! per-render temp root. Split out of `pandoc.rs` to keep it under the
//! 400-line cap. The `tokio::fs` writes here are the documented exception to
//! the no-`std::fs`-in-backends seam: everything written lands inside the
//! temp dir `PandocBackend::render_to_bytes` owns and cleans up; all *reads*
//! still go through the `AssetProvider`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use futures::future::try_join_all;

use super::reveal_brand;
use crate::{AssetProvider, BrandSpec};

/// Pull every key beginning with `prefix` from the provider and write the bytes
/// to `dest/<key-without-prefix>`. Returns the number of files materialised.
/// Used to lay out the reveal.js dist (and could later cover MathJax, etc.).
pub(super) async fn materialise_subtree(
    provider: &dyn AssetProvider,
    prefix: &str,
    dest: &Path,
) -> Result<usize> {
    let keys = provider.list(prefix).await?;
    let written = try_join_all(keys.iter().map(|key| async move {
        let Some(bytes) = provider.get(key).await? else {
            return Ok(0usize);
        };
        let rel = key.strip_prefix(prefix).unwrap_or(key);
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &bytes).await?;
        Ok::<usize, anyhow::Error>(1)
    }))
    .await?;
    Ok(written.into_iter().sum())
}

/// Write the brand CSS layer (issue #57) to `<root>/brand.css.html` — a
/// `<style data-brand>` block combining `reveal_brand::brand_css`'s
/// palette/font projection with the raw contents of the provider's
/// `revealjs/brand.css` escape hatch, if present — and return its path.
/// `None` when neither source has anything to say, so an unbranded doc adds
/// no pandoc arg and no temp file.
pub(super) async fn brand_style_file(
    root: &Path,
    brand: &BrandSpec,
    assets: &dyn AssetProvider,
) -> Result<Option<PathBuf>> {
    let mut body = reveal_brand::brand_css(brand).unwrap_or_default();

    if let Some(bytes) = assets.get("revealjs/brand.css").await? {
        match std::str::from_utf8(&bytes) {
            Ok(raw) => body.push_str(raw),
            Err(_) => tracing::warn!("revealjs/brand.css is not valid UTF-8; skipping"),
        }
    }

    if body.is_empty() {
        return Ok(None);
    }

    let path = root.join("brand.css.html");
    tokio::fs::write(&path, format!("<style data-brand>\n{body}</style>\n")).await?;
    Ok(Some(path))
}

/// Fetch the brand logo (issue #57) via the provider and write its
/// `<img>` overlay to `<root>/brand-logo.html`, returning its path. `None`
/// when `brand.logo` is unset; a missing/unresolvable key warns and also
/// returns `None` rather than failing the render.
pub(super) async fn brand_logo_file(
    root: &Path,
    brand: &BrandSpec,
    assets: &dyn AssetProvider,
) -> Result<Option<PathBuf>> {
    let Some(logo) = &brand.logo else {
        return Ok(None);
    };
    let Some(bytes) = assets.get(&logo.key).await? else {
        tracing::warn!(
            key = logo.key.as_str(),
            "brand logo not found in provider; skipping"
        );
        return Ok(None);
    };
    let mime = crate::image_format::mime_type(&bytes);
    let html = reveal_brand::logo_html(logo, &bytes, mime);
    let path = root.join("brand-logo.html");
    tokio::fs::write(&path, html).await?;
    Ok(Some(path))
}
