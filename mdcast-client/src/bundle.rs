//! The caller-facing half of asset negotiation: a bag of `key → bytes` that
//! knows how to hash itself.
//!
//! Nobody using this crate should ever compute a sha256 or hand-build an
//! [`AssetManifest`] — that is the whole point. Fill an [`AssetBundle`] with
//! the same flat `AssetProvider` keys mdcast uses everywhere else
//! (`img/logo.png`, `typst/layouts/pdf/hero.typ`, `reference/reference.docx`),
//! hand it to a render call, and the digests, the manifest, and the upload of
//! whatever the server is missing all follow from it.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use mdcast_api::{AssetManifest, Digest};
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};

/// The sha256 of `bytes`, in the form the wire protocol names blobs by.
pub fn sha256(bytes: &[u8]) -> Digest {
    Digest::from_sha256(Sha256::digest(bytes).into())
}

/// The error a digest-only entry's `fetch` closure failed with. Opaque on
/// purpose — the caller's storage layer (a DB, a filesystem, an object store)
/// is not this crate's concern, only that the failure is reported rather than
/// swallowed.
pub type BlobFetchError = Box<dyn std::error::Error + Send + Sync>;

type FetchFuture = Pin<Box<dyn Future<Output = std::result::Result<Bytes, BlobFetchError>> + Send>>;

/// Type-erased so entries backed by different closures can share one map.
/// `Fn`, not `FnOnce`: a fetch may run again if the caller reuses the bundle
/// across renders, though within a single render it runs at most once — only
/// if the server's `409` actually names this digest.
type Fetch = Arc<dyn Fn() -> FetchFuture + Send + Sync>;

/// One bundle entry: bytes already in hand, or a digest with bytes fetched
/// lazily on demand.
#[derive(Clone)]
enum Entry {
    Bytes(Bytes),
    Lazy { digest: Digest, fetch: Fetch },
}

impl Entry {
    /// The digest this entry is known by — computed on the spot for eager
    /// bytes, already known (no fetch) for a lazy entry.
    fn digest(&self) -> Digest {
        match self {
            Entry::Bytes(bytes) => sha256(bytes),
            Entry::Lazy { digest, .. } => digest.clone(),
        }
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Entry::Bytes(bytes) => f.debug_tuple("Bytes").field(bytes).finish(),
            Entry::Lazy { digest, .. } => f
                .debug_struct("Lazy")
                .field("digest", digest)
                .finish_non_exhaustive(),
        }
    }
}

/// Asset key → bytes (or a digest with bytes fetched lazily), for one render.
#[derive(Debug, Clone, Default)]
pub struct AssetBundle {
    assets: BTreeMap<String, Entry>,
}

