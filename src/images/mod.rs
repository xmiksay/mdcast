//! Resolve markdown image references through the `AssetProvider`.
//!
//! `collect_images` is the shared walk/dedup/fetch pipeline: it finds every
//! non-remote image reference across a set of pages and fetches the bytes
//! once per unique key. The two engines differ only in what they do with the
//! result — `resolve_images` (pandoc path) writes bytes into a per-render
//! temp directory and rewrites the markdown so the path points at the
//! materialised file; the typst path (`backends/typst/mod.rs`) keeps the
//! bytes in memory and registers them as virtual files with the compiler.
//! Anything the provider returns `None` for is left untouched — backends
//! will then fall back to whatever their engine does (pandoc's
//! `--resource-path`, missing-image warning, etc.).

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::future::try_join_all;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::AssetProvider;
use crate::Target;
use crate::image_format::warn_if_unsupported;
use crate::pages::Page;

/// One `![alt](url "title")` / `![alt][ref]` image reference found in a page
/// body, together with the byte range it spans in the source so it can be
/// spliced back out.
pub(crate) struct ImageRef {
    range: Range<usize>,
    pub(crate) dest_url: String,
    title: Option<String>,
    alt: String,
}

/// Parse every image reference in `body` via pulldown-cmark's `Tag::Image`
/// rather than a hand-rolled regex, so titled images (`(url "title")`),
/// angle-bracket URLs (`(<url>)`), and reference-style images (`![alt][ref]`)
/// are all recognised — pulldown-cmark resolves reference definitions and
/// strips angle brackets/titles into `dest_url`/`title` for us. Used by
/// `collect_images`, so both engines agree on what an image reference is.
pub(crate) fn image_refs(body: &str) -> Vec<ImageRef> {
    let mut refs = Vec::new();
    let mut current: Option<(Range<usize>, String, Option<String>, String)> = None;
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                current = Some((
                    range,
                    dest_url.into_string(),
                    (!title.is_empty()).then(|| title.into_string()),
                    String::new(),
                ));
            }
            Event::Text(text) => {
                if let Some((_, _, _, alt)) = current.as_mut() {
                    alt.push_str(&text);
                }
            }
            Event::End(TagEnd::Image) => {
                if let Some((range, dest_url, title, alt)) = current.take() {
                    refs.push(ImageRef {
                        range,
                        dest_url,
                        title,
                        alt,
                    });
                }
            }
            _ => {}
        }
    }
    refs
}

/// Walk every page, collect the unique non-remote image keys referenced, and
/// fetch each through `provider` in parallel, deduped. Shared by the pandoc
/// path (`resolve_images`, which materialises the bytes to disk) and the
/// typst path (`collect_images_for_typst`, which keeps them in memory) — the
/// only difference between the two engines is what they do with the bytes
/// once fetched. Keys the provider returns `None` for are simply absent from
/// the result.
///
/// With the `remote-images` feature enabled, `http(s)://` URLs are fetched
/// too (deduped the same way, direct via `reqwest` rather than through
/// `provider` — a remote URL isn't a provider key) and folded into the same
/// result map, so a page-body `![alt](https://…)` resolves identically for
/// both engines: the typst path registers it as a virtual file same as any
/// other image, and the pandoc path (`resolve_images`) materialises it to
/// disk and rewrites the markdown to the local copy, so pandoc never touches
/// the network itself. A fetch failure (DNS, 404, timeout, …) warns and is
/// dropped rather than failing the render — one dead link shouldn't sink a
/// whole document.
pub(crate) async fn collect_images(
    pages: &[Page],
    provider: &dyn AssetProvider,
    target: Target,
) -> Result<BTreeMap<String, Bytes>> {
    let mut keys: BTreeMap<String, ()> = BTreeMap::new();
    #[cfg(feature = "remote-images")]
    let mut remote_keys: BTreeMap<String, ()> = BTreeMap::new();
    for page in pages {
        for r in image_refs(&page.body) {
            if looks_remote(&r.dest_url) {
                #[cfg(feature = "remote-images")]
                if r.dest_url.starts_with("http://") || r.dest_url.starts_with("https://") {
                    remote_keys.insert(r.dest_url, ());
                }
            } else {
                keys.insert(r.dest_url, ());
            }
        }
    }

    let key_vec: Vec<String> = keys.into_keys().collect();
    let fetched = try_join_all(key_vec.iter().map(|k| async move {
        let bytes = provider.get(k).await?;
        Ok::<(String, Option<Bytes>), anyhow::Error>((k.clone(), bytes))
    }))
    .await?;

    #[cfg_attr(not(feature = "remote-images"), allow(unused_mut))]
    let mut result: BTreeMap<String, Bytes> = fetched
        .into_iter()
        .filter_map(|(k, bytes)| bytes.map(|b| (k, b)))
        .collect();

    #[cfg(feature = "remote-images")]
    {
        let remote_fetched =
            futures::future::join_all(remote_keys.into_keys().map(|url| async move {
                let bytes = fetch_remote(&url).await;
                (url, bytes)
            }))
            .await;
        result.extend(
            remote_fetched
                .into_iter()
                .filter_map(|(k, bytes)| bytes.map(|b| (k, b))),
        );
    }

    for (key, bytes) in &result {
        warn_if_unsupported(key, bytes, target);
    }

    Ok(result)
}

