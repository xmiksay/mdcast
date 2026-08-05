//! The one error type every call returns.
//!
//! The failures a caller actually branches on are separate variants — bad
//! credentials, a body the server refused as too large, a `409` that survived
//! the retry, a bundle that cannot satisfy its own manifest. Everything else
//! the server classified itself arrives as [`Error::Server`], carrying the
//! wire [`ErrorCode`] rather than a flattened string, so a caller can match on
//! `RenderFailed` versus `Internal` without parsing prose.

use mdcast_api::Digest;
use mdcast_api::wire::{ErrorCode, ErrorResponse, MissingBlob};

pub type Result<T> = std::result::Result<T, Error>;

/// How long an unparseable error body may grow before it is truncated into an
/// error message — a misconfigured proxy can answer with a whole HTML page.
const SNIPPET_CHARS: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The client was built with a base URL or token it cannot use.
    #[error("invalid client configuration: {0}")]
    Config(String),

    /// The request never completed: DNS, connect, TLS, timeout, or a body that
    /// could not be read to the end.
    #[error("could not reach the mdcast server: {0}")]
    Transport(#[from] reqwest::Error),

    /// `401` — the bearer token is missing or wrong. The remedy is credentials,
    /// not payload, which is why it is not folded into [`Error::Server`].
    #[error("the mdcast server rejected the bearer token: {message}")]
    Unauthorized { message: String },

    /// `413` — the body exceeded the server's `MDCAST_MAX_UPLOAD_BYTES`.
    /// Retrying the identical request is hopeless; the caller has to shrink or
    /// split it.
    #[error("the mdcast server refused the request body as too large: {message}")]
    PayloadTooLarge { message: String },

    /// A second `409` for the same render after the missing blobs were
    /// uploaded. The client retries exactly once — the same digests coming
    /// back means eviction is racing the retry or the server is misbehaving,
    /// and looping would only hide it.
    #[error(
        "the mdcast server still reports {} blob(s) missing after uploading them and retrying once",
        .missing.len()
    )]
    BlobsStillMissing { missing: Vec<MissingBlob> },

    /// The server answered with its own [`ErrorResponse`] — the code and the
    /// message are carried through verbatim.
    #[error("the mdcast server returned {status} ({code:?}): {message}")]
    Server {
        status: u16,
        code: ErrorCode,
        message: String,
    },

    /// A status this protocol does not define, or one whose body is not an
    /// `ErrorResponse` (a proxy or load balancer answering instead of mdcast).
    #[error("unexpected {status} response from the mdcast server: {body}")]
    UnexpectedStatus { status: u16, body: String },

    /// The status was one the protocol defines, but its body did not decode.
    #[error("could not decode the mdcast server's {status} response: {reason}")]
    InvalidResponse { status: u16, reason: String },

    /// A manifest entry the bundle holds no bytes for. Caught locally: a `409`
    /// naming it would only tell the caller a digest it cannot resolve.
    #[error("asset key `{key}` is named by the manifest but the bundle holds no bytes for it")]
    MissingAsset { key: String },

    /// A digest-only bundle entry's `fetch` closure failed to produce bytes at
    /// all, while satisfying an upload the server actually asked for. Never a
    /// silent skip — the retry would just `409` on the same digest again.
    #[error("fetching asset `{key}` failed: {source}")]
    BlobFetchFailed {
        key: String,
        #[source]
        source: crate::bundle::BlobFetchError,
    },

    /// A digest asked for by hand (via [`crate::Client::upload`]) that nothing
    /// in the bundle hashes to.
    #[error("no asset in the bundle hashes to {digest}")]
    UnknownDigest { digest: Digest },

    /// A caller-supplied manifest that disagrees with the bundle's actual
    /// bytes. The upload would be rejected server-side anyway — the server
    /// re-hashes every byte — so it fails before the round trip. Also raised
    /// when a digest-only entry's `fetch` succeeds but the bytes it returns
    /// hash to something other than the digest it was declared with.
    #[error("asset key `{key}` is declared as {declared} but its bundle bytes hash to {actual}")]
    DigestMismatch {
        key: String,
        declared: Digest,
        actual: Digest,
    },
}

/// Project a non-success, non-`409` response onto the typed error. `401` and
/// `413` keep their identity even when the body is not an `ErrorResponse`,
/// because an intermediary can answer either one with its own page.
pub(crate) fn from_status(status: u16, body: &[u8]) -> Error {
    let parsed = serde_json::from_slice::<ErrorResponse>(body).ok();
    match status {
        401 => Error::Unauthorized {
            message: message_of(parsed, body),
        },
        413 => Error::PayloadTooLarge {
            message: message_of(parsed, body),
        },
        _ => match parsed {
            Some(ErrorResponse { code, message }) => Error::Server {
                status,
                code,
                message,
            },
            None => Error::UnexpectedStatus {
                status,
                body: snippet(body),
            },
        },
    }
}

fn message_of(parsed: Option<ErrorResponse>, body: &[u8]) -> String {
    parsed.map(|e| e.message).unwrap_or_else(|| snippet(body))
}

fn snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.chars().count() <= SNIPPET_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(SNIPPET_CHARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(code: &str, message: &str) -> Vec<u8> {
        format!(r#"{{"code":"{code}","message":"{message}"}}"#).into_bytes()
    }

    #[test]
    fn unauthorized_and_too_large_get_their_own_variants() {
        assert!(matches!(
            from_status(401, &body("unauthorized", "missing or invalid bearer token")),
            Error::Unauthorized { message } if message.contains("bearer token")
        ));
        assert!(matches!(
            from_status(413, &body("payload_too_large", "too big")),
            Error::PayloadTooLarge { message } if message == "too big"
        ));
    }

    #[test]
    fn an_intermediarys_bodyless_401_is_still_unauthorized() {
        assert!(matches!(
            from_status(401, b"<html>Forbidden by the proxy</html>"),
            Error::Unauthorized { message } if message.contains("proxy")
        ));
    }

    #[test]
    fn a_server_error_response_carries_its_code_through() {
        let err = from_status(422, &body("render_failed", "render failed (incident 7-0)"));

        match err {
            Error::Server {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 422);
                assert_eq!(code, ErrorCode::RenderFailed);
                assert!(message.contains("incident 7-0"), "{message}");
            }
            other => panic!("expected Server, got {other:?}"),
        }
        assert!(matches!(
            from_status(501, &body("unsupported_target", "no typst-html here")),
            Error::Server {
                code: ErrorCode::UnsupportedTarget,
                ..
            }
        ));
        assert!(matches!(
            from_status(500, &body("internal", "internal error")),
            Error::Server {
                code: ErrorCode::Internal,
                ..
            }
        ));
    }

    #[test]
    fn a_body_that_is_not_an_error_response_is_unexpected() {
        match from_status(502, b"<html>bad gateway</html>") {
            Error::UnexpectedStatus { status, body } => {
                assert_eq!(status, 502);
                assert_eq!(body, "<html>bad gateway</html>");
            }
            other => panic!("expected UnexpectedStatus, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_body_is_truncated_on_a_char_boundary() {
        let huge = "é".repeat(SNIPPET_CHARS * 2).into_bytes();

        match from_status(502, &huge) {
            Error::UnexpectedStatus { body, .. } => {
                assert_eq!(body.chars().count(), SNIPPET_CHARS + 1, "{body}");
                assert!(body.ends_with('…'));
            }
            other => panic!("expected UnexpectedStatus, got {other:?}"),
        }
    }
}
