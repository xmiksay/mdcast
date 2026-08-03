//! Router-level tests: every one of these drives the real `Router` through
//! `tower::ServiceExt::oneshot`, so routing, the auth layer, extractors and
//! error mapping are all exercised — without a socket or a live server.
//!
//! Split out of `mod.rs` to keep both files under the size cap (same
//! convention as `backends/*_tests.rs`).

use super::*;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use bytes::Bytes;
use tower::ServiceExt;

use crate::blobs::store::sha256;

const TOKEN: &str = "test-token";
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;

async fn app(dir: &tempfile::TempDir) -> (Router, Arc<BlobStore>) {
    let store = Arc::new(
        BlobStore::open(dir.path().to_path_buf(), 1 << 20)
            .await
            .unwrap(),
    );
    let state = Arc::new(AppState {
        store: store.clone(),
        registry: Registry::with_defaults(),
    });
    (router(state, TOKEN, 1 << 20), store)
}

fn authorized(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

fn json_request(uri: &str, body: serde_json::Value) -> Request<Body> {
    authorized("POST", uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// `multipart/form-data` with one part per blob, each named by its claimed
/// digest — hand-rolled so the tests exercise the exact wire shape a client
/// has to produce.
fn multipart(parts: &[(&str, &[u8])]) -> Request<Body> {
    const BOUNDARY: &str = "mdcastboundary";
    let mut body: Vec<u8> = Vec::new();
    for (name, bytes) in parts {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    authorized("POST", "/v1/blobs")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Response) {
    let response = app.clone().oneshot(request).await.unwrap();
    (response.status(), response)
}

async fn body_bytes(response: Response) -> Bytes {
    to_bytes(response.into_body(), usize::MAX).await.unwrap()
}

async fn body_json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

#[tokio::test]
async fn health_needs_no_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_bytes(response).await, &b"ok"[..]);
}

#[tokio::test]
async fn v1_rejects_anything_but_the_configured_token() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    for header_value in [None, Some("Bearer wrong-token"), Some("Basic dXNlcjpwdw==")] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/blobs/check")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(value) = header_value {
            request = request.header(header::AUTHORIZATION, value);
        }
        let request = request.body(Body::from(r#"{"digests":[]}"#)).unwrap();

        let (status, response) = send(&app, request).await;

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "accepted {header_value:?} — an unauthenticated caller must never reach a handler"
        );
        assert_eq!(body_json(response).await["code"], "unauthorized");
    }
}

#[tokio::test]
async fn blob_check_reports_exactly_the_digests_the_store_lacks() {
    let dir = tempfile::tempdir().unwrap();
    let (app, store) = app(&dir).await;
    let present = sha256(b"present");
    let absent = sha256(b"absent");
    store.put(&present, b"present").await.unwrap();

    let request = json_request(
        "/v1/blobs/check",
        serde_json::json!({ "digests": [present.as_str(), absent.as_str()] }),
    );
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({ "missing": [absent.as_str()] })
    );
}

#[tokio::test]
async fn uploading_several_blobs_at_once_satisfies_a_later_check() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;
    let (first, second) = (sha256(b"one"), sha256(b"two"));

    let (status, _) = send(
        &app,
        multipart(&[(first.as_str(), b"one"), (second.as_str(), b"two")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Re-sending the identical upload must be a no-op, not an error.
    let (status, _) = send(&app, multipart(&[(first.as_str(), b"one")])).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let request = json_request(
        "/v1/blobs/check",
        serde_json::json!({ "digests": [first.as_str(), second.as_str()] }),
    );
    let (_, response) = send(&app, request).await;
    assert_eq!(
        body_json(response).await,
        serde_json::json!({ "missing": [] })
    );
}

#[tokio::test]
async fn an_upload_whose_bytes_do_not_match_its_digest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (app, store) = app(&dir).await;
    let claimed = sha256(b"what I promised");

    let (status, response) = send(&app, multipart(&[(claimed.as_str(), b"what I sent")])).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body_json(response).await["code"], "bad_request");
    assert!(!store.contains(&claimed).await);
}

fn markdown_request(manifest: serde_json::Value) -> Request<Body> {
    json_request(
        "/v1/render",
        serde_json::json!({
            "markdown": "# Quarterly review\n\n![chart](charts/revenue.svg)\n",
            "target": "pdf",
            "assets": manifest,
        }),
    )
}

#[tokio::test]
async fn a_render_naming_blobs_the_server_lacks_answers_409_with_all_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let (app, store) = app(&dir).await;
    let here = sha256(b"here");
    store.put(&here, b"here").await.unwrap();

    let request = markdown_request(serde_json::json!({
        "charts/revenue.svg": sha256(SVG).as_str(),
        "branding/logo.svg": sha256(b"logo").as_str(),
        "reference/reference.docx": here.as_str(),
    }));
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::CONFLICT);
    let body = body_json(response).await;
    let missing: Vec<&str> = body["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        missing,
        vec!["branding/logo.svg", "charts/revenue.svg"],
        "one 409 must list every missing blob, not just the first"
    );
    let digests: Vec<&str> = body["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["digest"].as_str().unwrap())
        .collect();
    assert!(digests.contains(&sha256(SVG).as_str()));
}

