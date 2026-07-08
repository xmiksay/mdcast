//! Resolve markdown image references through the `AssetProvider`.
//!
//! Walks every page body for image references, asks the provider for the
//! bytes, writes them into a per-render temp directory, and rewrites the
//! markdown so the path points at the materialised file. Anything the provider
//! returns `None` for is left untouched — backends will then fall back to
//! whatever their engine does (pandoc's `--resource-path`, missing-image
//! warning, etc.).

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures::future::try_join_all;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::AssetProvider;
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
/// strips angle brackets/titles into `dest_url`/`title` for us. Shared by the
/// pandoc rewrite path (`resolve_images`) and the typst image collector
/// (`collect_images_for_typst`) so both engines agree on what an image
/// reference is.
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

/// Resolve every image reference in `pages` through `provider`. Mutates the
/// page bodies in place; materialises bytes into `dest`. Returns the set of
/// keys actually fetched (handy for tests and `--explain`).
pub async fn resolve_images(
    pages: &mut [Page],
    provider: &dyn AssetProvider,
    dest: &Path,
) -> Result<Vec<String>> {
    // Pass 1: collect unique keys referenced, plus each page's parsed refs
    // (reused in pass 3 so the body isn't re-parsed after being rewritten).
    let mut keys: BTreeMap<String, ()> = BTreeMap::new();
    let page_refs: Vec<Vec<ImageRef>> = pages
        .iter()
        .map(|page| {
            let refs = image_refs(&page.body);
            for r in &refs {
                if !looks_remote(&r.dest_url) {
                    keys.insert(r.dest_url.clone(), ());
                }
            }
            refs
        })
        .collect();

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

    // Pass 3: rewrite page bodies. Each resolved reference — whatever its
    // original form (titled, angle-bracket, reference-style) — is spliced out
    // and replaced with a plain `![alt](local-path)`, applied back-to-front so
    // earlier byte ranges stay valid as later ones are replaced.
    for (page, refs) in pages.iter_mut().zip(page_refs) {
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

    Ok(resolved)
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
    async fn resolves_titled_image() {
        let provider = sync_provider(|key| match key {
            "diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
            _ => Ok(None),
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut pages = vec![page(r#"![d](diagram.svg "Fig 1")"#)];
        let resolved = resolve_images(&mut pages, &provider, tmp.path())
            .await
            .unwrap();

        assert_eq!(resolved, vec!["diagram.svg".to_string()]);
        assert!(pages[0].body.contains("diagram.svg"), "{}", pages[0].body);
    }

    #[tokio::test]
    async fn resolves_angle_bracket_url() {
        let provider = sync_provider(|key| match key {
            "diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
            _ => Ok(None),
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut pages = vec![page("![d](<diagram.svg>)")];
        let resolved = resolve_images(&mut pages, &provider, tmp.path())
            .await
            .unwrap();

        assert_eq!(resolved, vec!["diagram.svg".to_string()]);
        assert!(pages[0].body.contains("diagram.svg"), "{}", pages[0].body);
    }

    #[tokio::test]
    async fn resolves_reference_style_image() {
        let provider = sync_provider(|key| match key {
            "diagram.svg" => Ok(Some(Bytes::from_static(b"<svg/>"))),
            _ => Ok(None),
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut pages = vec![page("![d][ref]\n\n[ref]: diagram.svg \"Fig 1\"\n")];
        let resolved = resolve_images(&mut pages, &provider, tmp.path())
            .await
            .unwrap();

        assert_eq!(resolved, vec!["diagram.svg".to_string()]);
        assert!(pages[0].body.contains("diagram.svg"), "{}", pages[0].body);
        assert!(
            !pages[0].body.contains("![d][ref]"),
            "reference-style form should be normalised to inline: {}",
            pages[0].body
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
