//! HTTP surface: the router, the shared state handlers run against, and the
//! artifact response both render endpoints produce.
//!
//! | Method + path        | Auth | Body → response |
//! |----------------------|------|-----------------|
//! | `GET /health`        | no   | `ok` |
//! | `GET /v1/capabilities` | no | → `CapabilitiesResponse` |
//! | `POST /v1/blobs/check` | if configured | `BlobCheckRequest` → `BlobCheckResponse` |
//! | `POST /v1/blobs`     | if configured  | multipart upload → `204` |
//! | `POST /v1/render`    | if configured  | `RenderMarkdownRequest` → artifact bytes |
//! | `POST /v1/render/template` | if configured | `RenderTemplateRequest` → artifact bytes |
//!
//! "if configured" means `MDCAST_TOKEN` was set at startup — see `config.rs`
//! and `main::warn_if_unauthenticated`. When it wasn't, every `/v1` route is
//! open, same as `/health`. `GET /v1/capabilities` is never gated, in either
//! mode — see `capabilities.rs`'s module docs for why.

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use mdcast::RenderedArtifact;
use mdcast::backends::Registry;
use mdcast::backends::typst::TypstCompiler;
use mdcast::mdcast_api::Target;

use crate::auth::require_bearer;
use crate::blobs::BlobStore;

mod blobs;
mod capabilities;
mod render;
mod template;

pub struct AppState {
    pub store: Arc<BlobStore>,
    pub registry: Registry,
    /// Used directly by `/v1/render/template`'s HTML arm, which — unlike
    /// `/v1/render` — doesn't go through `Registry` (see `template.rs`'s
    /// module docs on why the template path bypasses `Backend`/`Registry`
    /// entirely). Also read by `capabilities.rs` to report whether `html`
    /// template export is currently available.
    pub typst_compiler: TypstCompiler,
    /// Whether a working `pandoc` binary was found at startup
    /// (`pandoc::probe_pandoc`) — gates the pandoc-backed targets reported by
    /// `GET /v1/capabilities`.
    pub pandoc_available: bool,
    /// Echoed by `GET /v1/capabilities` — `MDCAST_MAX_UPLOAD_BYTES`, so a
    /// client can reject an oversized bundle before any round trip.
    pub max_upload_bytes: u64,
}

/// Everything under `/v1` sits behind the bearer gate when `token` is
/// `Some` — `None` means `MDCAST_TOKEN` was unset at startup, and the layer
/// is skipped entirely rather than kept and made to pass everything, so an
/// authless deployment can never regress into "always authorised" by
/// accident. `/health` is never gated, so a load balancer or `docker
/// healthcheck` needs no credentials either way. The body limit is applied
/// once, at the outermost layer, so it covers render requests as well as
/// uploads — one knob, no way around it.
pub fn router(state: Arc<AppState>, token: Option<&str>, max_body_bytes: usize) -> Router {
    let mut v1 = Router::new()
        .route("/blobs/check", post(blobs::check))
        .route("/blobs", post(blobs::upload))
        .route("/render", post(render::markdown))
        .route("/render/template", post(template::render));
    if let Some(token) = token {
        v1 = v1.route_layer(axum::middleware::from_fn_with_state(
            Arc::<str>::from(token),
            require_bearer,
        ));
    }
    // Added after `route_layer` so it is never wrapped by the bearer gate —
    // `route_layer` only wraps routes already registered on the router at
    // the point it's called, not ones added afterward. See `capabilities.rs`
    // for why this endpoint is deliberately unauthenticated.
    v1 = v1.route("/capabilities", get(capabilities::capabilities));

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
