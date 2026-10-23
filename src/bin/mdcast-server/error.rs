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
//!
//! The one opt-in exception to the first rule is a failed compile's typst
//! diagnostics (`MDCAST_EXPOSE_DIAGNOSTICS`, issue #96): already sanitised by
//! the engine down to virtual paths, they describe the caller's own template,
//! and without them a template author can only guess.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use mdcast::backends::typst::CompileDiagnostics;
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
    /// Every render slot stayed busy for the whole queue wait.
    Overloaded { retry_after_secs: u64 },
    /// The pipeline ran and failed: unrenderable input, a template that does
    /// not compile, an engine error. `detail` is what the response may show
    /// of it — empty unless diagnostics are exposed (see [`render_failed`]).
    RenderFailed {
        error: anyhow::Error,
        detail: Vec<String>,
    },
    /// Anything not attributable to the request.
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

/// The `map_err` for a render: with `expose`, the typst diagnostics anywhere
/// in the error chain become the response's `detail`.
pub fn render_failed(expose: bool) -> impl FnOnce(anyhow::Error) -> ApiError {
    move |error| {
        let detail = if expose {
            error
                .chain()
                .find_map(|e| e.downcast_ref::<CompileDiagnostics>())
                .map(|d| d.diagnostics.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        ApiError::RenderFailed { error, detail }
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
            ApiError::RenderFailed { error, detail } => sanitised(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::RenderFailed,
                "render failed",
                &error,
                detail,
            ),
            ApiError::Internal(error) => sanitised(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "internal error",
                &error,
                Vec::new(),
            ),
            ApiError::Overloaded { retry_after_secs } => {
                tracing::warn!("every render slot busy for the whole queue wait; answering 503");
                let mut response = body(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorCode::Overloaded,
                    "every render slot is busy; retry later",
                );
                response
                    .headers_mut()
                    .insert(RETRY_AFTER, retry_after_secs.into());
                response
            }
        }
    }
}

fn body(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Response {
    (status, Json(ErrorResponse::new(code, message))).into_response()
}

/// Log the full error chain against an incident id, return the id and nothing
/// else. The id is `<pid>-<counter>`: unique within a process, and the process
/// id is already in every log line's context.
fn sanitised(
    status: StatusCode,
    code: ErrorCode,
    what: &str,
    error: &anyhow::Error,
    detail: Vec<String>,
) -> Response {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let incident = format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    tracing::error!(incident, error = format!("{error:#}"), "{what}");
    let message = format!("{what} (incident {incident}); see the server log for details");
    (
        status,
        Json(ErrorResponse::new(code, message).with_detail(detail)),
    )
        .into_response()
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
                render_failed(false)(anyhow::anyhow!("boom")),
                StatusCode::UNPROCESSABLE_ENTITY,
                "render_failed",
            ),
            (
                ApiError::Overloaded {
                    retry_after_secs: 1,
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded",
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

    #[tokio::test]
    async fn overloaded_says_when_to_retry() {
        let response = ApiError::Overloaded {
            retry_after_secs: 7,
        }
        .into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[RETRY_AFTER], "7");
    }

    fn compile_failure() -> anyhow::Error {
        anyhow::Error::from(CompileDiagnostics {
            diagnostics: vec!["main.typ:1:9: error: unclosed delimiter".into()],
        })
        .context("render template")
    }

    #[tokio::test]
    async fn diagnostics_are_exposed_only_when_asked_for() {
        let (_, hidden) = parts(render_failed(false)(compile_failure())).await;
        assert!(!hidden.contains("\"detail\""), "{hidden}");
        assert!(!hidden.contains("unclosed"), "{hidden}");

        let (status, shown) = parts(render_failed(true)(compile_failure())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: ErrorResponse = serde_json::from_str(&shown).unwrap();
        assert_eq!(body.detail, ["main.typ:1:9: error: unclosed delimiter"]);
        assert!(body.message.contains("incident"), "{shown}");
    }

    #[tokio::test]
    async fn a_failure_without_diagnostics_has_no_detail_even_when_exposed() {
        let (_, body) = parts(render_failed(true)(anyhow::anyhow!("/tmp/x: boom"))).await;
        assert!(!body.contains("\"detail\""), "{body}");
        assert!(!body.contains("/tmp/x"), "{body}");
    }
}
