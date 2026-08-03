//! HTTP surface: the router, the shared state handlers run against, and the
//! artifact response both render endpoints produce.
//!
//! | Method + path        | Auth | Body → response |
//! |----------------------|------|-----------------|
//! | `GET /health`        | no   | `ok` |
//! | `POST /v1/blobs/check` | yes | `BlobCheckRequest` → `BlobCheckResponse` |
//! | `POST /v1/blobs`     | yes  | multipart upload → `204` |
//! | `POST /v1/render`    | yes  | `RenderMarkdownRequest` → artifact bytes |
//! | `POST /v1/render/template` | yes | `RenderTemplateRequest` → artifact bytes |

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use mdcast::RenderedArtifact;
use mdcast::backends::Registry;
use mdcast::mdcast_api::Target;

use crate::auth::require_bearer;
use crate::blobs::BlobStore;

mod blobs;
mod render;
mod template;

pub struct AppState {
    pub store: Arc<BlobStore>,
    pub registry: Registry,
}

/// Everything under `/v1` sits behind the bearer gate; `/health` deliberately
/// does not, so a load balancer or `docker healthcheck` needs no credentials.
/// The body limit is applied once, at the outermost layer, so it covers render
/// requests as well as uploads — one knob, no way around it.
pub fn router(state: Arc<AppState>, token: &str, max_body_bytes: usize) -> Router {
    let v1 = Router::new()
        .route("/blobs/check", post(blobs::check))
        .route("/blobs", post(blobs::upload))
        .route("/render", post(render::markdown))
        .route("/render/template", post(template::render))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::<str>::from(token),
            require_bearer,
        ));

    Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// Hand the rendered bytes back with the content type the target implies and
/// the filename the backend suggested. `RenderedArtifact::extras` is ignored:
/// every current backend produces none (html-reveal embeds its resources), and
/// a single response body can only carry the primary artifact anyway.
fn artifact_response(content_type: &'static str, artifact: RenderedArtifact) -> Response {
    (
        [
            (CONTENT_TYPE, content_type.to_string()),
            (
                CONTENT_DISPOSITION,
                // The filename is `output.<ext>`, minted by the backend from
                // the target — never caller-controlled, so nothing to escape.
                format!("attachment; filename=\"{}\"", artifact.filename),
            ),
        ],
        artifact.primary,
    )
        .into_response()
}

fn content_type(target: Target) -> &'static str {
    match target {
        Target::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Target::Odt => "application/vnd.oasis.opendocument.text",
        Target::Pptx => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Target::Pdf | Target::PdfPresentation => "application/pdf",
        Target::HtmlReveal => "text/html; charset=utf-8",
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
