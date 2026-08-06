//! `POST /v1/render/template` router-level tests. Split out of `tests.rs` to
//! keep both files under the size cap (same convention as
//! `backends/*_tests.rs`).

use super::*;

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
    let (app, _) = app();

    let (status, response) = send(&app, template_request("pdf")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/pdf");
    assert!(body_bytes(response).await.starts_with(b"%PDF-"));
}

#[tokio::test]
async fn a_template_can_import_a_partial_from_the_manifest() {
    let (app, _) = app();
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
async fn a_template_render_threads_font_keys_into_the_compile() {
    let (app, _) = app();
    // Not a parseable face — registration warns and skips it (the same
    // degrade semantics as the markdown path's brand fonts), so the render
    // still succeeds; what this pins down is the wire field flowing through
    // blob resolution into the compile without an error. Face-level
    // resolution is covered by `tests/typst_fonts.rs` and the `assemble`
    // unit tests.
    const FONT: &[u8] = b"not-a-real-font";
    let digest = sha256(FONT);
    send(&app, multipart(&[(digest.as_str(), FONT)])).await;

    let request = json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "= Hi\n",
            "data": "{}",
            "assets": { "fonts/brand.ttf": digest.as_str() },
            "fonts": ["fonts/brand.ttf"],
        }),
    );
    let (status, response) = send(&app, request).await;

    assert_eq!(status, StatusCode::OK, "{:?}", body_bytes(response).await);
}

#[tokio::test]
async fn a_template_render_with_malformed_data_is_a_400() {
    let (app, _) = app();

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
    let (app, _) = app();

    let (status, response) = send(&app, template_request("html")).await;

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body_json(response).await["code"], "unsupported_target");
}

#[cfg(feature = "typst-html")]
#[tokio::test]
async fn html_template_output_works_when_the_feature_is_on() {
    let (app, _) = app();

    let (status, response) = send(&app, template_request("html")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    let body = body_bytes(response).await;
    assert!(String::from_utf8_lossy(&body).contains("Invoice INV-042"));
}

/// Issue #73's whole point for the template route: `cli` mode answers
/// `format: "html"` regardless of the `typst-html` cargo feature, unlike
/// `library` mode (the two tests above). Skips gracefully when no `typst`
/// binary is on `PATH`.
#[tokio::test]
async fn html_template_output_works_in_cli_mode_without_the_feature() {
    let Some(compiler) = typst_cli_compiler(std::time::Duration::from_secs(20)) else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let (app, _) = app_with_compiler(compiler);

    let (status, response) = send(&app, template_request("html")).await;

    assert_eq!(status, StatusCode::OK, "{:?}", body_bytes(response).await);
}

/// Acceptance criterion for issue #73: a deliberately non-terminating
/// template is killed under `cli` mode's compile timeout, the request
/// answers with a typed error rather than hanging, and no `typst` process is
/// left behind — the check `library` mode structurally cannot pass, since
/// `spawn_blocking` cannot be cancelled. Skips gracefully when no `typst`
/// binary is on `PATH`.
#[tokio::test]
async fn a_non_terminating_cli_compile_times_out_instead_of_hanging_the_request() {
    let Some(compiler) = typst_cli_compiler(std::time::Duration::from_millis(300)) else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let (app, _) = app_with_compiler(compiler);
    const SLOW: &str = r#"
#let x = {
  let i = 0
  let n = int(calc.pow(2, 62))
  for _ in range(n) { i = i + 1 }
  i
}
#x
"#;
    let request = json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": SLOW,
            "data": "{}",
        }),
    );

    let start = std::time::Instant::now();
    let (status, response) = send(&app, request).await;

    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "request did not return promptly after the compile timeout elapsed"
    );
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(response).await["code"], "render_failed");
}
