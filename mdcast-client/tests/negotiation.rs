//! End-to-end tests of the request/retry flow against a canned HTTP server.
//!
//! `wiremock` is a dev-dependency only — it never reaches a consumer's tree —
//! and the real `mdcast-server` binary is deliberately not involved: these
//! tests are about what the *client* does with the statuses and bodies the
//! protocol defines, and pulling an engine in to produce them would defeat the
//! point of a crate that does not depend on `mdcast`.

use mdcast_api::Target;
use mdcast_api::wire::{
    BlobCheckResponse, ErrorCode, ErrorResponse, MissingBlob, MissingBlobsError,
    RenderMarkdownRequest, TemplateFormat,
};
use mdcast_client::{AssetBundle, Client, Error, request, sha256};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "correct-horse-battery-staple";
const CHART: &[u8] = b"<svg id='chart'/>";
const LOGO: &[u8] = b"<svg id='logo'/>";
const PDF: &[u8] = b"%PDF-1.7 rendered";

const RENDER: &str = "/v1/render";
const RENDER_TEMPLATE: &str = "/v1/render/template";
const BLOBS: &str = "/v1/blobs";
const BLOBS_CHECK: &str = "/v1/blobs/check";

fn bundle() -> AssetBundle {
    AssetBundle::new()
        .with("img/chart.svg", CHART)
        .with("img/logo.svg", LOGO)
}

fn client(server: &MockServer) -> Client {
    Client::builder(server.uri())
        .token(TOKEN)
        .build()
        .expect("the mock server's URI is a valid base URL")
}

fn markdown() -> RenderMarkdownRequest {
    request::markdown("# Q3\n\n![](img/chart.svg)", Target::Pdf)
}

/// What `mdcast-server`'s `artifact_response` produces on a successful render.
fn artifact_ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_bytes(PDF)
        .insert_header("content-type", "application/pdf")
        .insert_header("content-disposition", "attachment; filename=\"output.pdf\"")
}

/// The `409` body: only the chart is missing — the logo is already cached.
fn chart_missing() -> MissingBlobsError {
    MissingBlobsError {
        message: "1 asset(s) referenced by the manifest are not on the server; \
                  upload them and retry"
            .to_string(),
        missing: vec![MissingBlob {
            key: "img/chart.svg".to_string(),
            digest: sha256(CHART),
        }],
    }
}

async fn requests_to(server: &MockServer, route: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("wiremock records requests by default")
        .into_iter()
        .filter(|request| request.url.path() == route)
        .collect()
}

async fn mount_json(server: &MockServer, route: &str, status: u16, body: impl serde::Serialize) {
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_warm_cache_renders_in_a_single_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(artifact_ok())
        .expect(1)
        .mount(&server)
        .await;

    let artifact = client(&server)
        .render(markdown(), &bundle())
        .await
        .expect("a warm cache renders straight through");

    assert_eq!(&artifact.bytes[..], PDF);
    assert_eq!(artifact.filename, "output.pdf");
    assert_eq!(artifact.content_type, "application/pdf");
    assert!(
        requests_to(&server, BLOBS).await.is_empty(),
        "nothing may be uploaded when the server asked for nothing"
    );
}

#[tokio::test]
async fn the_manifest_is_built_from_the_bundle_without_the_caller_hashing_anything() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(artifact_ok())
        .mount(&server)
        .await;

    client(&server)
        .render(markdown(), &bundle())
        .await
        .expect("renders");

    let sent = requests_to(&server, RENDER).await;
    let body: RenderMarkdownRequest =
        serde_json::from_slice(&sent[0].body).expect("the client posts a RenderMarkdownRequest");

    assert_eq!(body.assets.get("img/chart.svg"), Some(&sha256(CHART)));
    assert_eq!(body.assets.get("img/logo.svg"), Some(&sha256(LOGO)));
    assert_eq!(body.assets.len(), 2);
}

#[tokio::test]
async fn a_cold_cache_uploads_only_the_missing_blob_and_retries() {
    let server = MockServer::start().await;
    // Mounted first and capped at one match, so the retry falls through to the
    // success mock below — wiremock keeps registration order within a priority.
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(ResponseTemplate::new(409).set_body_json(chart_missing()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(artifact_ok())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(BLOBS))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let artifact = client(&server)
        .render(markdown(), &bundle())
        .await
        .expect("the retry after the upload succeeds");

    assert_eq!(&artifact.bytes[..], PDF);
    assert_eq!(
        requests_to(&server, RENDER).await.len(),
        2,
        "the identical request is posted again after the upload"
    );

    let uploads = requests_to(&server, BLOBS).await;
    assert_eq!(uploads.len(), 1, "one request satisfies the whole 409");
    let body = String::from_utf8_lossy(&uploads[0].body);
    assert!(
        body.contains(&format!("name=\"{}\"", sha256(CHART))),
        "the part name is the claimed digest: {body}"
    );
    assert!(
        body.contains("<svg id='chart'/>"),
        "the missing blob's bytes ride along: {body}"
    );
    assert!(
        !body.contains(&sha256(LOGO).to_string()) && !body.contains("<svg id='logo'/>"),
        "only the missing blob is uploaded, never the whole bundle: {body}"
    );
}

#[tokio::test]
async fn a_warm_cache_never_fetches_a_digest_only_entrys_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(artifact_ok())
        .expect(1)
        .mount(&server)
        .await;

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&calls);
    let bundle = AssetBundle::new().with_digest("img/chart.svg", sha256(CHART), move || {
        let counted = std::sync::Arc::clone(&counted);
        async move {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<bytes::Bytes, std::convert::Infallible>(bytes::Bytes::from_static(CHART))
        }
    });

    client(&server)
        .render(markdown(), &bundle)
        .await
        .expect("a warm cache renders straight through");

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a warm cache must never call a digest-only entry's fetch"
    );
}

