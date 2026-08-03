//! The one error type every handler returns, and its projection onto the wire
//! protocol.
//!
//! Two rules hold here. First, an internal failure never reaches the client
//! verbatim: the `anyhow` chain goes to `tracing::error!` together with an
//! incident id, and the response carries that id plus a fixed message, so an
//! operator can join the two without the client learning about paths, engine
//! internals, or the shape of the filesystem. Second, everything that *is* the
//! caller's own doing (a malformed body, a digest that doesn't match the bytes
//! it was claimed for) is echoed back in full — that is the caller's own data,
//! and hiding it would just make the API unusable.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mdcast::mdcast_api::wire::{ErrorCode, ErrorResponse, MissingBlob, MissingBlobsError};
use serde::de::DeserializeOwned;

pub enum ApiError {
    /// The request body is malformed or a field is out of range.
    BadRequest(String),
    /// No bearer token, a malformed `Authorization` header, or the wrong one.
    Unauthorized,
    /// The body is bigger than `MDCAST_MAX_UPLOAD_BYTES`.
    PayloadTooLarge,
    /// This build has no engine/feature for what was asked. Only HTML
    /// template output can be missing today, so the variant exists only in a
    /// build that can actually hit it — a server compiled with `typst-html`
    /// supports every target and format the protocol can name, and giving it
    /// an error it can never produce would be a lie the compiler is right to
    /// flag. Drop the `cfg` the day a second unsupported case appears.
    #[cfg(not(feature = "typst-html"))]
    Unsupported(String),
    /// The manifest names blobs the store does not hold — the negotiation
    /// signal a client retries on, never a plain error.
    MissingBlobs(Vec<MissingBlob>),
    /// The pipeline ran and failed: unrenderable input, a template that does
    /// not compile, an engine error.
    RenderFailed(anyhow::Error),
    /// Anything not attributable to the request.
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            // 409 is the only status with its own body shape: a client acts on
            // it (upload the listed blobs, retry the identical request) rather
            // than reporting it.
            ApiError::MissingBlobs(missing) => {
                tracing::info!(count = missing.len(), "manifest incomplete; answering 409");
                (
                    StatusCode::CONFLICT,
                    Json(MissingBlobsError {
                        message: format!(
                            "{} asset(s) referenced by the manifest are not on the server; \
                             upload them and retry",
                            missing.len()
                        ),
                        missing,
                    }),
                )
                    .into_response()
            }
            ApiError::BadRequest(message) => {
                body(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
            }
            ApiError::Unauthorized => body(
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "missing or invalid bearer token",
            ),
            ApiError::PayloadTooLarge => body(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::PayloadTooLarge,
                "request body exceeds the configured limit",
            ),
            #[cfg(not(feature = "typst-html"))]
            ApiError::Unsupported(message) => body(
                StatusCode::NOT_IMPLEMENTED,
                ErrorCode::UnsupportedTarget,
                message,
            ),
            // 422 rather than 500: the overwhelmingly common cause is the
            // submitted document/template, and `Internal` exists for what the
            // server genuinely cannot attribute to the request.
            ApiError::RenderFailed(error) => sanitised(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::RenderFailed,
                "render failed",
                &error,
            ),
            ApiError::Internal(error) => sanitised(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "internal error",
                &error,
            ),
        }
    }
}

fn body(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Response {
    (status, Json(ErrorResponse::new(code, message))).into_response()
}

/// Log the full error chain against an incident id, return the id and nothing
/// else. The id is `<pid>-<counter>`: unique within a process, and the process
/// id is already in every log line's context.
fn sanitised(status: StatusCode, code: ErrorCode, what: &str, error: &anyhow::Error) -> Response {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let incident = format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    tracing::error!(incident, error = format!("{error:#}"), "{what}");
    body(
        status,
        code,
        format!("{what} (incident {incident}); see the server log for details"),
    )
}

/// Turn axum's `Json` rejection into an `ApiError` so *every* failure carries
/// an `ErrorResponse` body, including the ones axum would otherwise answer
/// with a bare status line. Handlers take `Result<Json<T>, JsonRejection>` and
/// hand it straight to this.
pub fn json_body<T: DeserializeOwned>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    match body {
        Ok(Json(value)) => Ok(value),
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            Err(ApiError::PayloadTooLarge)
        }
        // `body_text()` describes the caller's own payload ("missing field
        // `target` at line 1 column 12"), so it is safe to echo.
        Err(rejection) => Err(ApiError::BadRequest(rejection.body_text())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use mdcast::mdcast_api::Digest;

    async fn parts(error: ApiError) -> (StatusCode, String) {
        let response = error.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn each_variant_maps_to_its_status_and_code() {
        for (error, status, code) in [
            (
                ApiError::BadRequest("nope".into()),
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                ApiError::Unauthorized,
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                ApiError::PayloadTooLarge,
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
            ),
            (
                ApiError::RenderFailed(anyhow::anyhow!("boom")),
                StatusCode::UNPROCESSABLE_ENTITY,
                "render_failed",
            ),
            (
                ApiError::Internal(anyhow::anyhow!("boom")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ] {
            let (got_status, body) = parts(error).await;
            assert_eq!(got_status, status, "{body}");
            assert!(body.contains(code), "expected code {code} in {body}");
        }
    }

    #[cfg(not(feature = "typst-html"))]
    #[tokio::test]
    async fn an_unsupported_format_is_a_501() {
        let (status, body) = parts(ApiError::Unsupported("no html here".into())).await;

        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(body.contains("unsupported_target"), "{body}");
    }

    #[tokio::test]
    async fn internal_errors_never_leak_their_message() {
        let secret = "/var/lib/mdcast/blobs/ab/deadbeef: permission denied";
        let (_, body) = parts(ApiError::Internal(anyhow::anyhow!("{secret}"))).await;

        assert!(!body.contains(secret), "leaked the internal error: {body}");
        assert!(body.contains("incident"), "{body}");
    }

    #[tokio::test]
    async fn missing_blobs_answers_409_with_every_missing_entry() {
        let missing: Vec<MissingBlob> = ["img/a.png", "img/b.png"]
            .iter()
            .enumerate()
            .map(|(i, key)| MissingBlob {
                key: key.to_string(),
                digest: Digest::from_sha256([i as u8; 32]),
            })
            .collect();

        let (status, body) = parts(ApiError::MissingBlobs(missing)).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body.contains("img/a.png") && body.contains("img/b.png"),
            "{body}"
        );
    }
}