/// Fetch one `http(s)` URL's bytes directly (not through the `AssetProvider`
/// — a remote URL isn't a provider key). Any failure — connection, non-2xx
/// status, body read — warns with the URL and returns `None` so the caller
/// treats it exactly like a provider miss.
#[cfg(feature = "remote-images")]
async fn fetch_remote(url: &str) -> Option<Bytes> {
    let resp = match reqwest::get(url).await {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(url, %error, "failed to fetch remote image; skipping");
            return None;
        }
    };
    let resp = match resp.error_for_status() {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(url, %error, "remote image fetch returned an error status; skipping");
            return None;
        }
    };
    match resp.bytes().await {
        Ok(b) => Some(b),
        Err(error) => {
            tracing::warn!(url, %error, "failed to read remote image body; skipping");
            None
        }
    }
}

/// Resolve every image reference in `pages` through `provider`. Mutates the
/// page bodies in place; materialises bytes into `dest`. Returns the set of
/// keys actually fetched (handy for tests and `--explain`).
pub async fn resolve_images(
    pages: &mut [Page],
    provider: &dyn AssetProvider,
    dest: &Path,
    target: Target,
) -> Result<Vec<String>> {
    let fetched = collect_images(pages, provider, target).await?;

    // Materialise each fetched key to disk; build url → local path map.
    let mut map: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (k, bytes) in &fetched {
        let local = dest.join(sanitize_key(k));
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create parent for {}", local.display()))?;
        }
        tokio::fs::write(&local, bytes)
            .await
            .with_context(|| format!("write {}", local.display()))?;
        map.insert(k.clone(), local);
    }

    // Rewrite page bodies. Each resolved reference — whatever its original
    // form (titled, angle-bracket, reference-style) — is spliced out and
    // replaced with a plain `![alt](local-path)`, applied back-to-front so
    // earlier byte ranges stay valid as later ones are replaced.
    for page in pages.iter_mut() {
        let refs = image_refs(&page.body);
        let mut body = page.body.clone();
        for r in refs.iter().rev() {
            if let Some(local) = map.get(&r.dest_url) {
                let replacement =
                    render_image_ref(&r.alt, &local.display().to_string(), r.title.as_deref());
                body.replace_range(r.range.clone(), &replacement);
            }
        }
        page.body = body;
    }

    Ok(map.into_keys().collect())
}

/// Rebuild `![alt](url "title")` markdown from resolved parts, escaping
/// characters that would otherwise break out of the alt/title/url syntax.
fn render_image_ref(alt: &str, url: &str, title: Option<&str>) -> String {
    let alt = escape(alt, &['\\', '[', ']']);
    let url = if url
        .chars()
        .any(|c| c.is_whitespace() || c == '(' || c == ')')
    {
        format!("<{url}>")
    } else {
        url.to_string()
    };
    match title {
        Some(t) => format!("![{alt}]({url} \"{}\")", escape(t, &['\\', '"'])),
        None => format!("![{alt}]({url})"),
    }
}

fn escape(s: &str, chars: &[char]) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if chars.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub(crate) fn looks_remote(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with("file://")
}

/// Flatten a provider key into a safe path segment — the provider's
/// namespace is conceptual, not a real filesystem/virtual-fs path. Shared by
/// the pandoc path (materialised files under a temp dir) and the typst path
/// (virtual paths registered with the in-process compiler).
pub(crate) fn sanitize_key(key: &str) -> String {
    key.replace(['/', '\\'], "__")
}

#[cfg(test)]
mod tests;
