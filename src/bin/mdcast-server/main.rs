//! `mdcast-server` — the render pipeline behind an HTTP API, speaking the
//! wire protocol in `mdcast_api::wire`.
//!
//! The service is deliberately small and synchronous: a render request blocks
//! until the artifact exists and the response body *is* the artifact. There is
//! no job queue, no polling, no `202` — one host owns pandoc/typst/fonts, and
//! callers post markdown.
//!
//! Asset bytes never ride along with a render. A request carries an
//! `AssetManifest` (asset key → sha256); if the server lacks any referenced
//! blob it answers `409` with every missing digest at once, the caller uploads
//! just those, and retries the identical request. Because a blob's name is its
//! content hash, a cached blob can never be stale — so there is no
//! invalidation protocol, and eviction is invisible.
//!
//! ```text
//! main.rs      wiring: config → blob store → registry → router → serve
//! config.rs    MDCAST_* environment configuration
//! auth.rs      shared-bearer-token gate for /v1
//! error.rs     ApiError → ErrorResponse/status, with internal detail kept in the log
//! blobs/       content-addressed store + the manifest-backed AssetProvider
//! routes/      router, /health, /v1/blobs*, /v1/render, /v1/render/template
//! ```
//!
//! **Known limitation (deliberate, see README's "Known limitations"):** a
//! caller-supplied typst template that loops forever wedges a
//! `spawn_blocking` thread for the life of the process — `spawn_blocking`
//! cannot be cancelled — and there is no compile time or memory limit. The
//! trust boundary is internal callers, so this is accepted rather than
//! designed around; the fix, if it ever bites, is to put the compile behind a
//! `Compiler` trait and run it in a killable subprocess with rlimits.

use std::sync::Arc;

use anyhow::{Context, Result};
use mdcast::backends::Registry;

mod auth;
mod blobs;
mod config;
mod error;
mod routes;

use blobs::BlobStore;
use config::Config;
use routes::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    warn_if_unauthenticated(&config);
    let store = BlobStore::open(config.blob_dir.clone(), config.max_cache_bytes).await?;
    let state = Arc::new(AppState {
        store: Arc::new(store),
        registry: Registry::with_defaults(),
    });
    let app = routes::router(state, config.token.as_deref(), config.max_upload_bytes);

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    tracing::info!(
        bind = %config.bind,
        blob_dir = %config.blob_dir.display(),
        max_cache_bytes = config.max_cache_bytes,
        max_upload_bytes = config.max_upload_bytes,
        "mdcast-server listening"
    );
    axum::serve(listener, app).await.context("serve http")
}

/// `MDCAST_TOKEN` unset or empty means the router skips `require_bearer`
/// entirely (`routes::router`) — every `/v1` route is then reachable by
/// anyone who can reach `bind`. That's a deliberate, documented mode for a
/// service already sitting behind infrastructure that authenticates callers
/// (ingress, mesh, WireGuard), but it must never be silent: a non-loopback
/// bind with no token is an open render/upload/arbitrary-Typst-execution
/// service on the network, which is worth shouting about louder than a
/// loopback bind whose blast radius is just the host it runs on.
fn warn_if_unauthenticated(config: &Config) {
    if config.token.is_some() {
        return;
    }
    if config.bind.ip().is_loopback() {
        tracing::warn!(
            bind = %config.bind,
            "MDCAST_TOKEN is unset — mdcast-server is running UNAUTHENTICATED. Every caller \
             that can reach the bind address can render, upload blobs, and (via \
             /v1/render/template) execute arbitrary Typst. Set MDCAST_TOKEN unless this \
             instance sits behind infrastructure that already authenticates every caller."
        );
    } else {
        tracing::warn!(
            bind = %config.bind,
            "MDCAST_TOKEN is unset AND MDCAST_BIND is not loopback — mdcast-server is an OPEN, \
             UNAUTHENTICATED render service reachable over the network. Any caller that can \
             reach this address can render, upload blobs, and (via /v1/render/template) \
             execute arbitrary Typst. Set MDCAST_TOKEN now unless this bind address is \
             intentionally exposed behind infrastructure that already authenticates every \
             caller."
        );
    }
}
