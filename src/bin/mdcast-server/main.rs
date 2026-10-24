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
//! main.rs      wiring: config → blob store → prefill → registry → router → serve
//! config.rs    MDCAST_* environment configuration
//! auth.rs      shared-bearer-token gate for /v1
//! error.rs     ApiError → ErrorResponse/status, with internal detail kept in the log
//! pandoc.rs    startup probe for a working `pandoc` binary (issue #81)
//! render_gate.rs  global render concurrency limit + queue timeout (issue #96)
//! blobs/       content-addressed store + the manifest-backed AssetProvider + startup prefill
//! routes/      router, /health, /v1/blobs*, /v1/render, /v1/render/template, /v1/capabilities
//! ```
//!
//! **Compiles run in a child process by default (`MDCAST_TYPST=cli`, issue
//! #96; see README's "Known limitations"):** each compile is a killable
//! `typst` child — `MDCAST_TYPST_TIMEOUT_SECS` bounds it with a kill+reap,
//! `MDCAST_TYPST_MAX_MEMORY_BYTES` / `MDCAST_TYPST_MAX_OUTPUT_BYTES` cap its
//! address space and output, and it sees neither system fonts, packages, the
//! network nor this process's environment. `MDCAST_TYPST=library` stays
//! selectable, but there a template that loops forever wedges a
//! `spawn_blocking` thread for the life of the process (it cannot be
//! cancelled) with no time or memory limit — only for trusted callers.

use std::sync::Arc;

use anyhow::{Context, Result};
use mdcast::backends::Registry;
use mdcast::backends::typst::{OutlineCommand, TypstCompiler};

mod auth;
mod blobs;
mod config;
mod error;
mod pandoc;
mod render_gate;
mod routes;

use blobs::BlobStore;
use config::Config;
use routes::AppState;

/// Sync on purpose: the hidden SVG-outline child mode (spawned by this same
/// binary when `MDCAST_TYPST=cli`) must be dispatched before config parsing
/// (it needs no `MDCAST_*`), before a runtime starts (its worker threads
/// count against the child's `RLIMIT_AS`) and before the log subscriber is
/// installed (stdout is the child's response channel).
fn main() -> Result<()> {
    use mdcast::backends::typst::{OUTLINE_CHILD_ARG, run_outline_child};
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == OUTLINE_CHILD_ARG)
    {
        return run_outline_child(std::io::stdin().lock(), std::io::stdout().lock());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start the tokio runtime")?
        .block_on(serve())
}

async fn serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut config = Config::from_env()?;
    config.typst_compiler = outline_in_self(config.typst_compiler)?;
    warn_if_unauthenticated(&config);
    // Best-effort: logs the CLI binary's reported version so version skew
    // between it and mdcast's linked `typst` crate is visible at startup
    // rather than discovered as a rendering difference (issue #73). A no-op
    // when `typst_compiler` is `Library`.
    config.typst_compiler.log_startup_info().await;
    // Feeds GET /v1/capabilities (issue #81): the pandoc-backed targets only
    // advertise as available when a real binary answered at startup.
    let pandoc_available = pandoc::probe_pandoc().await;
    let store = BlobStore::new(
        config.max_cache_bytes,
        config.cache_lives,
        config.max_cache_lives,
    )?;
    if let Some(prefill_dir) = &config.prefill_dir {
        let summary = blobs::prefill::prefill(&store, prefill_dir)
            .await
            .with_context(|| format!("prefill blob store from {}", prefill_dir.display()))?;
        tracing::info!(
            ingested = summary.ingested,
            already_present = summary.already_present,
            bytes = summary.bytes,
            pinned_bytes = store.pinned_bytes(),
            "blob store prefill complete"
        );
    }
    let state = Arc::new(AppState {
        store: Arc::new(store),
        registry: Registry::with_typst_compiler(config.typst_compiler.clone())
            .with_pandoc_limits(config.pandoc_timeout, config.pandoc_max_memory_bytes),
        typst_compiler: config.typst_compiler.clone(),
        pandoc_available,
        max_upload_bytes: config.max_upload_bytes as u64,
        render_gate: render_gate::RenderGate::new(
            config.max_concurrent_renders,
            config.render_queue,
        ),
        expose_diagnostics: config.expose_diagnostics,
    });
    let app = routes::router(state, config.token.as_deref(), config.max_upload_bytes);

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    tracing::info!(
        bind = %config.bind,
        max_cache_bytes = config.max_cache_bytes,
        max_upload_bytes = config.max_upload_bytes,
        cache_lives = config.cache_lives,
        max_cache_lives = config.max_cache_lives,
        typst_compiler = ?config.typst_compiler,
        max_concurrent_renders = config.max_concurrent_renders,
        render_queue_secs = config.render_queue.as_secs(),
        pandoc_timeout_secs = config.pandoc_timeout.as_secs(),
        expose_diagnostics = config.expose_diagnostics,
        pandoc_available,
        "mdcast-server listening"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve http")
}

/// `cli` mode runs the SVG-outline step as this binary's hidden child mode,
/// under the same timeout and memory cap as the typst compile child.
fn outline_in_self(compiler: TypstCompiler) -> Result<TypstCompiler> {
    Ok(match compiler {
        TypstCompiler::Cli(cli) => {
            TypstCompiler::Cli(cli.with_outline(Some(OutlineCommand::current_exe()?)))
        }
        other => other,
    })
}

/// The server runs as PID 1 in the container, where an unhandled SIGTERM is
/// ignored — without this every rollout would wait out the grace period and
/// SIGKILL in-flight renders instead of draining them.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "install Ctrl-C handler");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
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
