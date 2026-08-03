//! Content-addressed asset storage: the on-disk cache ([`store`]) and the
//! `AssetProvider` that projects one request's manifest onto it
//! ([`provider`]).

pub mod provider;
pub mod store;

pub use provider::BlobAssets;
pub use store::{BlobStore, WriteError};
