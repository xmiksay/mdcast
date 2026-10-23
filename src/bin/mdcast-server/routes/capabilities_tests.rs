//! `GET /v1/capabilities` router-level tests. Split out of `tests.rs` to keep
//! both files under the size cap (same convention as `template_tests.rs`).

use super::*;

fn app_with(
    pandoc_available: bool,
    max_upload_bytes: u64,
    typst_compiler: TypstCompiler,
) -> Router {
    let store = Arc::new(BlobStore::new(1 << 20, 3, 32).unwrap());
    let state = Arc::new(AppState {
        store,
        registry: Registry::with_typst_compiler(typst_compiler.clone()),
        typst_compiler,
        pandoc_available,
        max_upload_bytes,
    });
    router(state, Some(TOKEN), 1 << 20)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// The whole point of the endpoint: reachable without a bearer token, even
/// though this router was built with one configured.
#[tokio::test]
async fn is_reachable_without_a_bearer_token() {
    let app = app_with(true, 1 << 20, TypstCompiler::Library);

    let (status, response) = send(&app, get("/v1/capabilities")).await;

    assert_eq!(status, StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["max_upload_bytes"], 1 << 20);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn typst_targets_are_always_reported() {
    let app = app_with(false, 1 << 20, TypstCompiler::Library);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    let body = body_json(response).await;
    let targets: Vec<&str> = body["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(targets, vec!["pdf", "pdf-presentation"]);
}

/// `Library` mode's `html` template support tracks the `typst-html` cargo
/// feature — mirrors `template_tests.rs`'s own split on this feature.
#[cfg(not(feature = "typst-html"))]
#[tokio::test]
async fn library_mode_reports_pdf_only_without_the_typst_html_feature() {
    let app = app_with(false, 1 << 20, TypstCompiler::Library);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    assert_eq!(
        body_json(response).await["template_formats"],
        serde_json::json!(["pdf"])
    );
}

#[cfg(feature = "typst-html")]
#[tokio::test]
async fn library_mode_reports_html_when_the_typst_html_feature_is_on() {
    let app = app_with(false, 1 << 20, TypstCompiler::Library);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    assert_eq!(
        body_json(response).await["template_formats"],
        serde_json::json!(["pdf", "html"])
    );
}

#[tokio::test]
async fn pandoc_targets_appear_only_when_pandoc_was_found_at_startup() {
    let app = app_with(true, 1 << 20, TypstCompiler::Library);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    let body = body_json(response).await;
    let targets: Vec<&str> = body["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        vec![
            "pdf",
            "pdf-presentation",
            "docx",
            "odt",
            "pptx",
            "html-reveal"
        ]
    );
}

/// `Cli` mode supports `html` template export regardless of the `typst-html`
/// cargo feature — mirrors `template.rs`'s own gate.
#[tokio::test]
async fn cli_typst_compiler_reports_html_template_support() {
    let cli = TypstCompiler::Cli(mdcast::backends::typst::CliCompiler::new(
        "typst",
        std::time::Duration::from_secs(1),
    ));
    let app = app_with(false, 1 << 20, cli);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    let body = body_json(response).await;
    assert_eq!(body["template_formats"], serde_json::json!(["pdf", "html"]));
}

#[tokio::test]
async fn generators_compiled_in_are_advertised() {
    let app = app_with(false, 1 << 20, TypstCompiler::Library);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    assert_eq!(
        body_json(response).await["generators"],
        serde_json::json!(["qr", "fen", "pgn"])
    );
}

#[tokio::test]
async fn max_upload_bytes_is_echoed_verbatim() {
    let app = app_with(true, 42, TypstCompiler::Library);

    let (_, response) = send(&app, get("/v1/capabilities")).await;

    assert_eq!(body_json(response).await["max_upload_bytes"], 42);
}
