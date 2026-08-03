//! HTTP wire protocol: the request and response bodies `mdcast-server` and
//! `mdcast-client` exchange. Types only — no transport, no handler logic.
//!
//! The shape of the exchange:
//!
//! 1. The client posts a render request ([`RenderMarkdownRequest`] or
//!    [`RenderTemplateRequest`]) whose [`crate::AssetManifest`] names the
//!    assets the render needs, by digest — never inline bytes.
//! 2. If the server holds every digest, it renders and answers with the
//!    artifact bytes.
//! 3. If it does not, it answers `409 Conflict` with a
//!    [`MissingBlobsError`]; the client uploads those blobs and retries.
//!    [`BlobCheckRequest`]/[`BlobCheckResponse`] let a client skip a doomed
//!    render round-trip and ask up front.
//! 4. Any other failure answers with an [`ErrorResponse`].

pub mod blob;
pub mod error;
pub mod render;

pub use blob::{BlobCheckRequest, BlobCheckResponse, MissingBlob, MissingBlobsError};
pub use error::{ErrorCode, ErrorResponse};
pub use render::{RenderMarkdownRequest, RenderOptions, RenderTemplateRequest, TemplateFormat};
