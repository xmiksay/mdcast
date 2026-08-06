//! `Client::capabilities()` against a canned `GET /v1/capabilities`. Split
//! out of `negotiation.rs` to keep both files under the size cap.

use mdcast_api::Target;
use mdcast_api::wire::{CapabilitiesResponse, TemplateFormat};
use mdcast_client::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "correct-horse-battery-staple";
const CAPABILITIES: &str = "/v1/capabilities";

fn client(server: &MockServer) -> Client {
    Client::builder(server.uri())
        .token(TOKEN)
        .build()
        .expect("the mock server's URI is a valid base URL")
}

fn response() -> CapabilitiesResponse {
    CapabilitiesResponse {
        targets: vec![Target::Pdf, Target::PdfPresentation],
        template_formats: vec![TemplateFormat::Pdf],
        max_upload_bytes: 268_435_456,
        version: "0.3.0".to_string(),
    }
}

#[tokio::test]
async fn decodes_the_response_into_a_typed_value() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CAPABILITIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(response()))
        .expect(1)
        .mount(&server)
        .await;

    let capabilities = client(&server)
        .capabilities()
        .await
        .expect("a well-formed 200 decodes");

    assert_eq!(capabilities, response());
}

/// The server-side route is unauthenticated, but the client still has to be
/// built with a token (`Client::builder(...).build()` requires one) — this
/// just confirms the request the client actually sends doesn't depend on the
/// server checking it.
#[tokio::test]
async fn works_against_an_unauthenticated_route() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CAPABILITIES))
        .respond_with(ResponseTemplate::new(200).set_body_json(response()))
        .mount(&server)
        .await;

    client(&server)
        .capabilities()
        .await
        .expect("no authorization header is required by this mock, and the call still succeeds");
}
