//! The blob half of the protocol: the "what are you missing?" probe, the
//! multipart upload that answers it, and the pessimistic combination of the
//! two.
//!
//! One `POST /v1/blobs` carries every blob it needs to, one part per blob, the
//! part *name* being the digest the server will re-hash the bytes against — so
//! a whole `409` is satisfied in a single request, never one round trip per
//! asset.

use bytes::Bytes;
use mdcast_api::Digest;
use mdcast_api::wire::{BlobCheckRequest, BlobCheckResponse, MissingBlob};
use reqwest::Body;
use reqwest::multipart::{Form, Part};

use crate::bundle::AssetBundle;
use crate::client::{BLOBS, BLOBS_CHECK, Client};
use crate::error::{Error, Result};

/// The parts one upload will carry: the claimed digest and the bytes behind it.
type UploadSet = Vec<(Digest, Bytes)>;

impl Client {
    /// Which of the bundle's assets the server does not hold yet. An empty
    /// result means a render with this bundle will not stall on a `409`.
    pub async fn check(&self, bundle: &AssetBundle) -> Result<Vec<Digest>> {
        let digests: Vec<Digest> = bundle
            .hashed()
            .into_iter()
            .map(|(_, digest, _)| digest)
            .collect();
        if digests.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .post_json(BLOBS_CHECK, &BlobCheckRequest { digests })
            .await?;
        let body: BlobCheckResponse = self.json_response(response).await?;
        Ok(body.missing)
    }

    /// Upload the named digests from `bundle`. A digest nothing in the bundle
    /// hashes to is [`Error::UnknownDigest`] — the request is never sent
    /// half-formed.
    pub async fn upload(&self, bundle: &AssetBundle, digests: &[Digest]) -> Result<()> {
        self.upload_set(select_by_digest(digests, bundle)?).await
    }

    /// The pessimistic strategy: ask what is missing, upload exactly that, and
    /// let the render that follows go straight through. Returns the digests it
    /// uploaded.
    pub async fn preflight(&self, bundle: &AssetBundle) -> Result<Vec<Digest>> {
        let missing = self.check(bundle).await?;
        self.upload(bundle, &missing).await?;
        Ok(missing)
    }

    /// Satisfy a `409`. The digests come from the manifest this client built,
    /// so they need no re-hashing — only the bytes have to be found again.
    pub(crate) async fn upload_missing(
        &self,
        missing: &[MissingBlob],
        bundle: &AssetBundle,
    ) -> Result<()> {
        self.upload_set(select_missing(missing, bundle)?).await
    }

    async fn upload_set(&self, parts: UploadSet) -> Result<()> {
        if parts.is_empty() {
            return Ok(());
        }

        let mut form = Form::new();
        for (digest, bytes) in parts {
            let length = bytes.len() as u64;
            // `Bytes` clones are refcount bumps, so streaming the part with a
            // known length hands the buffer over without copying it.
            form = form.part(
                digest.to_string(),
                Part::stream_with_length(Body::from(bytes), length),
            );
        }

        let response = self
            .http
            .post(self.url(BLOBS))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await?;
        self.expect_success(response).await
    }
}

/// The upload set for a `409`: exactly the blobs the server named, in the
/// order it named them — never the whole bundle.
fn select_missing(missing: &[MissingBlob], bundle: &AssetBundle) -> Result<UploadSet> {
    missing
        .iter()
        .map(|blob| {
            let bytes = bundle.get(&blob.key).ok_or_else(|| Error::MissingAsset {
                key: blob.key.clone(),
            })?;
            Ok((blob.digest.clone(), bytes.clone()))
        })
        .collect()
}

/// The upload set for an explicit [`Client::upload`]: the bundle entries whose
/// bytes hash to one of `digests`, deduplicated — two keys can share content.
fn select_by_digest(digests: &[Digest], bundle: &AssetBundle) -> Result<UploadSet> {
    let hashed = bundle.hashed();
    let mut parts: UploadSet = Vec::with_capacity(digests.len());

    for digest in digests {
        if parts.iter().any(|(taken, _)| taken == digest) {
            continue;
        }
        let bytes = hashed
            .iter()
            .find(|(_, candidate, _)| candidate == digest)
            .map(|(_, _, bytes)| (*bytes).clone())
            .ok_or_else(|| Error::UnknownDigest {
                digest: digest.clone(),
            })?;
        parts.push((digest.clone(), bytes));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::sha256;

    fn bundle() -> AssetBundle {
        AssetBundle::new()
            .with("img/kept.png", &b"kept"[..])
            .with("img/gone.png", &b"gone"[..])
            .with("img/also-gone.png", &b"also gone"[..])
    }

    fn missing(keys: &[&str]) -> Vec<MissingBlob> {
        keys.iter()
            .map(|key| MissingBlob {
                key: key.to_string(),
                digest: sha256(&bundle().get(key).expect("a bundled key")[..]),
            })
            .collect()
    }

    #[test]
    fn a_409_uploads_only_the_blobs_it_named() {
        let parts = select_missing(&missing(&["img/gone.png", "img/also-gone.png"]), &bundle())
            .expect("both are in the bundle");

        assert_eq!(parts.len(), 2, "never the whole bundle");
        assert_eq!(parts[0].0, sha256(b"gone"));
        assert_eq!(&parts[0].1[..], b"gone");
        assert_eq!(parts[1].0, sha256(b"also gone"));
        assert!(
            !parts.iter().any(|(d, _)| *d == sha256(b"kept")),
            "a blob the server already holds must not be re-uploaded"
        );
    }

    #[test]
    fn a_409_naming_a_key_the_bundle_lacks_fails_before_the_upload() {
        let phantom = vec![MissingBlob {
            key: "img/never-supplied.png".to_string(),
            digest: sha256(b"whatever"),
        }];

        match select_missing(&phantom, &bundle()) {
            Err(Error::MissingAsset { key }) => assert_eq!(key, "img/never-supplied.png"),
            other => panic!("expected MissingAsset, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_missing_list_selects_nothing() {
        assert!(select_missing(&[], &bundle()).unwrap().is_empty());
        assert!(select_by_digest(&[], &bundle()).unwrap().is_empty());
    }

    #[test]
    fn selecting_by_digest_finds_the_bytes_that_hash_to_it() {
        let parts = select_by_digest(&[sha256(b"kept"), sha256(b"gone")], &bundle()).unwrap();

        assert_eq!(parts.len(), 2);
        assert_eq!(&parts[0].1[..], b"kept");
        assert_eq!(&parts[1].1[..], b"gone");
    }

    #[test]
    fn selecting_by_digest_deduplicates_shared_content() {
        let bundle = AssetBundle::new()
            .with("a.png", &b"same"[..])
            .with("b.png", &b"same"[..]);
        let digest = sha256(b"same");

        let parts = select_by_digest(&[digest.clone(), digest], &bundle).unwrap();

        assert_eq!(parts.len(), 1, "one blob is one part, however many keys");
    }

    #[test]
    fn selecting_an_unknown_digest_is_an_error() {
        let stranger = sha256(b"nothing in the bundle");

        match select_by_digest(std::slice::from_ref(&stranger), &bundle()) {
            Err(Error::UnknownDigest { digest }) => assert_eq!(digest, stranger),
            other => panic!("expected UnknownDigest, got {other:?}"),
        }
    }
}
