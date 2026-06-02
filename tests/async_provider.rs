//! Proves the AssetProvider boundary is genuinely async: a provider that
//! defers via `tokio::time::sleep` must still complete a render correctly.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use mdcast::{AssetProvider, EmbeddedAssets, LayeredAssets, async_provider};

#[tokio::test]
async fn async_callback_resolves_after_await() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();

    let provider = async_provider(move |key: String| {
        let calls = calls_clone.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            calls.fetch_add(1, Ordering::SeqCst);
            if key == "user/cover.png" {
                Ok(Some(Bytes::from_static(b"PNGBYTES")))
            } else {
                Ok(None)
            }
        }
    });

    let layered = LayeredAssets { over: provider, base: EmbeddedAssets };

    let got = layered.get("user/cover.png").await.unwrap();
    assert_eq!(got, Some(Bytes::from_static(b"PNGBYTES")));

    let fallback = layered.get("typst/layouts/pdf/hero.typ").await.unwrap();
    assert!(fallback.is_some(), "embedded fallback should resolve");

    assert!(calls.load(Ordering::SeqCst) >= 1);
}
