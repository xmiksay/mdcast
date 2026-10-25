//! `GET /` landing page router-level tests (issue #99). Split out of
//! `tests.rs` to keep both files under the size cap.

use std::time::Duration;

use super::*;

fn get_root() -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap()
}

/// The router is built with a token, so a 200 here proves `/` sits outside
/// the bearer layer.
#[tokio::test]
async fn landing_page_is_public_html_with_the_version() {
    let (app, _) = app();

    let (status, response) = send(&app, get_root()).await;

    assert_eq!(status, StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], landing::CSP);
    let body = String::from_utf8(body_bytes(response).await.to_vec()).unwrap();
    assert!(body.starts_with("<!doctype html>"));
    assert!(body.contains(env!("CARGO_PKG_VERSION")));
    assert!(body.contains(r#"href="/health""#));
    assert!(body.contains(r#"href="/v1/capabilities""#));
}

#[tokio::test]
async fn landing_page_takes_no_render_slot() {
    let state = Arc::new(AppState {
        render_gate: RenderGate::new(0, Duration::ZERO, 0),
        ..state(TypstCompiler::Library, true, 1 << 20)
    });
    let app = router(state, Some(TOKEN), 1 << 20);

    assert_eq!(send(&app, get_root()).await.0, StatusCode::OK);
}

#[tokio::test]
async fn landing_page_is_get_only() {
    let (app, _) = app();
    let post = Request::builder()
        .method("POST")
        .uri("/")
        .body(Body::empty())
        .unwrap();

    assert_eq!(send(&app, post).await.0, StatusCode::METHOD_NOT_ALLOWED);
}
