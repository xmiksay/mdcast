//! Async HTTP client for [`mdcast-server`], with the content-addressed asset
//! negotiation made invisible.
//!
//! Hand it a document and a bag of assets. It hashes them, builds the
//! [`AssetManifest`], posts the render, and — if the server answers `409` with
//! blobs it does not hold — uploads exactly those and retries once. You never
//! compute a digest, never build a manifest, and never write a retry loop.
//!
//! ```no_run
//! use mdcast_client::{AssetBundle, Client, request};
//! use mdcast_api::Target;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::builder("http://mdcast.internal:8080")
//!     .token(std::env::var("MDCAST_TOKEN")?)
//!     .build()?;
//!
//! let bundle = AssetBundle::new().with("img/chart.svg", std::fs::read("chart.svg")?);
//! let artifact = client
//!     .render(request::markdown("# Q3\n\n![](img/chart.svg)", Target::Pdf), &bundle)
//!     .await?;
//!
//! std::fs::write(&artifact.filename, &artifact.bytes)?;
//! # Ok(())
//! # }
//! ```
//!
//! That is the whole cold-cache exchange: three requests (`409` → upload →
//! `200`) behind one `await`. A warm cache costs one.
//!
//! ## Why this crate does not depend on `mdcast`
//!
//! Speaking the protocol needs a handful of serde structs, not a rendering
//! engine — so this crate depends on [`mdcast_api`] and an HTTP client, and
//! **never on `mdcast` itself**, not even with `default-features = false`.
//! Depending on it would compile `rust-embed` (which pulls the whole vendored
//! reveal.js tree and the reference OOXML documents in through a proc macro),
//! `clap`, `tempfile`, `toml`, `regex`, `pulldown-cmark`, `futures`,
//! `tracing-subscriber`, `typst` and pandoc's zip/xml stack into a process
//! whose entire job is to POST some JSON. That cost is exactly what the
//! workspace split exists to prevent (`PROJECT_PLAN.md` §7.1).
//!
//! ## Module map
//!
//! | Module | Contents |
//! |---|---|
//! | [`client`] | [`Client`] + [`ClientBuilder`] — render, with the `409` retry folded in |
//! | [`blobs`] | [`Client::check`] / [`Client::upload`] / [`Client::preflight`] — the explicit, pessimistic path |
//! | [`bundle`] | [`AssetBundle`] — `key → bytes`, and the hashing that turns it into a manifest |
//! | [`artifact`] | [`Artifact`] — bytes, filename, content type |
//! | [`request`] | constructors for the two wire request bodies |
//! | [`error`] | [`Error`] — transport, `401`, `413`, a `409` that survived the retry, a server [`ErrorResponse`], a bundle that cannot satisfy its manifest |
//!
//! ## Two strategies
//!
//! [`Client::render`] is optimistic: assume the server has the assets, react
//! to a `409` if it does not. That is one request in the steady state and
//! three on a cold cache, and it is the default.
//!
//! A caller that would rather never discover a large upload mid-render calls
//! [`Client::preflight`] first — `POST /v1/blobs/check`, upload what came
//! back, then render — trading a guaranteed extra round trip for a render that
//! cannot `409`.
//!
//! [`mdcast-server`]: https://github.com/xmiksay/mdcast#http-service-server-feature
//! [`AssetManifest`]: mdcast_api::AssetManifest
//! [`ErrorResponse`]: mdcast_api::wire::ErrorResponse

pub mod artifact;
pub mod blobs;
pub mod bundle;
pub mod client;
pub mod error;
pub mod request;

pub use artifact::Artifact;
pub use bundle::{AssetBundle, BlobFetchError, sha256};
pub use client::{Client, ClientBuilder};
pub use error::{Error, Result};

/// The wire protocol itself, re-exported so a consumer needs exactly one
/// dependency line: `Target`, `DocMeta`, `BrandSpec`, `AssetManifest`,
/// `ErrorCode` and the request bodies all live here.
pub use mdcast_api;
