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

/// The 501 comes before blob resolution: no 409 upload round trip (and no
/// render slot) for output the server could never produce.
#[cfg(not(feature = "typst-html"))]
#[tokio::test]
async fn unsupported_html_output_is_a_501_even_with_missing_blobs() {
    let (app, _) = app();
    let missing = sha256(b"never uploaded");

    let request = json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "= Hi\n",
            "data": "{}",
            "format": "html",
            "assets": { "img/logo.png": missing.as_str() },
        }),
    );
    let (status, response) = send(&app, request).await;

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
    let _guard = typst_process_lock().lock().await;
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
    let _guard = typst_process_lock().lock().await;
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

    let before = count_child_processes_named("typst");
    let start = std::time::Instant::now();
    let (status, response) = send(&app, request).await;

    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "request did not return promptly after the compile timeout elapsed"
    );
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(response).await["code"], "render_failed");
    let mut after = count_child_processes_named("typst");
    for _ in 0..20 {
        if after <= before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        after = count_child_processes_named("typst");
    }
    assert!(
        after <= before,
        "typst child left behind: before={before} after={after}"
    );
}

/// The lib's `test_support` helper is `#[cfg(test)]` inside `mdcast`, so not
/// reachable from this binary. Direct children only (an unreaped zombie keeps
/// its ppid); the ppid is the second field after the last `)`, since `comm`
/// may contain `)`. Linux-only; 0 elsewhere, never a false failure.
fn count_child_processes_named(name: &str) -> usize {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return 0;
        };
        let me = std::process::id().to_string();
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok())
            .filter(|e| {
                std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name)
            })
            .filter(|e| {
                std::fs::read_to_string(e.path().join("stat")).is_ok_and(|stat| {
                    stat.rsplit_once(')')
                        .and_then(|(_, rest)| rest.split_whitespace().nth(1).map(|p| p == me))
                        .unwrap_or(false)
                })
            })
            .count()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        0
    }
}

fn generated_request(generated: serde_json::Value, assets: serde_json::Value) -> Request<Body> {
    json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "#image(\"qr.svg\", width: 3cm)\n",
            "data": "{}",
            "assets": assets,
            "generated": generated,
        }),
    )
}

#[tokio::test]
async fn a_generated_qr_code_is_served_at_its_path() {
    let (app, _) = app();
    let generated = serde_json::json!([{"path": "qr.svg", "kind": "qr", "data": "SPD*1.0*AM:1"}]);

    let (status, response) = send(&app, generated_request(generated, serde_json::json!({}))).await;

    let body = body_bytes(response).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body.starts_with(b"%PDF-"));
}

#[tokio::test]
async fn an_unencodable_generated_entry_is_a_400_naming_it() {
    let (app, _) = app();
    let generated = serde_json::json!([{"path": "qr.svg", "kind": "qr", "data": ""}]);

    let (status, response) = send(&app, generated_request(generated, serde_json::json!({}))).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body_json(response).await["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("\"qr.svg\""), "{message}");
}

#[tokio::test]
async fn a_generated_path_colliding_with_the_manifest_is_a_400_before_any_409() {
    let (app, _) = app();
    let generated = serde_json::json!([{"path": "qr.svg", "kind": "qr", "data": "x"}]);
    // Never uploaded: the collision must be reported instead of a 409.
    let assets = serde_json::json!({"qr.svg": sha256(b"never uploaded").as_str()});

    let (status, response) = send(&app, generated_request(generated, assets)).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body_json(response).await["message"]
            .as_str()
            .unwrap()
            .contains("collides")
    );
}

fn board_request(generated: serde_json::Value) -> Request<Body> {
    json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "#image(\"board.svg\")\n",
            "data": "{}",
            "generated": generated,
        }),
    )
}

#[tokio::test]
async fn generated_fen_and_pgn_boards_are_served_at_their_paths() {
    let (app, _) = app();
    for generated in [
        serde_json::json!([{"path": "board.svg", "kind": "fen",
            "fen": "8/8/8/4k3/8/8/8/4K3", "flip": true, "theme": "green"}]),
        serde_json::json!([{"path": "board.svg", "kind": "pgn",
            "pgn": "1. e4 e5 2. Nf3", "move": 2}]),
    ] {
        let (status, response) = send(&app, board_request(generated)).await;

        let body = body_bytes(response).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body.starts_with(b"%PDF-"));
    }
}

#[tokio::test]
async fn an_invalid_generated_board_is_a_400_naming_it() {
    let (app, _) = app();
    for generated in [
        serde_json::json!([{"path": "board.svg", "kind": "fen", "fen": "not a fen"}]),
        serde_json::json!([{"path": "board.svg", "kind": "pgn", "pgn": "1. e4", "move": 5}]),
    ] {
        let (status, response) = send(&app, board_request(generated)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body_json(response).await["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("\"board.svg\""), "{message}");
    }
}

#[tokio::test]
async fn a_template_with_a_text_svg_from_the_manifest_renders() {
    // Mixed-fallback SVG text (issue #92's trigger) must be outlined before
    // typst sees it, or the compile can hang instead of answering.
    const SVG: &[u8] = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"40\">\
        <text x=\"5\" y=\"25\" font-family=\"arial\">HTTP ↔ doména</text></svg>"
        .as_bytes();
    let (app, _) = app();
    let digest = sha256(SVG);
    send(&app, multipart(&[(digest.as_str(), SVG)])).await;

    let request = json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "#image(\"flow.svg\", width: 5cm)\n",
            "data": "{}",
            "assets": { "flow.svg": digest.as_str() },
        }),
    );
    let (status, response) =
        tokio::time::timeout(std::time::Duration::from_secs(60), send(&app, request))
            .await
            .expect("template render hung");

    assert_eq!(status, StatusCode::OK, "{:?}", body_bytes(response).await);
}
