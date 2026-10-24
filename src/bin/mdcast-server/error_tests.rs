//! `ApiError` projection tests. Split out of `error.rs` to keep it under the
//! size cap.

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

#[test]
fn detail_is_capped_by_count_and_by_bytes() {
    let many: Vec<String> = (0..150)
        .map(|i| format!("main.typ:{i}:1: error: x"))
        .collect();
    let capped = cap_detail(&many);
    assert_eq!(capped.len(), MAX_DETAIL_ENTRIES + 1);
    assert_eq!(capped[MAX_DETAIL_ENTRIES], "… 50 more");

    // Multi-byte chars straddling the byte budget must not split.
    let huge = vec!["é".repeat(MAX_DETAIL_BYTES), "dropped".into()];
    let capped = cap_detail(&huge);
    assert_eq!(capped.len(), 2);
    assert!(capped[0].len() <= MAX_DETAIL_BYTES);
    assert_eq!(capped[1], "… 1 more");

    let few = vec!["a".to_string(), "b".to_string()];
    assert_eq!(cap_detail(&few), few, "under the caps nothing changes");
}

#[tokio::test]
async fn an_engine_that_cannot_run_is_internal_not_render_failed() {
    let error = anyhow::Error::from(SpawnError {
        what: "pandoc".into(),
        program: "pandoc".into(),
        source: std::io::Error::from(std::io::ErrorKind::NotFound),
    })
    .context("render docx");

    let (status, body) = parts(render_failed(true)(error)).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("\"internal\""), "{body}");
}

#[tokio::test]
async fn a_failure_without_diagnostics_has_no_detail_even_when_exposed() {
    let (_, body) = parts(render_failed(true)(anyhow::anyhow!("/tmp/x: boom"))).await;
    assert!(!body.contains("\"detail\""), "{body}");
    assert!(!body.contains("/tmp/x"), "{body}");
}
