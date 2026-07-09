//! Resolves the two sources of non-layout files the typst engine needs:
//! page-body images (scanned out of markdown by `images::collect_images`) and
//! declared `ResolvedDoc.assets` (layout chrome — logos, backgrounds — that
//! isn't referenced from any page body). Both funnel through
//! `register_virtual_files`, which turns a set of fetched `(key, bytes)`
//! pairs into the `(key → virtual-path, virtual-path → bytes)` shape
//! `backends/typst/mod.rs` registers with the engine, differing only in the
//! path prefix and in whether a miss is worth a warning. `fetch_deduped` is
//! also reused by `fonts.rs::collect_fonts`, which needs the same
//! dedup-then-fetch-through-the-provider step but skips virtual-file
//! registration entirely (font bytes go straight to the typst engine's font
//! book, not through `register_virtual_files`).

use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use bytes::Bytes;
use futures::future::try_join_all;

use crate::AssetRef;
use crate::Target;
use crate::assets::AssetProvider;
use crate::images::{collect_images, sanitize_key};
use crate::pages::Page;

/// `key → virtual-path` map (for the md→typst converter / `/context.typ`)
/// paired with the `virtual-path → bytes` list the engine's static file
/// resolver registers.
type VirtualFileSet = (BTreeMap<String, String>, Vec<(String, Vec<u8>)>);

/// Fetch each of `refs`' declared keys through the provider, deduped in
/// declaration order (a repeated key shouldn't cost a second round-trip, and
/// callers that care about ordering — e.g. font precedence on an exact tie —
/// need the first declared occurrence to stay first), all concurrently.
/// `bytes` is `None` for a key the provider has no data for; callers decide
/// what a miss means (warn-and-skip, silent skip, ...).
pub(super) async fn fetch_deduped(
    refs: &[AssetRef],
    provider: &dyn AssetProvider,
) -> Result<Vec<(String, Option<Bytes>)>> {
    let mut seen = HashSet::new();
    let keys: Vec<&str> = refs
        .iter()
        .map(|r| r.key.as_str())
        .filter(|key| seen.insert(*key))
        .collect();
    try_join_all(keys.into_iter().map(|key| async move {
        let bytes = provider.get(key).await?;
        Ok::<(String, Option<Bytes>), anyhow::Error>((key.to_string(), bytes))
    }))
    .await
}

/// Fold fetched `(key, bytes)` pairs into a `VirtualFileSet`. A key the
/// provider had no bytes for is dropped — `warn_missing` controls whether
/// that's worth logging: declared layout assets warn (a missing logo is a
/// real gap), page-body images don't (`collect_images` already leaves
/// unresolved refs untouched in the markdown, which is the intended
/// fallback).
fn register_virtual_files(
    fetched: Vec<(String, Option<Bytes>)>,
    prefix: &str,
    warn_missing: bool,
) -> VirtualFileSet {
    let mut map = BTreeMap::new();
    let mut files = Vec::new();
    for (key, bytes) in fetched {
        match bytes {
            // Register under `<prefix>/...` but emit `/<prefix>/...` — the
            // leading slash makes typst resolve relative to the project root
            // instead of the layout file's directory.
            Some(b) => {
                let vpath = format!("{prefix}/{}", sanitize_key(&key));
                map.insert(key, format!("/{vpath}"));
                files.push((vpath, b.to_vec()));
            }
            None if warn_missing => {
                tracing::warn!(key, "layout asset not found in provider; skipping");
            }
            None => {}
        }
    }
    (map, files)
}

/// Fetch every image reference via the shared `collect_images` pipeline and
/// produce (a) a `url → virtual_path` map for the md→typst converter, (b) the
/// bytes to register with the typst engine.
pub(super) async fn collect_images_for_typst(
    pages: &[Page],
    provider: &dyn AssetProvider,
    target: Target,
) -> Result<VirtualFileSet> {
    let fetched = collect_images(pages, provider, target).await?;
    let fetched = fetched.into_iter().map(|(k, b)| (k, Some(b))).collect();
    Ok(register_virtual_files(fetched, "images", false))
}

/// Fetch every `ResolvedDoc.assets` entry through the provider and register it
/// under a stable virtual path (`assets/<sanitized-key>`). Unlike page-body
/// images, these aren't scanned out of markdown: they're chrome (logos,
/// backgrounds) a layout reaches via `#import "/context.typ": asset-path` and
/// looks up by its own declared key. Duplicate keys are deduped before
/// fetching — a repeated `--layout-asset KEY` shouldn't cost a second
/// round-trip. A key the provider doesn't have warns and is skipped rather
/// than failing the whole render — a missing logo shouldn't break the PDF.
pub(super) async fn collect_layout_assets(
    refs: &[AssetRef],
    provider: &dyn AssetProvider,
) -> Result<VirtualFileSet> {
    let fetched = fetch_deduped(refs, provider).await?;
    Ok(register_virtual_files(fetched, "assets", true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::sync_provider;

    #[tokio::test]
    async fn collect_layout_assets_registers_found_keys_under_stable_vpath() {
        let provider = sync_provider(|key| match key {
            "branding/logo.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
            _ => Ok(None),
        });
        let refs = vec![AssetRef {
            key: "branding/logo.svg".to_string(),
        }];

        let (map, files) = collect_layout_assets(&refs, &provider).await.unwrap();

        assert_eq!(
            map.get("branding/logo.svg"),
            Some(&"/assets/branding__logo.svg".to_string())
        );
        assert_eq!(
            files,
            vec![("assets/branding__logo.svg".to_string(), b"<svg/>".to_vec())]
        );
    }

    #[tokio::test]
    async fn collect_layout_assets_skips_missing_keys_without_erroring() {
        let provider = sync_provider(|_| Ok(None));
        let refs = vec![AssetRef {
            key: "does/not-exist.png".to_string(),
        }];

        let (map, files) = collect_layout_assets(&refs, &provider).await.unwrap();

        assert!(map.is_empty());
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn collect_layout_assets_dedups_repeated_keys() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = calls.clone();
        let provider = sync_provider(move |key| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            match key {
                "logo.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
                _ => Ok(None),
            }
        });
        let refs = vec![
            AssetRef {
                key: "logo.svg".to_string(),
            },
            AssetRef {
                key: "logo.svg".to_string(),
            },
        ];

        let (map, files) = collect_layout_assets(&refs, &provider).await.unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "expected a single fetch for a repeated key"
        );
        assert_eq!(map.len(), 1);
        assert_eq!(files.len(), 1);
    }
}