#[tokio::test]
async fn a_409_naming_a_digest_only_entry_fetches_and_uploads_it_lazily() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(ResponseTemplate::new(409).set_body_json(chart_missing()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(artifact_ok())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(BLOBS))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&calls);
    let bundle = AssetBundle::new().with_digest("img/chart.svg", sha256(CHART), move || {
        let counted = std::sync::Arc::clone(&counted);
        async move {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<bytes::Bytes, std::convert::Infallible>(bytes::Bytes::from_static(CHART))
        }
    });

    let artifact = client(&server)
        .render(markdown(), &bundle)
        .await
        .expect("the retry after the lazily-fetched upload succeeds");

    assert_eq!(&artifact.bytes[..], PDF);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the fetch runs exactly once, only because the server named this digest"
    );
    let uploads = requests_to(&server, BLOBS).await;
    let body = String::from_utf8_lossy(&uploads[0].body);
    assert!(
        body.contains("<svg id='chart'/>"),
        "the lazily-fetched bytes ride along: {body}"
    );
}

#[tokio::test]
async fn a_second_409_surfaces_as_its_own_error_instead_of_looping() {
    let server = MockServer::start().await;
    mount_json(&server, RENDER, 409, chart_missing()).await;
    Mock::given(method("POST"))
        .and(path(BLOBS))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .render(markdown(), &bundle())
        .await
        .expect_err("a 409 that survives the retry is a failure, not a retry signal");

    match error {
        Error::BlobsStillMissing { missing } => {
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].key, "img/chart.svg");
        }
        other => panic!("expected BlobsStillMissing, got {other:?}"),
    }
    assert_eq!(
        requests_to(&server, RENDER).await.len(),
        2,
        "exactly one retry — a loop here would hang instead of reporting"
    );
    assert_eq!(requests_to(&server, BLOBS).await.len(), 1);
}

#[tokio::test]
async fn a_401_is_reported_as_unauthorized() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        RENDER,
        401,
        ErrorResponse::new(ErrorCode::Unauthorized, "missing or invalid bearer token"),
    )
    .await;

    let error = client(&server)
        .render(markdown(), &bundle())
        .await
        .expect_err("a rejected token is an error");

    match error {
        Error::Unauthorized { message } => assert!(message.contains("bearer token"), "{message}"),
        other => panic!("expected Unauthorized, got {other:?}"),
    }
}

#[tokio::test]
async fn a_server_error_response_arrives_with_its_code_intact() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        RENDER,
        422,
        ErrorResponse::new(
            ErrorCode::RenderFailed,
            "render failed (incident 4711-0); see the server log for details",
        ),
    )
    .await;

    let error = client(&server)
        .render(markdown(), &bundle())
        .await
        .expect_err("a failed render is an error");

    match error {
        Error::Server {
            status: 422,
            code: ErrorCode::RenderFailed,
            message,
            ..
        } => assert!(message.contains("incident 4711-0"), "{message}"),
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn preflight_uploads_what_the_check_reports_and_the_render_never_409s() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        BLOBS_CHECK,
        200,
        BlobCheckResponse {
            missing: vec![sha256(CHART)],
        },
    )
    .await;
    Mock::given(method("POST"))
        .and(path(BLOBS))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(artifact_ok())
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let bundle = bundle();

    let uploaded = client.preflight(&bundle).await.expect("preflight succeeds");
    client.render(markdown(), &bundle).await.expect("renders");

    assert_eq!(uploaded, vec![sha256(CHART)]);
    assert_eq!(
        requests_to(&server, RENDER).await.len(),
        1,
        "the pessimistic path pays a check round trip so the render cannot 409"
    );
    let uploads = requests_to(&server, BLOBS).await;
    let body = String::from_utf8_lossy(&uploads[0].body);
    assert!(!body.contains("<svg id='logo'/>"), "{body}");
}

#[tokio::test]
async fn a_template_render_goes_to_the_template_route_and_names_its_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RENDER_TEMPLATE))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"<html/>".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = request::template("= Invoice", r#"{"number":"INV-042"}"#);
    req.format = TemplateFormat::Html;

    let artifact = client(&server)
        .render_template(req, &AssetBundle::new())
        .await
        .expect("renders");

    assert_eq!(&artifact.bytes[..], b"<html/>");
    assert_eq!(
        artifact.filename, "output.html",
        "a response without Content-Disposition falls back to the format's own name"
    );
}

#[tokio::test]
async fn a_caller_supplied_http_client_is_the_one_that_sends() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RENDER))
        .respond_with(artifact_ok())
        .mount(&server)
        .await;

    let http = reqwest::Client::builder()
        .user_agent("caller-supplied/1")
        .build()
        .expect("a reqwest client builds");
    let client = Client::builder(server.uri())
        .token(TOKEN)
        .http_client(http)
        .build()
        .expect("valid configuration");

    client.render(markdown(), &bundle()).await.expect("renders");

    let sent = requests_to(&server, RENDER).await;
    assert_eq!(
        sent[0].headers.get("user-agent").map(|v| v.as_bytes()),
        Some(&b"caller-supplied/1"[..]),
        "the injected pool, not a freshly built default one"
    );
}
