//! Blob negotiation: the exchange that keeps asset bytes off the render
//! request. A client asks which digests the server is missing, uploads only
//! those, and renders.

use serde::{Deserialize, Serialize};

use crate::digest::Digest;

/// "Which of these do you already have?" — sent before a render (or before
/// re-uploading a large asset set) to avoid a doomed round-trip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobCheckRequest {
    pub digests: Vec<Digest>,
}

/// The subset of the requested digests the server does **not** hold. Empty
/// means a render with that manifest will not stall on missing assets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobCheckResponse {
    pub missing: Vec<Digest>,
}

/// One manifest entry the server cannot resolve. Carries the key as well as
/// the digest so the client knows which of its own assets to upload without
/// keeping a reverse index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingBlob {
    pub key: String,
    pub digest: Digest,
}

/// `409 Conflict` body from a render whose manifest references digests the
/// server does not hold. This is the negotiation a client retries on: upload
/// `missing`, then repeat the identical render request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingBlobsError {
    pub message: String,
    pub missing: Vec<MissingBlob>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Digest {
        Digest::from_sha256([byte; 32])
    }

    #[test]
    fn blob_check_round_trips_through_json() {
        let req = BlobCheckRequest {
            digests: vec![digest(1), digest(2)],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<BlobCheckRequest>(&json).unwrap(),
            req
        );

        let res = BlobCheckResponse {
            missing: vec![digest(2)],
        };
        let json = serde_json::to_string(&res).unwrap();
        assert_eq!(
            serde_json::from_str::<BlobCheckResponse>(&json).unwrap(),
            res
        );
    }

    #[test]
    fn empty_blob_check_response_means_nothing_is_missing() {
        let res: BlobCheckResponse = serde_json::from_str(r#"{"missing":[]}"#).unwrap();
        assert_eq!(res, BlobCheckResponse::default());
        assert!(res.missing.is_empty());
    }

    #[test]
    fn missing_blobs_error_round_trips_through_json() {
        let err = MissingBlobsError {
            message: "2 assets are not on the server".to_string(),
            missing: vec![
                MissingBlob {
                    key: "img/logo.png".to_string(),
                    digest: digest(1),
                },
                MissingBlob {
                    key: "typst/layouts/pdf/hero.typ".to_string(),
                    digest: digest(2),
                },
            ],
        };

        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            serde_json::from_str::<MissingBlobsError>(&json).unwrap(),
            err
        );
    }

    #[test]
    fn missing_blobs_error_rejects_an_invalid_digest() {
        let err = serde_json::from_str::<MissingBlobsError>(
            r#"{"message":"m","missing":[{"key":"k","digest":"NOPE"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("64 hex characters"), "{err}");
    }
}
