//! Shared data types for mdcast: the document/brand model plus the HTTP wire
//! protocol spoken between `mdcast-server` and `mdcast-client`.
//!
//! This crate exists so a client does not have to build mdcast itself. The
//! `mdcast` crate unconditionally pulls `clap`, `rust-embed` (which compiles
//! the whole vendored `embedded/` tree through a proc macro), `tempfile`,
//! `toml`, `regex`, `pulldown-cmark`, `futures`, `tracing-subscriber` and a
//! fat `tokio` — a price nobody should pay for a handful of structs. So the
//! types both sides need live here, and depend on **serde and nothing else**.
//!
//! Nothing in here does IO or returns `anyhow::Result` by default. The
//! loaders that do (`frontmatter::extract`, the `anyhow`-wrapped
//! `brand::from_toml`, …) stay in `mdcast` and operate on these types, which
//! `mdcast` re-exports from their original paths (`mdcast::Target`,
//! `mdcast::BrandSpec`, `mdcast::DocMeta`, …). The one exception is
//! [`BrandSpec::from_toml`](brand::BrandSpec::from_toml), gated behind this
//! crate's optional `toml` feature (issue #80), for a caller that has a
//! `brand.toml` but must not compile `mdcast` to parse it.
//!
//! ## Module map
//!
//! | Module | Contents |
//! |---|---|
//! | [`doc`] | [`Target`], [`DocMeta`] — the document model |
//! | [`brand`] | [`BrandSpec`] and the auto-layout config it carries |
//! | [`digest`] | [`Digest`] — a validated lowercase-hex sha256 |
//! | [`manifest`] | [`AssetManifest`] — asset key → digest |
//! | [`wire`] | request/response bodies for the HTTP service |

pub mod brand;
pub mod digest;
pub mod doc;
pub mod manifest;
pub mod wire;

pub use brand::{AutoLayout, AutoRule, BrandSpec, LogoPosition, LogoSpec, ShapePredicate};
pub use digest::{Digest, DigestError};
pub use doc::{DocMeta, Target};
pub use manifest::AssetManifest;
pub use wire::{
    BlobCheckRequest, BlobCheckResponse, CapabilitiesResponse, ErrorCode, ErrorResponse,
    MissingBlob, MissingBlobsError, RenderMarkdownRequest, RenderOptions, RenderTemplateRequest,
    TemplateFormat,
};
