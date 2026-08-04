//! Asset resolution. Backends fetch templates, reference docs, images, etc.
//! through this trait — never `std::fs`. The default impl bakes the built-in
//! catalog in via `rust-embed`; consumers wrap or replace it.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use bytes::Bytes;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Single boundary trait. Boxed futures so it is dyn-safe; the overhead is
/// irrelevant compared to the cost of resolving an asset (often a network or
/// disk hit).
pub trait AssetProvider: Send + Sync {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>>;
    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>>;

    /// A real filesystem path this key already lives at, if the provider can
    /// offer one directly instead of handing back bytes for the caller to
    /// write itself (e.g. a subprocess backend that would otherwise copy
    /// `get()`'s bytes into its own temp file can hard-link this path
    /// instead). Default: no path available — callers fall back to `get()`
    /// plus their own write.
    fn path<'a>(&'a self, _key: &'a str) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(async move { Ok(None) })
    }
}

#[derive(rust_embed::RustEmbed)]
#[folder = "embedded/"]
struct Embedded;

/// Default provider: the catalog baked into the binary at build time.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddedAssets;

impl AssetProvider for EmbeddedAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        let result = Embedded::get(key).map(|f| Bytes::copy_from_slice(&f.data));
        Box::pin(async move { Ok(result) })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        let result: Vec<String> = Embedded::iter()
            .filter(|p| p.starts_with(prefix))
            .map(|p| p.into_owned())
            .collect();
        Box::pin(async move { Ok(result) })
    }
}

/// Delegate through a `Box` so provider stacks can be built up dynamically
/// (e.g. the CLI layering mermaid SVGs and `--assets` over `EmbeddedAssets`).
impl<T: AssetProvider + ?Sized> AssetProvider for Box<T> {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        (**self).get(key)
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        (**self).list(prefix)
    }

    fn path<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        (**self).path(key)
    }
}

/// Try `over` first, fall back to `base`. Lets a consumer ship a small override
/// set on top of `EmbeddedAssets` without re-implementing the whole trait.
#[derive(Debug, Clone, Copy)]
pub struct LayeredAssets<O, B> {
    pub over: O,
    pub base: B,
}

impl<O: AssetProvider, B: AssetProvider> AssetProvider for LayeredAssets<O, B> {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        Box::pin(async move {
            match self.over.get(key).await? {
                Some(b) => Ok(Some(b)),
                None => self.base.get(key).await,
            }
        })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async move {
            let mut out = self.over.list(prefix).await?;
            for k in self.base.list(prefix).await? {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
            Ok(out)
        })
    }

    fn path<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(async move {
            match self.over.path(key).await? {
                Some(p) => Ok(Some(p)),
                None => self.base.path(key).await,
            }
        })
    }
}

/// Wrap a synchronous `get` closure as an `AssetProvider`. `list` is a no-op
/// (returns empty); impl the trait directly if you need real `list` support.
pub fn sync_provider<F>(get: F) -> SyncProvider<F>
where
    F: Fn(&str) -> Result<Option<Bytes>> + Send + Sync + 'static,
{
    SyncProvider { get }
}

pub struct SyncProvider<F> {
    get: F,
}

impl<F> AssetProvider for SyncProvider<F>
where
    F: Fn(&str) -> Result<Option<Bytes>> + Send + Sync + 'static,
{
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        let r = (self.get)(key);
        Box::pin(async move { r })
    }

    fn list<'a>(&'a self, _prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

/// Wrap an async `get` closure as an `AssetProvider`. As above for `list`.
pub fn async_provider<F, Fut>(get: F) -> AsyncProvider<F>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Bytes>>> + Send + 'static,
{
    AsyncProvider { get }
}

pub struct AsyncProvider<F> {
    get: F,
}

impl<F, Fut> AssetProvider for AsyncProvider<F>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Bytes>>> + Send + 'static,
{
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        let fut = (self.get)(key.to_owned());
        Box::pin(fut)
    }

    fn list<'a>(&'a self, _prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sync_provider_returns_value() {
        let p = sync_provider(|key| {
            if key == "hello" {
                Ok(Some(Bytes::from_static(b"world")))
            } else {
                Ok(None)
            }
        });
        assert_eq!(
            p.get("hello").await.unwrap(),
            Some(Bytes::from_static(b"world"))
        );
        assert!(p.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn async_provider_can_defer() {
        let p = async_provider(|key: String| async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            if key == "k" {
                Ok(Some(Bytes::from_static(b"v")))
            } else {
                Ok(None)
            }
        });
        assert_eq!(p.get("k").await.unwrap(), Some(Bytes::from_static(b"v")));
        assert!(p.list("").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn layered_prefers_over() {
        let over = sync_provider(|k| match k {
            "a" => Ok(Some(Bytes::from_static(b"OVER"))),
            _ => Ok(None),
        });
        let base = sync_provider(|k| match k {
            "a" => Ok(Some(Bytes::from_static(b"BASE"))),
            "b" => Ok(Some(Bytes::from_static(b"BASE-B"))),
            _ => Ok(None),
        });
        let layered = LayeredAssets { over, base };
        assert_eq!(
            layered.get("a").await.unwrap(),
            Some(Bytes::from_static(b"OVER"))
        );
        assert_eq!(
            layered.get("b").await.unwrap(),
            Some(Bytes::from_static(b"BASE-B"))
        );
        assert_eq!(layered.get("c").await.unwrap(), None);
    }

    /// Minimal `AssetProvider` that can offer a real path for one key —
    /// `sync_provider`/`async_provider` don't override `path()`, so
    /// exercising it needs a real impl.
    struct PathProvider(&'static str, &'static str);

    impl AssetProvider for PathProvider {
        fn get<'a>(&'a self, _key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
            Box::pin(async move { Ok(None) })
        }

        fn list<'a>(&'a self, _prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
            Box::pin(async move { Ok(Vec::new()) })
        }

        fn path<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<PathBuf>>> {
            let result = (key == self.0).then(|| PathBuf::from(self.1));
            Box::pin(async move { Ok(result) })
        }
    }

    #[tokio::test]
    async fn layered_path_prefers_over_then_falls_back_to_base() {
        let over = PathProvider("a", "/over/a");
        let base = PathProvider("b", "/base/b");
        let layered = LayeredAssets { over, base };

        assert_eq!(
            layered.path("a").await.unwrap(),
            Some(PathBuf::from("/over/a"))
        );
        assert_eq!(
            layered.path("b").await.unwrap(),
            Some(PathBuf::from("/base/b"))
        );
        assert_eq!(layered.path("c").await.unwrap(), None);
    }
}
