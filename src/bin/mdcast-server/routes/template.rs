//! `POST /v1/render/template` — the data-driven typst path (`TemplateDoc`),
//! which bypasses `ResolvedDoc` entirely.
//!
//! Two things differ from the CLI's `render-template`. The template arrives as
//! *source*, not as a provider key, so it is served to the engine under a
//! synthetic root key. And the provider stack deliberately stops at the
//! request's own blobs — `EmbeddedAssets` is **not** layered underneath,
//! because `render_template` discovers sibling files by listing the template's
//! directory: with the template at the root, that listing is the manifest, and
//! layering the embedded catalog under it would register the entire vendored
//! reveal.js tree and the reference documents as virtual files on every
//! render.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::response::Response;
use bytes::Bytes;
use mdcast::backends::typst::{TemplateDoc, render_template};
use mdcast::mdcast_api::wire::{RenderTemplateRequest, TemplateFormat};
use mdcast::{AssetProvider, BrandHandle, LayeredAssets, sync_provider};

use crate::blobs::BlobAssets;
use crate::error::{ApiError, json_body};

use super::{AppState, artifact_response};

/// Virtual key the submitted template source is served under. At the root, so
/// a `#import "partials/header.typ"` resolves against the manifest keys the
/// caller uploaded, exactly as it would against a filesystem.
const TEMPLATE_KEY: &str = "main.typ";

pub async fn render(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RenderTemplateRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let RenderTemplateRequest {
        template_source,
        data,
        meta,
        brand,
        format,
        assets,
    } = json_body(body)?;

    // `data` travels as raw JSON text and ends up in the virtual /data.json.
    // Parsing it here is not a round trip for its own sake: it turns "your
    // JSON is malformed" into a 400 instead of a confusing typst error.
    let data = serde_json::from_str(&data)
        .map_err(|e| ApiError::BadRequest(format!("`data` is not valid JSON: {e}")))?;

    let blobs = BlobAssets::resolve(state.store.clone(), assets)
        .await
        .map_err(ApiError::MissingBlobs)?;
    let provider = LayeredAssets {
        over: sync_provider(move |key| {
            Ok((key == TEMPLATE_KEY).then(|| Bytes::from(template_source.clone())))
        }),
        base: blobs,
    };

    let doc = TemplateDoc {
        template: TEMPLATE_KEY.to_string(),
        data,
        meta,
        brand: BrandHandle(Arc::new(brand.unwrap_or_default())),
    };

    tracing::info!(format = ?format, "rendering template");
    match format {
        TemplateFormat::Pdf => {
            let artifact = render_template(&doc, &provider)
                .await
                .map_err(ApiError::RenderFailed)?;
            Ok(artifact_response("application/pdf", artifact))
        }
        TemplateFormat::Html => html(&doc, &provider).await,
    }
}

#[cfg(feature = "typst-html")]
async fn html(doc: &TemplateDoc, provider: &dyn AssetProvider) -> Result<Response, ApiError> {
    let artifact = mdcast::backends::typst::render_template_html(doc, provider)
        .await
        .map_err(ApiError::RenderFailed)?;
    Ok(artifact_response("text/html; charset=utf-8", artifact))
}

/// HTML export lives behind the off-by-default `typst-html` feature. A server
/// built without it says so, rather than panicking or quietly handing back a
/// PDF with the wrong content type.
#[cfg(not(feature = "typst-html"))]
async fn html(_doc: &TemplateDoc, _provider: &dyn AssetProvider) -> Result<Response, ApiError> {
    Err(ApiError::Unsupported(
        "this server was built without the `typst-html` feature; \
         only \"pdf\" template output is available"
            .into(),
    ))
}
