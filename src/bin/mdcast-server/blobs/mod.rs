//! Content-addressed asset storage: the in-memory cache ([`store`]), with a
//! lazily-populated on-disk mirror solely for hard-linking into pandoc's
//! per-render temp dirs, and the `AssetProvider` that projects one request's
//! manifest onto it ([`provider`]).

pub mod provider;
pub mod store;

pub use provider::BlobAssets;
pub use store::{BlobStore, WriteError};
