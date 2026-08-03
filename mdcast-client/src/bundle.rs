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

use bytes::Bytes;
use mdcast_api::{AssetManifest, Digest};
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};

/// The sha256 of `bytes`, in the form the wire protocol names blobs by.
pub fn sha256(bytes: &[u8]) -> Digest {
    Digest::from_sha256(Sha256::digest(bytes).into())
}

/// Asset key → bytes, for one render.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetBundle {
    assets: BTreeMap<String, Bytes>,
}

impl AssetBundle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare `key`, returning the bytes it previously held.
    pub fn insert(&mut self, key: impl Into<String>, bytes: impl Into<Bytes>) -> Option<Bytes> {
        self.assets.insert(key.into(), bytes.into())
    }

    /// [`AssetBundle::insert`] for chaining at a call site.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, bytes: impl Into<Bytes>) -> Self {
        self.insert(key, bytes);
        self
    }

    pub fn get(&self, key: &str) -> Option<&Bytes> {
        self.assets.get(key)
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

    /// The manifest naming every asset in the bundle by digest.
    pub fn manifest(&self) -> AssetManifest {
        self.hashed()
            .into_iter()
            .map(|(key, digest, _)| (key.to_string(), digest))
            .collect()
    }

    /// Every asset as `(key, digest, bytes)` — one hashing pass shared by the
    /// manifest, the completeness check, and upload-set selection.
    pub(crate) fn hashed(&self) -> Vec<(&str, Digest, &Bytes)> {
        self.assets
            .iter()
            .map(|(key, bytes)| (key.as_str(), sha256(bytes), bytes))
            .collect()
    }

    /// The manifest a render request will actually carry.
    ///
    /// An empty `declared` (the common case — the caller never touched the
    /// field) is filled from the bundle. A caller that did build its own is
    /// verified against the bundle instead of trusted: a key with no bytes, or
    /// a digest that disagrees with those bytes, fails here rather than after
    /// a round trip, because a `409` can only name a digest and would leave the
    /// caller guessing which of its assets went wrong.
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
                .map(|(k, v)| (k.into(), v.into()))
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
}
