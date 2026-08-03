//! Asset manifest: the flat asset-key namespace mapped to content digests.
//!
//! Keys are exactly the `AssetProvider` keys mdcast already uses
//! (`img/logo.png`, `typst/layouts/pdf/hero.typ`, `reference/reference.docx`,
//! …) — a manifest says "resolve these keys to these bytes for this render",
//! without shipping the bytes. The server answers with the digests it is
//! missing, the client uploads only those, and the render is retried.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::digest::Digest;

/// Asset key → digest of its bytes. Serializes as a plain JSON object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetManifest(BTreeMap<String, Digest>);

impl AssetManifest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare `key`, returning the digest it previously mapped to.
    pub fn insert(&mut self, key: impl Into<String>, digest: Digest) -> Option<Digest> {
        self.0.insert(key.into(), digest)
    }

    pub fn get(&self, key: &str) -> Option<&Digest> {
        self.0.get(key)
    }

    /// Every `(key, digest)` pair, in key order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &Digest)> {
        self.0.iter().map(|(k, d)| (k.as_str(), d))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(String, Digest)> for AssetManifest {
    fn from_iter<I: IntoIterator<Item = (String, Digest)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Digest {
        Digest::from_sha256([byte; 32])
    }

    #[test]
    fn insert_returns_the_replaced_digest() {
        let mut manifest = AssetManifest::new();

        assert_eq!(manifest.insert("img/logo.png", digest(1)), None);
        assert_eq!(
            manifest.insert("img/logo.png", digest(2)),
            Some(digest(1)),
            "re-declaring a key reports what it used to point at"
        );
        assert_eq!(manifest.get("img/logo.png"), Some(&digest(2)));
        assert_eq!(manifest.len(), 1);
    }

    #[test]
    fn empty_manifest_is_empty() {
        let manifest = AssetManifest::new();
        assert!(manifest.is_empty());
        assert_eq!(manifest.get("img/logo.png"), None);
    }

    #[test]
    fn entries_come_back_in_key_order() {
        let manifest = AssetManifest::from_iter([
            ("typst/layouts/pdf/hero.typ".to_string(), digest(3)),
            ("img/logo.png".to_string(), digest(1)),
            ("reference/reference.docx".to_string(), digest(2)),
        ]);

        let keys: Vec<&str> = manifest.entries().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            vec![
                "img/logo.png",
                "reference/reference.docx",
                "typst/layouts/pdf/hero.typ"
            ]
        );
    }

    #[test]
    fn serializes_as_a_flat_object_of_key_to_digest() {
        let manifest = AssetManifest::from_iter([("img/logo.png".to_string(), digest(0))]);

        let json = serde_json::to_string(&manifest).unwrap();
        assert_eq!(
            json,
            format!("{{\"img/logo.png\":\"{}\"}}", digest(0).as_str())
        );
        assert_eq!(
            serde_json::from_str::<AssetManifest>(&json).unwrap(),
            manifest
        );
    }

    #[test]
    fn deserialization_rejects_an_invalid_digest() {
        let err = serde_json::from_str::<AssetManifest>(r#"{"img/logo.png":"nope"}"#).unwrap_err();
        assert!(err.to_string().contains("64 hex characters"), "{err}");
    }
}
