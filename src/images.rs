//! Resolve markdown image references through the `AssetProvider`.
//!
//! Walks every page body for `![alt](key)` references, asks the provider for
//! the bytes, writes them into a per-render temp directory, and rewrites the
//! markdown so the path points at the materialised file. Anything the provider
//! returns `None` for is left untouched — backends will then fall back to
//! whatever their engine does (pandoc's `--resource-path`, missing-image
//! warning, etc.).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures::future::try_join_all;
use regex::Regex;

use crate::AssetProvider;
use crate::pages::Page;

/// Resolve every image reference in `pages` through `provider`. Mutates the
/// page bodies in place; materialises bytes into `dest`. Returns the set of
/// keys actually fetched (handy for tests and `--explain`).
pub async fn resolve_images(
    pages: &mut [Page],
    provider: &dyn AssetProvider,
    dest: &Path,
) -> Result<Vec<String>> {
    let re = image_regex();

    // Pass 1: collect unique keys referenced.
    let mut keys: BTreeMap<String, ()> = BTreeMap::new();
    for page in pages.iter() {
        for cap in re.captures_iter(&page.body) {
            let url = cap.name("url").unwrap().as_str();
            if looks_remote(url) {
                continue;
            }
            keys.insert(url.to_string(), ());
        }
    }

    // Pass 2: fetch all in parallel; build url → local path map.
    let key_vec: Vec<String> = keys.into_keys().collect();
    let fetches = try_join_all(key_vec.iter().map(|k| async move {
        let res: Result<(String, Option<PathBuf>)> = async {
            let Some(bytes) = provider.get(k).await? else {
                return Ok((k.clone(), None));
            };
            let local = dest.join(sanitize_key(k));
            if let Some(parent) = local.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("create parent for {}", local.display()))?;
            }
            tokio::fs::write(&local, &bytes)
                .await
                .with_context(|| format!("write {}", local.display()))?;
            Ok((k.clone(), Some(local)))
        }
        .await;
        res
    }))
    .await?;

    let mut map: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut resolved: Vec<String> = Vec::new();
    for (k, p) in fetches {
        if let Some(p) = p {
            map.insert(k.clone(), p);
            resolved.push(k);
        }
    }

    // Pass 3: rewrite page bodies. We rewrite only the URL inside `()`, leaving
    // the alt text and surrounding markdown untouched.
    for page in pages.iter_mut() {
        let new_body = re
            .replace_all(&page.body, |caps: &regex::Captures<'_>| {
                let url = caps.name("url").unwrap().as_str();
                match map.get(url) {
                    Some(local) => {
                        format!("![{}]({})", &caps["alt"], local.display())
                    }
                    None => caps[0].to_string(),
                }
            })
            .into_owned();
        page.body = new_body;
    }

    Ok(resolved)
}

/// Matches `![alt](url)` where url has no spaces, no inner `)`, and the alt
/// text has no nested brackets. Pandoc-flavoured links with titles
/// `(url "title")` aren't supported in v1. Shared by the typst backend, which
/// only needs the `url` capture.
pub(crate) fn image_regex() -> Regex {
    Regex::new(r"!\[(?P<alt>[^\]]*)\]\((?P<url>[^)\s]+)\)").unwrap()
}

pub(crate) fn looks_remote(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with("file://")
}

fn sanitize_key(key: &str) -> String {
    // Flatten slashes; the provider's namespace is conceptual, not a real path.
    key.replace(['/', '\\'], "__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::sync_provider;
    use crate::pages::PageOrigin;
    use bytes::Bytes;

    fn page(body: &str) -> Page {
        Page {
            class: "content".into(),
            body: body.into(),
            origin: PageOrigin::Explicit,
        }
    }

    #[tokio::test]
    async fn rewrites_resolved_keys_only() {
        let provider = sync_provider(|key| match key {
            "img/diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
            _ => Ok(None),
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut pages = vec![page(
            "![d](img/diagram.svg) and ![x](img/missing.png) and ![ext](https://e.com/p.png)",
        )];
        let resolved = resolve_images(&mut pages, &provider, tmp.path())
            .await
            .unwrap();

        assert_eq!(resolved, vec!["img/diagram.svg".to_string()]);
        assert!(pages[0].body.contains("img__diagram.svg"));
        assert!(
            pages[0].body.contains("img/missing.png"),
            "missing keys preserved"
        );
        assert!(
            pages[0].body.contains("https://e.com/p.png"),
            "remote URLs preserved"
        );
    }

    #[tokio::test]
    async fn dedup_fetches_same_key_only_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = calls.clone();
        let provider = sync_provider(move |k| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            if k == "shared.png" {
                Ok(Some(Bytes::from_static(b"x")))
            } else {
                Ok(None)
            }
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut pages = vec![page("![](shared.png)"), page("![](shared.png)")];
        resolve_images(&mut pages, &provider, tmp.path())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
