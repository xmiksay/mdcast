//! The single integration point between the blob cache and the render
//! pipeline: an [`AssetProvider`] whose key namespace is one request's
//! [`AssetManifest`].
//!
//! A render never sees digests. It asks for `img/logo.png`, the manifest turns
//! that into a digest, and the digest into bytes — which is why a caller can
//! keep using mdcast's ordinary asset keys (`reference/reference.docx`,
//! `typst/layouts/pdf/hero.typ`, plain image paths) while the wire protocol
//! ships content, not names.
//!
//! Resolution happens up front, in [`BlobAssets::resolve`]: the *whole*
//! manifest is checked against the store before any rendering starts, so a
//! caller learns about every missing blob in one `409` instead of discovering
//! them one failed render at a time.

use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use mdcast::mdcast_api::AssetManifest;
use mdcast::mdcast_api::wire::MissingBlob;
use mdcast::{AssetProvider, BoxFuture};

use super::store::BlobStore;

pub struct BlobAssets {
    store: Arc<BlobStore>,
    manifest: AssetManifest,
}

impl BlobAssets {
    /// Bind `manifest` to the store, or report **every** entry the store
    /// cannot satisfy.
    pub async fn resolve(
        store: Arc<BlobStore>,
        manifest: AssetManifest,
    ) -> Result<Self, Vec<MissingBlob>> {
        let mut missing = Vec::new();
        for (key, digest) in manifest.entries() {
            if !store.contains(digest).await {
                missing.push(MissingBlob {
                    key: key.to_string(),
                    digest: digest.clone(),
                });
            }
        }
        if missing.is_empty() {
            Ok(Self { store, manifest })
        } else {
            Err(missing)
        }
    }
}

impl AssetProvider for BlobAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        Box::pin(async move {
            let Some(digest) = self.manifest.get(key) else {
                return Ok(None); // not declared: fall through to the layer below
            };
            self.store.get(digest).await
        })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        let keys: Vec<String> = self
            .manifest
            .entries()
            .map(|(key, _)| key)
            .filter(|key| key.starts_with(prefix))
            .map(str::to_string)
            .collect();
        Box::pin(async move { Ok(keys) })
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::sha256;
    use super::*;

    async fn store(dir: &tempfile::TempDir) -> Arc<BlobStore> {
        Arc::new(
            BlobStore::open(dir.path().to_path_buf(), 1 << 20)
                .await
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn resolve_reports_every_missing_entry_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let present = sha256(b"present");
        store.put(&present, b"present").await.unwrap();

        let manifest = AssetManifest::from_iter([
            ("img/here.png".to_string(), present),
            ("img/gone.png".to_string(), sha256(b"gone")),
            ("img/also-gone.png".to_string(), sha256(b"also gone")),
        ]);

        let missing = BlobAssets::resolve(store, manifest).await.err().unwrap();

        let keys: Vec<&str> = missing.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["img/also-gone.png", "img/gone.png"],
            "one 409 must name every missing blob, not just the first"
        );
    }

    #[tokio::test]
    async fn a_resolved_manifest_serves_its_keys_and_only_its_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let digest = sha256(b"<svg/>");
        store.put(&digest, b"<svg/>").await.unwrap();
        let manifest = AssetManifest::from_iter([("img/logo.svg".to_string(), digest)]);

        let assets = BlobAssets::resolve(store, manifest).await.ok().unwrap();

        assert_eq!(
            assets.get("img/logo.svg").await.unwrap().unwrap(),
            &b"<svg/>"[..]
        );
        assert_eq!(
            assets.get("reference/reference.docx").await.unwrap(),
            None,
            "an undeclared key must fall through to the layer below"
        );
    }

    #[tokio::test]
    async fn list_is_the_manifest_filtered_by_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let digest = sha256(b"x");
        store.put(&digest, b"x").await.unwrap();
        let manifest = AssetManifest::from_iter([
            ("partials/header.typ".to_string(), digest.clone()),
            ("logo.svg".to_string(), digest),
        ]);
        let assets = BlobAssets::resolve(store, manifest).await.ok().unwrap();

        assert_eq!(
            assets.list("partials/").await.unwrap(),
            vec!["partials/header.typ"]
        );
        let mut all = assets.list("").await.unwrap();
        all.sort();
        assert_eq!(all, vec!["logo.svg", "partials/header.typ"]);
    }

    /// A blob evicted between the manifest check and the render reads back as
    /// `None`, not an error — the render then degrades exactly as it would for
    /// an asset key nobody ever supplied.
    #[tokio::test]
    async fn an_evicted_blob_degrades_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let digest = sha256(b"doomed");
        store.put(&digest, b"doomed").await.unwrap();
        let manifest = AssetManifest::from_iter([("img/x.png".to_string(), digest.clone())]);
        let assets = BlobAssets::resolve(store.clone(), manifest)
            .await
            .ok()
            .unwrap();

        std::fs::remove_file(store.path_for(&digest)).unwrap();

        assert_eq!(assets.get("img/x.png").await.unwrap(), None);
    }
}