/// The whole negotiation, end to end against the real typst compiler: 409,
/// upload, retry, PDF.
#[tokio::test]
async fn a_render_with_a_satisfied_manifest_returns_the_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;
    let chart = sha256(SVG);
    let manifest = serde_json::json!({ "charts/revenue.svg": chart.as_str() });

    let (status, _) = send(&app, markdown_request(manifest.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "the blob is not there yet");

    let (status, _) = send(&app, multipart(&[(chart.as_str(), SVG)])).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, response) = send(&app, markdown_request(manifest)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"output.pdf\""
    );
    assert!(body_bytes(response).await.starts_with(b"%PDF-"));
}

fn pandoc_available() -> bool {
    std::process::Command::new("pandoc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The pandoc-backed leg of the same path — skipped gracefully (not
/// `#[ignore]`) when `pandoc` is absent, matching `tests/render_smoke.rs`, so
/// `cargo test` stays green on a machine without it.
#[tokio::test]
async fn a_pandoc_target_renders_end_to_end() {
    if !pandoc_available() {
        eprintln!("skipping a_pandoc_target_renders_end_to_end: `pandoc` not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let request = json_request(
        "/v1/render",
        serde_json::json!({
            "markdown": "---\ntitle: From the frontmatter\n---\n\n# Hello\n\nBody text.\n",
            "target": "docx",
            "options": { "toc_depth": 2 },
        }),
    );
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("wordprocessingml"),
    );
    assert!(
        body_bytes(response).await.starts_with(b"PK"),
        "docx is a zip"
    );
}

#[tokio::test]
async fn a_malformed_render_request_is_a_400_with_an_error_body() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    for body in [
        serde_json::json!({ "markdown": "# Hi" }),
        serde_json::json!({ "markdown": "# Hi", "target": "not-a-target" }),
        serde_json::json!({ "markdown": "# Hi", "target": "pdf", "options": { "toc_depth": 9 } }),
    ] {
        let (status, response) = send(&app, json_request("/v1/render", body)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["code"], "bad_request");
    }
}

/// An invalid digest in a manifest never reaches the store — `Digest`'s
/// deserializer rejects it at the request boundary.
#[tokio::test]
async fn a_manifest_with_a_bogus_digest_is_a_400() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let (status, _) = send(
        &app,
        markdown_request(serde_json::json!({ "charts/revenue.svg": "NOPE" })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn template_request(format: &str) -> Request<Body> {
    json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "#let d = json(\"/data.json\")\n= Invoice #d.number\n",
            "data": r#"{"number":"INV-042"}"#,
            "format": format,
        }),
    )
}

#[tokio::test]
async fn a_template_render_returns_a_pdf() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let (status, response) = send(&app, template_request("pdf")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/pdf");
    assert!(body_bytes(response).await.starts_with(b"%PDF-"));
}

#[tokio::test]
async fn a_template_can_import_a_partial_from_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;
    const PARTIAL: &[u8] = b"#let greeting = [Hello]";
    let digest = sha256(PARTIAL);
    send(&app, multipart(&[(digest.as_str(), PARTIAL)])).await;

    let request = json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "#import \"partials/header.typ\": greeting\n#greeting\n",
            "data": "{}",
            "assets": { "partials/header.typ": digest.as_str() },
        }),
    );
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK, "{:?}", body_bytes(response).await);
}

#[tokio::test]
async fn a_template_render_with_malformed_data_is_a_400() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let request = json_request(
        "/v1/render/template",
        serde_json::json!({ "template_source": "= Hi", "data": "not json" }),
    );
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body_json(response).await["message"]
            .as_str()
            .unwrap()
            .contains("`data`")
    );
}

#[cfg(not(feature = "typst-html"))]
#[tokio::test]
async fn html_template_output_says_so_when_the_feature_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let (status, response) = send(&app, template_request("html")).await;

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body_json(response).await["code"], "unsupported_target");
}

#[cfg(feature = "typst-html")]
#[tokio::test]
async fn html_template_output_works_when_the_feature_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = app(&dir).await;

    let (status, response) = send(&app, template_request("html")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    let body = body_bytes(response).await;
    assert!(String::from_utf8_lossy(&body).contains("Invoice INV-042"));
}

#[tokio::test]
async fn a_body_over_the_limit_is_a_413() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        BlobStore::open(dir.path().to_path_buf(), 1 << 20)
            .await
            .unwrap(),
    );
    let app = router(
        Arc::new(AppState {
            store,
            registry: Registry::with_defaults(),
        }),
        TOKEN,
        64,
    );

    let (status, response) = send(
        &app,
        json_request(
            "/v1/render",
            serde_json::json!({ "markdown": "#".repeat(4096), "target": "pdf" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body_json(response).await["code"], "payload_too_large");
}