impl AssetBundle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare `key`, returning the bytes it previously held (a previously
    /// digest-only entry reports `None` — there were no bytes in hand).
    pub fn insert(&mut self, key: impl Into<String>, bytes: impl Into<Bytes>) -> Option<Bytes> {
        match self.assets.insert(key.into(), Entry::Bytes(bytes.into())) {
            Some(Entry::Bytes(bytes)) => Some(bytes),
            _ => None,
        }
    }

    /// [`AssetBundle::insert`] for chaining at a call site.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, bytes: impl Into<Bytes>) -> Self {
        self.insert(key, bytes);
        self
    }

    /// Declare `key` by digest only: `manifest()`/`bind()` use `digest`
    /// without ever calling `fetch`, which only runs if the server's `409`
    /// actually names this digest — the steady-state case for a caller whose
    /// storage is already content-addressed never touches the bytes at all.
    /// `fetch` may be called again on a retry of the same bundle, so it
    /// should be idempotent; a fetch that fails, or whose bytes hash to
    /// something other than `digest`, surfaces as
    /// [`Error::BlobFetchFailed`](crate::Error::BlobFetchFailed) or
    /// [`Error::DigestMismatch`](crate::Error::DigestMismatch) when the
    /// upload it was needed for runs — never a silent skip.
    pub fn insert_digest<F, Fut, E>(
        &mut self,
        key: impl Into<String>,
        digest: Digest,
        fetch: F,
    ) -> Option<Bytes>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<Bytes, E>> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let fetch: Fetch = Arc::new(move || {
            let fut = fetch();
            Box::pin(async move { fut.await.map_err(|e| Box::new(e) as BlobFetchError) })
        });
        match self
            .assets
            .insert(key.into(), Entry::Lazy { digest, fetch })
        {
            Some(Entry::Bytes(bytes)) => Some(bytes),
            _ => None,
        }
    }

    /// [`AssetBundle::insert_digest`] for chaining at a call site.
    #[must_use]
    pub fn with_digest<F, Fut, E>(
        mut self,
        key: impl Into<String>,
        digest: Digest,
        fetch: F,
    ) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<Bytes, E>> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.insert_digest(key, digest, fetch);
        self
    }

    /// The bytes `key` holds, if it was declared eagerly. `None` both for an
    /// absent key and for one declared with [`AssetBundle::with_digest`] —
    /// use [`Client::check`](crate::Client::check) / render to reach a
    /// digest-only entry's bytes, since fetching them is async.
    pub fn get(&self, key: &str) -> Option<&Bytes> {
        match self.assets.get(key)? {
            Entry::Bytes(bytes) => Some(bytes),
            Entry::Lazy { .. } => None,
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.assets.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// The manifest naming every asset in the bundle by digest. Never
    /// touches a digest-only entry's bytes.
    pub fn manifest(&self) -> AssetManifest {
        self.digests()
            .into_iter()
            .map(|(key, digest)| (key.to_string(), digest))
            .collect()
    }

    /// Every asset's key and digest — cheap, and safe for a digest-only entry
    /// since it never calls `fetch`.
    pub(crate) fn digests(&self) -> Vec<(&str, Digest)> {
        self.assets
            .iter()
            .map(|(key, entry)| (key.as_str(), entry.digest()))
            .collect()
    }

    /// The bytes named by `key`, verified against `expected`. Eager bytes are
    /// just cloned (as before, untrusted against `expected` — the bundle is
    /// the source of truth for its own bytes). A digest-only entry's `fetch`
    /// runs here: a failure becomes [`Error::BlobFetchFailed`], and bytes
    /// that hash to something other than `expected` become
    /// [`Error::DigestMismatch`] — never silently skipped, since the caller's
    /// next attempt would just `409` on the same digest again.
    pub(crate) async fn resolve(&self, key: &str, expected: &Digest) -> Result<Bytes> {
        match self.assets.get(key) {
            None => Err(Error::MissingAsset {
                key: key.to_string(),
            }),
            Some(Entry::Bytes(bytes)) => Ok(bytes.clone()),
            Some(Entry::Lazy { fetch, .. }) => {
                let bytes = fetch().await.map_err(|source| Error::BlobFetchFailed {
                    key: key.to_string(),
                    source,
                })?;
                let actual = sha256(&bytes);
                if &actual != expected {
                    return Err(Error::DigestMismatch {
                        key: key.to_string(),
                        declared: expected.clone(),
                        actual,
                    });
                }
                Ok(bytes)
            }
        }
    }

    /// The manifest a render request will actually carry.
    ///
    /// An empty `declared` (the common case — the caller never touched the
    /// field) is filled from the bundle. A caller that did build its own is
    /// verified against the bundle instead of trusted: a key with no bytes, or
    /// a digest that disagrees with those bytes, fails here rather than after
    /// a round trip, because a `409` can only name a digest and would leave the
    /// caller guessing which of its assets went wrong. Only touches digests —
    /// a digest-only entry's bytes are never fetched for this check.
    pub(crate) fn bind(&self, declared: AssetManifest) -> Result<AssetManifest> {
        let computed = self.manifest();
        if declared.is_empty() {
            return Ok(computed);
        }
        for (key, digest) in declared.entries() {
            match computed.get(key) {
                None => {
                    return Err(Error::MissingAsset {
                        key: key.to_string(),
                    });
                }
                Some(actual) if actual != digest => {
                    return Err(Error::DigestMismatch {
                        key: key.to_string(),
                        declared: digest.clone(),
                        actual: actual.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(declared)
    }
}

impl<K: Into<String>, V: Into<Bytes>> FromIterator<(K, V)> for AssetBundle {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            assets: iter
                .into_iter()
                .map(|(k, v)| (k.into(), Entry::Bytes(v.into())))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published sha256 of the empty input and of "hello" — a check that
    /// this hashes what it claims to, not merely something stable.
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn bundle() -> AssetBundle {
        AssetBundle::new()
            .with("img/logo.png", &b"hello"[..])
            .with("reference/reference.docx", &b""[..])
    }

    #[test]
    fn sha256_matches_the_known_vectors() {
        assert_eq!(sha256(b"").as_str(), EMPTY);
        assert_eq!(sha256(b"hello").as_str(), HELLO);
    }

    #[test]
    fn manifest_names_every_asset_by_its_digest() {
        let manifest = bundle().manifest();

        assert_eq!(manifest.len(), 2);
        assert_eq!(
            manifest.get("img/logo.png").map(Digest::as_str),
            Some(HELLO)
        );
        assert_eq!(
            manifest.get("reference/reference.docx").map(Digest::as_str),
            Some(EMPTY)
        );
    }

    #[test]
    fn an_empty_bundle_produces_an_empty_manifest() {
        let empty = AssetBundle::new();
        assert!(empty.is_empty());
        assert!(empty.manifest().is_empty());
    }

    #[test]
    fn bind_fills_an_undeclared_manifest_from_the_bundle() {
        assert_eq!(
            bundle().bind(AssetManifest::new()).unwrap(),
            bundle().manifest()
        );
    }

    #[test]
    fn bind_keeps_a_declared_manifest_that_the_bundle_satisfies() {
        let declared = AssetManifest::from_iter([("img/logo.png".to_string(), sha256(b"hello"))]);

        let bound = bundle().bind(declared.clone()).unwrap();

        assert_eq!(bound, declared, "a partial manifest is a deliberate choice");
        assert_eq!(bound.len(), 1);
    }

    #[test]
    fn bind_rejects_a_key_the_bundle_has_no_bytes_for() {
        let declared = AssetManifest::from_iter([("img/absent.png".to_string(), sha256(b"x"))]);

        match bundle().bind(declared) {
            Err(Error::MissingAsset { key }) => assert_eq!(key, "img/absent.png"),
            other => panic!("expected MissingAsset, got {other:?}"),
        }
    }

    #[test]
    fn bind_rejects_a_digest_that_disagrees_with_the_bytes() {
        let declared =
            AssetManifest::from_iter([("img/logo.png".to_string(), sha256(b"something else"))]);

        match bundle().bind(declared) {
            Err(Error::DigestMismatch { key, actual, .. }) => {
                assert_eq!(key, "img/logo.png");
                assert_eq!(actual.as_str(), HELLO);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
    }

    #[test]
    fn insert_replaces_and_reports_the_previous_bytes() {
        let mut bundle = AssetBundle::new();

        assert_eq!(bundle.insert("k", &b"one"[..]), None);
        assert_eq!(bundle.insert("k", &b"two"[..]), Some(Bytes::from("one")));
        assert_eq!(bundle.get("k").map(|b| &b[..]), Some(&b"two"[..]));
        assert_eq!(bundle.len(), 1);
    }

    #[test]
    fn collects_from_pairs_and_reports_its_keys_in_order() {
        let bundle = AssetBundle::from_iter([("b/2.png", "two"), ("a/1.png", "one")]);

        assert_eq!(
            bundle.keys().collect::<Vec<_>>(),
            vec!["a/1.png", "b/2.png"]
        );
    }

    #[derive(Debug, thiserror::Error)]
    #[error("boom")]
    struct Boom;

    fn digest_only_bundle(calls: Arc<std::sync::atomic::AtomicUsize>) -> AssetBundle {
        AssetBundle::new()
            .with("img/logo.png", &b"hello"[..])
            .with_digest("img/photo.jpg", sha256(b"photo"), move || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<Bytes, Boom>(Bytes::from_static(b"photo"))
                }
            })
    }

    #[test]
    fn manifest_and_bind_never_call_a_digest_only_entrys_fetch() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bundle = digest_only_bundle(Arc::clone(&calls));

        let manifest = bundle.manifest();
        bundle.bind(AssetManifest::new()).unwrap();

        assert_eq!(
            manifest.get("img/photo.jpg").map(Digest::as_str),
            Some(sha256(b"photo").as_str())
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "manifest/bind must never fetch a digest-only entry's bytes"
        );
    }

    #[test]
    fn get_reports_none_for_a_digest_only_entry() {
        let bundle = digest_only_bundle(Arc::new(std::sync::atomic::AtomicUsize::new(0)));

        assert_eq!(bundle.get("img/photo.jpg"), None);
        assert_eq!(
            bundle.get("img/logo.png").map(|b| &b[..]),
            Some(&b"hello"[..])
        );
    }

    #[tokio::test]
    async fn resolve_fetches_and_verifies_a_digest_only_entry() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bundle = digest_only_bundle(Arc::clone(&calls));

        let bytes = bundle
            .resolve("img/photo.jpg", &sha256(b"photo"))
            .await
            .unwrap();

        assert_eq!(&bytes[..], b"photo");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_reports_a_failed_fetch_as_blob_fetch_failed() {
        let bundle = AssetBundle::new().with_digest("img/photo.jpg", sha256(b"photo"), || async {
            Err::<Bytes, Boom>(Boom)
        });

        match bundle.resolve("img/photo.jpg", &sha256(b"photo")).await {
            Err(Error::BlobFetchFailed { key, .. }) => assert_eq!(key, "img/photo.jpg"),
            other => panic!("expected BlobFetchFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_reports_bytes_that_hash_to_the_wrong_digest_as_a_mismatch() {
        let bundle = AssetBundle::new().with_digest("img/photo.jpg", sha256(b"photo"), || async {
            Ok::<Bytes, Boom>(Bytes::from_static(b"not the declared photo"))
        });

        match bundle.resolve("img/photo.jpg", &sha256(b"photo")).await {
            Err(Error::DigestMismatch {
                key,
                declared,
                actual,
            }) => {
                assert_eq!(key, "img/photo.jpg");
                assert_eq!(declared, sha256(b"photo"));
                assert_eq!(actual, sha256(b"not the declared photo"));
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
    }

    #[test]
    fn with_digest_replacing_an_eager_entry_reports_its_previous_bytes() {
        let mut bundle = AssetBundle::new();
        bundle.insert("k", &b"one"[..]);

        let previous = bundle.insert_digest("k", sha256(b"two"), || async {
            Ok::<Bytes, Boom>(Bytes::new())
        });

        assert_eq!(previous, Some(Bytes::from("one")));
    }
}
