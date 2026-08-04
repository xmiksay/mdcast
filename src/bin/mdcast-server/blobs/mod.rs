//! Content-addressed asset storage: the in-memory cache ([`store`]), with a
//! lazily-populated on-disk mirror solely for hard-linking into pandoc's
//! per-render temp dirs, the `AssetProvider` that projects one request's
//! manifest onto it ([`provider`]), and the startup warmup routine that pins
//! a predefined asset set into it ([`prefill`]).

pub mod prefill;
pub mod provider;
pub mod store;

pub use provider::BlobAssets;
pub use store::{BlobStore, WriteError};
