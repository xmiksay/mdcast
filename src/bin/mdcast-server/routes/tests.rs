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

fn app() -> (Router, Arc<BlobStore>) {
    app_with_compiler(TypstCompiler::Library)
}

fn app_with_compiler(typst_compiler: TypstCompiler) -> (Router, Arc<BlobStore>) {
    let state = state(typst_compiler, true, 1 << 20);
    let store = state.store.clone();
    (router(Arc::new(state), Some(TOKEN), 1 << 20), store)
}

/// Generous render slots, diagnostics hidden: what every test not about
/// those two knobs wants.
fn state(typst_compiler: TypstCompiler, pandoc_available: bool, max_upload: u64) -> AppState {
    AppState {
        store: Arc::new(BlobStore::new(1 << 20, 3, 32).unwrap()),
        registry: Registry::with_typst_compiler(typst_compiler.clone()),
        typst_compiler,
        pandoc_available,
        max_upload_bytes: max_upload,
        render_gate: RenderGate::new(16, std::time::Duration::from_secs(10), 32),
        expose_diagnostics: false,
    }
}

/// `None` when no `typst` binary is on `PATH` — callers skip gracefully,
/// matching the pandoc convention in `tests/render_smoke.rs`.
fn typst_cli_compiler(timeout: std::time::Duration) -> Option<TypstCompiler> {
    let bin = std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|dir| std::path::Path::new(dir).join("typst"))
        .find(|p| p.is_file())
        .map(|_| "typst".to_string())?;
    Some(TypstCompiler::Cli(
        mdcast::backends::typst::CliCompiler::new(bin, timeout),
    ))
}

/// Held by every test that spawns a real `typst` child, so the timeout test's
/// child count is not thrown off by a sibling's compile still running.
fn typst_process_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
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
    let (app, _) = app();

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
    let (app, _) = app();

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

/// A router built with `token: None` (the `MDCAST_TOKEN`-unset case) must
/// serve `/v1` without any credentials at all — the auth layer isn't merely
/// permissive, it's absent.
#[tokio::test]
async fn a_router_with_no_configured_token_serves_v1_without_credentials() {
    let state = state(TypstCompiler::Library, true, 1 << 20);
    let app = router(Arc::new(state), None, 1 << 20);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/blobs/check")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"digests":[]}"#))
        .unwrap();
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({ "missing": [] })
    );
}

#[tokio::test]
async fn blob_check_reports_exactly_the_digests_the_store_lacks() {
    let (app, store) = app();
    let present = sha256(b"present");
    let absent = sha256(b"absent");
    store.put(&present, b"present").unwrap();

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
    let (app, _) = app();
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
    let (app, store) = app();
    let claimed = sha256(b"what I promised");

    let (status, response) = send(&app, multipart(&[(claimed.as_str(), b"what I sent")])).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body_json(response).await["code"], "bad_request");
    assert!(!store.contains(&claimed));
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
    let (app, store) = app();
    let here = sha256(b"here");
    store.put(&here, b"here").unwrap();

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
    let (app, _) = app();
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
    let (app, _) = app();

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
    let (app, _) = app();

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
    let (app, _) = app();

    let (status, _) = send(
        &app,
        markdown_request(serde_json::json!({ "charts/revenue.svg": "NOPE" })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[path = "template_tests.rs"]
mod template_tests;

#[path = "capabilities_tests.rs"]
mod capabilities_tests;

#[path = "hardening_tests.rs"]
mod hardening_tests;

#[path = "landing_tests.rs"]
mod landing_tests;
