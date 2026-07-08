//! Resolves `ResolvedDoc.fonts` — brand font faces a consumer wants the typst
//! font book to know about, fetched through the `AssetProvider` rather than a
//! host install. Unlike `virtual_files::collect_layout_assets`, resolved bytes
//! aren't registered as virtual files: they're handed straight to
//! `TypstEngine::builder().fonts(...)`, which parses each blob into one or
//! more `typst::text::Font` faces itself.

use anyhow::Result;

use crate::AssetRef;
use crate::assets::AssetProvider;

use super::virtual_files::fetch_deduped;

/// Fetch every declared font key through the provider, deduped in
/// declaration order, in parallel. Order matters here (unlike
/// `collect_layout_assets`, which only ever looks fetched bytes up by key):
/// `TypstEngine::builder().fonts(...)` pushes faces into the font book in
/// `Vec` order, and an exact tie between two registered faces keeps the
/// first-inserted one — so a caller relying on that documented precedence
/// needs its declaration order preserved, not re-sorted by key.
///
/// A key the provider has no bytes for warns and is dropped — a missing
/// brand font degrades to host/embedded font search rather than failing the
/// render, matching `collect_layout_assets`'s treatment of a missing logo.
pub(super) async fn collect_fonts(
    refs: &[AssetRef],
    provider: &dyn AssetProvider,
) -> Result<Vec<Vec<u8>>> {
    let fetched = fetch_deduped(refs, provider).await?;

    let mut fonts = Vec::new();
    for (key, bytes) in fetched {
        match bytes {
            Some(b) => fonts.push(b.to_vec()),
            None => tracing::warn!(key, "brand font not found in provider; skipping"),
        }
    }
    Ok(fonts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::sync_provider;
    use bytes::Bytes;

    #[tokio::test]
    async fn collect_fonts_returns_bytes_for_found_keys() {
        let provider = sync_provider(|key| match key {
            "fonts/Brand-Regular.ttf" => Ok(Some(Bytes::from_static(b"FAKE FONT BYTES"))),
            _ => Ok(None),
        });
        let refs = vec![AssetRef {
            key: "fonts/Brand-Regular.ttf".to_string(),
        }];

        let fonts = collect_fonts(&refs, &provider).await.unwrap();

        assert_eq!(fonts, vec![b"FAKE FONT BYTES".to_vec()]);
    }

    #[tokio::test]
    async fn collect_fonts_skips_missing_keys_without_erroring() {
        let provider = sync_provider(|_| Ok(None));
        let refs = vec![AssetRef {
            key: "fonts/does-not-exist.ttf".to_string(),
        }];

        let fonts = collect_fonts(&refs, &provider).await.unwrap();

        assert!(fonts.is_empty());
    }

    #[tokio::test]
    async fn collect_fonts_dedups_repeated_keys() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = calls.clone();
        let provider = sync_provider(move |key| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            match key {
                "fonts/Brand-Regular.ttf" => Ok(Some(Bytes::from_static(b"FONT"))),
                _ => Ok(None),
            }
        });
        let refs = vec![
            AssetRef {
                key: "fonts/Brand-Regular.ttf".to_string(),
            },
            AssetRef {
                key: "fonts/Brand-Regular.ttf".to_string(),
            },
        ];

        let fonts = collect_fonts(&refs, &provider).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "expected a single fetch");
        assert_eq!(fonts.len(), 1);
    }

    #[tokio::test]
    async fn collect_fonts_empty_refs_is_noop() {
        let provider = sync_provider(|_| Ok(None));
        let fonts = collect_fonts(&[], &provider).await.unwrap();
        assert!(fonts.is_empty());
    }

    /// `.fonts(...)` registers faces in `Vec` order, and an exact tie between
    /// two registered faces keeps the first-inserted one — so declaration
    /// order must survive the dedup-and-fetch step, not get re-sorted by key.
    #[tokio::test]
    async fn collect_fonts_preserves_declaration_order() {
        let provider = sync_provider(|key| match key {
            "fonts/zz-preferred.ttf" => Ok(Some(Bytes::from_static(b"PREFERRED"))),
            "fonts/aa-fallback.ttf" => Ok(Some(Bytes::from_static(b"FALLBACK"))),
            _ => Ok(None),
        });
        let refs = vec![
            AssetRef {
                key: "fonts/zz-preferred.ttf".to_string(),
            },
            AssetRef {
                key: "fonts/aa-fallback.ttf".to_string(),
            },
        ];

        let fonts = collect_fonts(&refs, &provider).await.unwrap();

        assert_eq!(
            fonts,
            vec![b"PREFERRED".to_vec(), b"FALLBACK".to_vec()],
            "a key-sorted dedup would return aa-fallback before zz-preferred"
        );
    }
}
