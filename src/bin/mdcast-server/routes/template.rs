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
//!
//! `generated` entries (a payment QR code, say) are rendered up front and
//! served at their `path` over everything else. A bad entry — unencodable
//! data, or a path the manifest or the template itself already claims — is a
//! `400` naming the entry: unlike a compile failure, it is entirely the
//! request's fault and safe to describe.
//!
//! HTML output (`format: "html"`) is available whenever `state.typst_compiler`
//! is `Cli` — a real `typst` binary supports `--features html` regardless of
//! how mdcast itself was built (issue #73) — and otherwise falls back to the
//! `Library`-only path gated by the `typst-html` cargo feature, unchanged
//! from before issue #73.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::response::Response;
use bytes::Bytes;
use mdcast::backends::typst::{TemplateDoc, TypstCompiler, render_template_with};
use mdcast::mdcast_api::AssetManifest;
use mdcast::mdcast_api::wire::{GeneratedAsset, RenderTemplateRequest, TemplateFormat};
use mdcast::{AssetProvider, BrandHandle, LayeredAssets, MemoryAssets, sync_provider};

use crate::blobs::BlobAssets;
use crate::error::{ApiError, json_body, render_failed};

#[cfg(not(feature = "typst-html"))]
use super::capabilities::html_template_export_available;
use super::{AppState, artifact_response};

/// Virtual key the submitted template source is served under. At the root, so
/// a `#import "partials/header.typ"` resolves against the manifest keys the
/// caller uploaded, exactly as it would against a filesystem.
const TEMPLATE_KEY: &str = "main.typ";

#[cfg(not(feature = "typst-html"))]
const HTML_UNSUPPORTED: &str = "this server was built without the `typst-html` feature; \
     only \"pdf\" template output is available in `library` mode — set \
     MDCAST_TYPST=cli to get HTML output without this build feature";

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
        fonts,
        generated,
    } = json_body(body)?;

    // First: an output this server cannot produce must not cost the caller a
    // 409 upload round trip or a render slot before the 501.
    #[cfg(not(feature = "typst-html"))]
    if matches!(format, TemplateFormat::Html)
        && !html_template_export_available(&state.typst_compiler)
    {
        return Err(ApiError::Unsupported(HTML_UNSUPPORTED.into()));
    }

    // `data` travels as raw JSON text and ends up in the virtual /data.json.
    // Parsing it here is not a round trip for its own sake: it turns "your
    // JSON is malformed" into a 400 instead of a confusing typst error.
    let data = serde_json::from_str(&data)
        .map_err(|e| ApiError::BadRequest(format!("`data` is not valid JSON: {e}")))?;

    // Before blob resolution: a request that can never render should not
    // cost the caller an upload round trip first.
    let generated = render_generated(&generated, &assets)?;

    let blobs = BlobAssets::resolve(state.store.clone(), assets)
        .await
        .map_err(ApiError::MissingBlobs)?;
    // Only now: a 400 or a 409 round trip never occupies a render slot.
    let _slot = state.render_gate.enter().await?;
    blobs.recheck().map_err(ApiError::MissingBlobs)?;
    let provider = LayeredAssets {
        over: generated,
        base: LayeredAssets {
            over: sync_provider(move |key| {
                Ok((key == TEMPLATE_KEY).then(|| Bytes::from(template_source.clone())))
            }),
            base: blobs,
        },
    };

    let doc = TemplateDoc {
        template: TEMPLATE_KEY.to_string(),
        data,
        meta,
        brand: BrandHandle(Arc::new(brand.unwrap_or_default())),
        fonts,
    };

    tracing::info!(format = ?format, "rendering template");
    match format {
        TemplateFormat::Pdf => {
            let artifact = render_template_with(&doc, &provider, &state.typst_compiler)
                .await
                .map_err(render_failed(state.expose_diagnostics))?;
            Ok(artifact_response("application/pdf", artifact))
        }
        TemplateFormat::Html => html(&doc, &provider, &state).await,
    }
}

fn render_generated(
    generated: &[GeneratedAsset],
    manifest: &AssetManifest,
) -> Result<MemoryAssets, ApiError> {
    let mut files = BTreeMap::new();
    for GeneratedAsset { path, spec } in generated {
        if path == TEMPLATE_KEY || manifest.get(path).is_some() || files.contains_key(path) {
            return Err(ApiError::BadRequest(format!(
                "generated path {path:?} collides with a template asset or another generated entry"
            )));
        }
        let bytes = mdcast::generators::render_spec(spec)
            .map_err(|e| ApiError::BadRequest(format!("generated entry {path:?} failed: {e:#}")))?;
        files.insert(path.clone(), bytes);
    }
    Ok(MemoryAssets(files))
}

/// `Cli` mode needs no build-time toggle for HTML export (issue #73) — a real
/// `typst` binary either supports `--features html` or it does not, decided
/// per invocation. `Library` mode keeps its pre-issue-#73 behaviour exactly:
/// gated behind the `typst-html` cargo feature, `501` without it.
async fn html(
    doc: &TemplateDoc,
    provider: &dyn AssetProvider,
    state: &AppState,
) -> Result<Response, ApiError> {
    let compiler = &state.typst_compiler;
    match compiler {
        TypstCompiler::Cli(_) => {
            let artifact =
                mdcast::backends::typst::render_template_html_with(doc, provider, compiler)
                    .await
                    .map_err(render_failed(state.expose_diagnostics))?;
            Ok(artifact_response("text/html; charset=utf-8", artifact))
        }
        TypstCompiler::Library => html_library(doc, provider, state.expose_diagnostics).await,
    }
}

#[cfg(feature = "typst-html")]
async fn html_library(
    doc: &TemplateDoc,
    provider: &dyn AssetProvider,
    expose_diagnostics: bool,
) -> Result<Response, ApiError> {
    let artifact = mdcast::backends::typst::render_template_html(doc, provider)
        .await
        .map_err(render_failed(expose_diagnostics))?;
    Ok(artifact_response("text/html; charset=utf-8", artifact))
}

/// HTML export via the in-process compiler lives behind the off-by-default
/// `typst-html` feature. A server built without it, and configured with
/// `MDCAST_TYPST=library`, says so rather than panicking or quietly handing
/// back a PDF with the wrong content type — set `MDCAST_TYPST=cli` instead to
/// get HTML output without this build feature.
#[cfg(not(feature = "typst-html"))]
async fn html_library(
    _doc: &TemplateDoc,
    _provider: &dyn AssetProvider,
    _expose_diagnostics: bool,
) -> Result<Response, ApiError> {
    Err(ApiError::Unsupported(HTML_UNSUPPORTED.into()))
}
