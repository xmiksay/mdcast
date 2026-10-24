//! Router-level tests for the request limits and the opt-in diagnostics
//! (issue #96): the body-size cap, the render concurrency gate, and
//! `render_failed`'s `detail`. Split out of `tests.rs` to keep both files
//! under the size cap.

use std::time::Duration;

use super::*;

/// A router whose state the test keeps, so it can hold a render slot itself.
fn gated(
    slots: usize,
    queue: Duration,
    expose_diagnostics: bool,
    compiler: TypstCompiler,
) -> (Router, Arc<AppState>) {
    let state = Arc::new(AppState {
        render_gate: RenderGate::new(slots, queue, 2 * slots),
        expose_diagnostics,
        ..state(compiler, true, 1 << 20)
    });
    (router(state.clone(), Some(TOKEN), 1 << 20), state)
}

fn pdf_request(markdown: &str) -> Request<Body> {
    json_request(
        "/v1/render",
        serde_json::json!({ "markdown": markdown, "target": "pdf" }),
    )
}

fn template_source_request(source: &str) -> Request<Body> {
    json_request(
        "/v1/render/template",
        serde_json::json!({ "template_source": source, "data": "{}" }),
    )
}

#[tokio::test]
async fn a_body_over_the_limit_is_a_413() {
    let app = router(
        Arc::new(state(TypstCompiler::Library, true, 64)),
        Some(TOKEN),
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

#[tokio::test]
async fn both_render_routes_answer_503_with_retry_after_while_every_slot_is_busy() {
    let (app, state) = gated(1, Duration::ZERO, false, TypstCompiler::Library);
    let held = state.render_gate.enter().await.ok().unwrap();

    for request in [pdf_request("# Hi"), template_source_request("= Hi")] {
        let (status, response) = send(&app, request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
        assert_eq!(body_json(response).await["code"], "overloaded");
    }

    drop(held);
    let (status, _) = send(&app, pdf_request("# Hi")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the freed slot serves the next render"
    );
}

#[tokio::test]
async fn a_render_waits_in_the_queue_for_a_slot_to_free_up() {
    let (app, state) = gated(1, Duration::from_secs(30), false, TypstCompiler::Library);
    let held = state.render_gate.enter().await.ok().unwrap();

    let pending = tokio::spawn({
        let app = app.clone();
        async move { send(&app, pdf_request("# Hi")).await.0 }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !pending.is_finished(),
        "it must queue, not fail or skip the gate"
    );
    drop(held);

    assert_eq!(pending.await.unwrap(), StatusCode::OK);
}

/// A blob evicted while its render waited for a slot is a `409` the caller
/// can recover from, not a `200` quietly missing that image.
#[tokio::test]
async fn a_blob_evicted_during_the_queue_wait_is_a_409() {
    // Budget 10 bytes, one life: the 5-byte blob evicts the 8-byte one.
    let state = Arc::new(AppState {
        store: Arc::new(BlobStore::new(10, 1, 32).unwrap()),
        render_gate: RenderGate::new(1, Duration::from_secs(30), 2),
        ..state(TypstCompiler::Library, true, 1 << 20)
    });
    let app = router(state.clone(), Some(TOKEN), 1 << 20);
    let doomed = sha256(b"a-doomed");
    state.store.put(&doomed, b"a-doomed").unwrap();
    let held = state.render_gate.enter().await.ok().unwrap();

    let pending = tokio::spawn({
        let app = app.clone();
        let request = json_request(
            "/v1/render",
            serde_json::json!({
                "markdown": "![](img/x.png)",
                "target": "pdf",
                "assets": { "img/x.png": doomed.as_str() },
            }),
        );
        async move {
            let (status, response) = send(&app, request).await;
            (status, body_json(response).await)
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!pending.is_finished(), "resolved, now queued");
    state.store.put(&sha256(b"bbbbb"), b"bbbbb").unwrap();
    assert!(!state.store.contains(&doomed), "the test must really evict");
    drop(held);

    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["missing"][0]["key"], "img/x.png", "{body}");
}

/// Past `MDCAST_RENDER_QUEUE_MAX` waiting renders, the next is turned away at
/// once instead of holding its body in memory for the whole queue time.
#[tokio::test]
async fn a_full_queue_answers_503_without_waiting() {
    let state = Arc::new(AppState {
        render_gate: RenderGate::new(1, Duration::from_secs(30), 1),
        ..state(TypstCompiler::Library, true, 1 << 20)
    });
    let app = router(state.clone(), Some(TOKEN), 1 << 20);
    let held = state.render_gate.enter().await.ok().unwrap();
    let queued = tokio::spawn({
        let app = app.clone();
        async move { send(&app, pdf_request("# Hi")).await.0 }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let start = std::time::Instant::now();
    let (status, response) = send(&app, pdf_request("# Hi")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(start.elapsed() < Duration::from_secs(5), "it did not wait");
    assert_eq!(body_json(response).await["code"], "overloaded");

    drop(held);
    assert_eq!(queued.await.unwrap(), StatusCode::OK, "the queued one runs");
}

/// Cheap answers — a 400, a 409, blob traffic, health — never need a slot.
#[tokio::test]
async fn only_actual_renders_take_a_slot() {
    let (app, _state) = gated(0, Duration::ZERO, false, TypstCompiler::Library);

    let bad_toc = json_request(
        "/v1/render",
        serde_json::json!({ "markdown": "# Hi", "target": "pdf", "options": { "toc_depth": 9 } }),
    );
    assert_eq!(send(&app, bad_toc).await.0, StatusCode::BAD_REQUEST);

    let missing = json_request(
        "/v1/render/template",
        serde_json::json!({
            "template_source": "= Hi",
            "data": "{}",
            "assets": { "logo.svg": sha256(b"absent").as_str() },
        }),
    );
    assert_eq!(send(&app, missing).await.0, StatusCode::CONFLICT);

    let check = json_request("/v1/blobs/check", serde_json::json!({ "digests": [] }));
    assert_eq!(send(&app, check).await.0, StatusCode::OK);

    let (status, _) = send(&app, pdf_request("# Hi")).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "zero slots: no render"
    );
}

const BROKEN: &str = "= Hi\n#let x = (\n";

async fn render_failed_detail(app: &Router, request: Request<Body>) -> Option<Vec<String>> {
    let (status, response) = send(app, request).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(response).await;
    assert_eq!(body["code"], "render_failed");
    assert!(body["message"].as_str().unwrap().contains("incident"));
    body.get("detail").map(|d| {
        d.as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect()
    })
}

async fn assert_detail_only_when_exposed(compiler: TypstCompiler) {
    let (hidden, _) = gated(4, Duration::ZERO, false, compiler.clone());
    let detail = render_failed_detail(&hidden, template_source_request(BROKEN)).await;
    assert_eq!(detail, None, "diagnostics are opt-in");

    let (shown, _) = gated(4, Duration::ZERO, true, compiler);
    let detail = render_failed_detail(&shown, template_source_request(BROKEN))
        .await
        .expect("detail when exposed");
    let all = detail.join("\n");
    assert!(all.contains("main.typ:2:"), "{all}");
    assert!(all.contains("error:"), "{all}");
    assert!(
        !all.contains("mdcast-typst-") && !all.contains("/tmp"),
        "temp paths leaked: {all}"
    );
}

#[tokio::test]
async fn template_diagnostics_are_exposed_only_on_opt_in_library() {
    assert_detail_only_when_exposed(TypstCompiler::Library).await;
}

#[tokio::test]
async fn template_diagnostics_are_exposed_only_on_opt_in_cli() {
    let Some(compiler) = typst_cli_compiler(Duration::from_secs(20)) else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    assert_detail_only_when_exposed(compiler).await;
}

/// The markdown route reaches the same compile: a caller-supplied layout
/// that does not compile surfaces its diagnostics too.
#[tokio::test]
async fn markdown_route_exposes_a_broken_layouts_diagnostics() {
    let (app, state) = gated(4, Duration::ZERO, true, TypstCompiler::Library);
    let layout = BROKEN.as_bytes();
    let digest = sha256(layout);
    state.store.put(&digest, layout).unwrap();

    let request = json_request(
        "/v1/render",
        serde_json::json!({
            "markdown": "<page class=\"broken\">\n\nHi\n\n</page>\n",
            "target": "pdf",
            "assets": { "typst/layouts/pdf/broken.typ": digest.as_str() },
        }),
    );
    let detail = render_failed_detail(&app, request)
        .await
        .expect("detail when exposed");

    assert!(detail.iter().any(|d| d.contains("error:")), "{detail:?}");
}
