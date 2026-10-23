//! `RenderTemplateRequest.generated` reaches the server untouched: the client
//! renders nothing itself and never folds generated paths into the manifest
//! (they have no bytes to hash — the server makes them).

use mdcast_api::wire::{BoardOptions, BoardTheme, GeneratedAsset, GeneratedSpec, QrEcLevel};
use mdcast_client::{AssetBundle, Client, request};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn generated_entries_are_sent_verbatim_and_stay_out_of_the_manifest() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/render/template"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder(server.uri())
        .token("token")
        .build()
        .expect("the mock server's URI is a valid base URL");

    let mut req = request::template("#image(\"qr.svg\")", "{}");
    req.generated = vec![
        GeneratedAsset {
            path: "qr.svg".to_string(),
            spec: GeneratedSpec::Qr {
                data: "SPD*1.0*AM:1".to_string(),
                ec: QrEcLevel::M,
            },
        },
        GeneratedAsset {
            path: "board.svg".to_string(),
            spec: GeneratedSpec::Fen {
                fen: "8/8/8/4k3/8/8/8/4K3".to_string(),
                board: BoardOptions {
                    flip: true,
                    ..BoardOptions::default()
                },
            },
        },
        GeneratedAsset {
            path: "game.svg".to_string(),
            spec: GeneratedSpec::Pgn {
                pgn: "1. e4 e5".to_string(),
                ply: Some(1),
                board: BoardOptions {
                    theme: BoardTheme::Blue,
                    ..BoardOptions::default()
                },
            },
        },
    ];
    client
        .render_template(req, &AssetBundle::new())
        .await
        .expect("renders");

    let sent = &server.received_requests().await.expect("recording is on")[0];
    let body: serde_json::Value = serde_json::from_slice(&sent.body).expect("JSON body");
    assert_eq!(
        body["generated"],
        serde_json::json!([
            {"path": "qr.svg", "kind": "qr", "data": "SPD*1.0*AM:1", "ec": "M"},
            {"path": "board.svg", "kind": "fen", "fen": "8/8/8/4k3/8/8/8/4K3",
             "flip": true, "theme": "brown", "coordinates": true},
            {"path": "game.svg", "kind": "pgn", "pgn": "1. e4 e5", "move": 1,
             "flip": false, "theme": "blue", "coordinates": true},
        ])
    );
    assert_eq!(body["assets"], serde_json::json!({}));
}
