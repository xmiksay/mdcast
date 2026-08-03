//! Blob endpoints: the cheap "what are you missing?" probe, and the upload
//! that answers it.
//!
//! Upload is `multipart/form-data`, one blob per part, the part *name* being
//! the sha256 the caller claims for it — so a client can satisfy a whole `409`
//! in a single request. Each part is streamed straight to disk through the
//! hasher; the claimed digest is never trusted, and a part whose bytes hash to
//! something else is rejected without ever becoming addressable.

use std::sync::Arc;

use axum::Json;
use axum::extract::multipart::MultipartError;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use mdcast::mdcast_api::Digest;
use mdcast::mdcast_api::wire::{BlobCheckRequest, BlobCheckResponse};

use crate::blobs::WriteError;
use crate::error::{ApiError, json_body};

use super::AppState;

pub async fn check(
    State(state): State<Arc<AppState>>,
    body: Result<Json<BlobCheckRequest>, JsonRejection>,
) -> Result<Json<BlobCheckResponse>, ApiError> {
    let request = json_body(body)?;
    let mut missing = Vec::new();
    for digest in request.digests {
        if !state.store.contains(&digest).await {
            missing.push(digest);
        }
    }
    Ok(Json(BlobCheckResponse { missing }))
}

pub async fn upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<StatusCode, ApiError> {
    let mut stored = 0usize;
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        let claimed = field_digest(field.name())?;

        let mut writer = state.store.begin_write().await?;
        while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
            writer.write(&chunk).await?;
        }
        writer.finish(&claimed).await?;
        stored += 1;
    }

    tracing::info!(stored, "accepted blob upload");
    // Eviction runs after the upload rather than before: the blobs that just
    // arrived are the most recently used ones, so they are the last to go.
    if let Err(e) = state.store.evict().await {
        tracing::warn!(error = format!("{e:#}"), "blob eviction failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The part name carries the claimed digest. Validating it here means a
/// malformed name is a `400` before a single byte is written.
fn field_digest(name: Option<&str>) -> Result<Digest, ApiError> {
    let Some(name) = name else {
        return Err(ApiError::BadRequest(
            "every multipart part must be named with the sha256 digest of its bytes".into(),
        ));
    };
    Digest::parse(name).map_err(|e| {
        ApiError::BadRequest(format!(
            "multipart part name {name:?} is not a sha256 digest: {e}"
        ))
    })
}

fn multipart_error(e: MultipartError) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::PayloadTooLarge
    } else {
        ApiError::BadRequest(e.body_text())
    }
}

impl From<WriteError> for ApiError {
    fn from(e: WriteError) -> Self {
        match e {
            // The caller's own bytes and the caller's own claim — reporting
            // both is what lets it find the bug on its side.
            WriteError::DigestMismatch { claimed, actual } => ApiError::BadRequest(format!(
                "uploaded bytes hash to {actual}, not the claimed {claimed}"
            )),
            WriteError::Io(e) => ApiError::Internal(e),
        }
    }
}
