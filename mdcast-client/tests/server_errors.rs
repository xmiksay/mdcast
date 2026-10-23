//! How the client surfaces the hardened server's answers (issue #96): the
//! `503` a full render queue produces, and the opt-in diagnostics `detail` on
//! a failed render. Same canned-server approach as `negotiation.rs`.

use std::time::Duration;

use mdcast_api::Target;
use mdcast_api::wire::{ErrorCode, ErrorResponse};
use mdcast_client::{AssetBundle, Client, Error, request};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn render_against(response: ResponseTemplate) -> Error {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/render"))
        .respond_with(response)
        .mount(&server)
        .await;
    Client::builder(server.uri())
        .token("t")
        .build()
        .unwrap()
        .render(request::markdown("# Hi", Target::Pdf), &AssetBundle::new())
        .await
        .expect_err("the server refused the render")
}

#[tokio::test]
async fn a_full_render_queue_is_overloaded_with_its_retry_after() {
    let error = render_against(
        ResponseTemplate::new(503)
            .insert_header("retry-after", "10")
            .set_body_json(ErrorResponse::new(
                ErrorCode::Overloaded,
                "every render slot is busy; retry later",
            )),
    )
    .await;

    match error {
        Error::Overloaded {
            message,
            retry_after,
        } => {
            assert!(message.contains("render slot"), "{message}");
            assert_eq!(retry_after, Some(Duration::from_secs(10)));
        }
        other => panic!("expected Overloaded, got {other:?}"),
    }
}

#[tokio::test]
async fn render_failed_detail_reaches_the_caller() {
    let error = render_against(
        ResponseTemplate::new(422).set_body_json(
            ErrorResponse::new(ErrorCode::RenderFailed, "render failed (incident 1-0)")
                .with_detail(vec!["main.typ:1:9: error: unclosed delimiter".into()]),
        ),
    )
    .await;

    match error {
        Error::Server {
            status: 422,
            code: ErrorCode::RenderFailed,
            detail,
            ..
        } => assert_eq!(detail, ["main.typ:1:9: error: unclosed delimiter"]),
        other => panic!("expected Server, got {other:?}"),
    }
}

/// A newer server's code must not turn a readable error into a decode
/// failure.
#[tokio::test]
async fn an_unknown_error_code_still_decodes() {
    let error = render_against(ResponseTemplate::new(429).set_body_raw(
        r#"{"code":"slow_down","message":"later"}"#,
        "application/json",
    ))
    .await;

    assert!(
        matches!(
            &error,
            Error::Server { status: 429, code: ErrorCode::Unknown, message, .. } if message == "later"
        ),
        "{error:?}"
    );
}
