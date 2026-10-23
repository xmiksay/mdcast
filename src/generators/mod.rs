//! Generated images: markdown blocks mdcast draws itself (a QR code today),
//! plus the same renderers for `RenderTemplateRequest.generated` entries.
//!
//! A generator answers to one name, written either as a fence or as a tag —
//! the tag form being the convention the F13 sites already use:
//!
//! ````text
//! ```qr ec=H size=large
//! SPD*1.0*ACC:CZ6508000000192000145399
//! ```
//!
//! <qr ec="H">SPD*1.0*…</qr>
//! <qr path="payments/invoice-42.txt" size="small">
//! ````
//!
//! A fence's body is its content minus the closing newline; a tag's body is
//! trimmed. `path="KEY"` reads the content through the [`AssetProvider`]
//! instead (trailing whitespace trimmed), which is why this pre-step is async
//! where `mermaid::render_diagrams` is not. Tags inside code are left alone.
//!
//! Each block becomes `![](<prefix>-N.svg)` with the SVG returned alongside,
//! for the caller to layer over its provider exactly like mermaid's output. A
//! block that fails — bad attribute, unreadable `path`, unencodable data —
//! warns and stays in the document as written: one bad block never sinks the
//! render. The template path ([`render_spec`]) fails hard instead, since the
//! caller asked for that exact file.

#[cfg(test)]
mod tests;

pub mod qr;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use bytes::Bytes;
use futures::future::join_all;
use mdcast_api::wire::{GeneratedSpec, Generator};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use regex::Regex;

use crate::AssetProvider;

/// The generators this build can render, for `GET /v1/capabilities`.
pub fn available() -> Vec<Generator> {
    vec![Generator::Qr]
}

/// Render one `RenderTemplateRequest.generated` entry. Errors name nothing
/// but the reason — the caller adds which entry it was.
pub fn render_spec(spec: &GeneratedSpec) -> Result<Bytes> {
    match spec {
        GeneratedSpec::Qr { data, ec } => Ok(Bytes::from(qr::svg(data, *ec, Size::Normal)?)),
        other => bail!("generator {other:?} is not available in this build"),
    }
}

/// Result of a markdown pre-step: the rewritten markdown plus the rendered
/// SVGs, keyed for the provider layer the caller adds.
#[derive(Debug, Clone, Default)]
pub struct Generated {
    pub markdown: String,
    /// `(asset key, SVG bytes)` per rendered block, in document order.
    pub svgs: Vec<(String, Bytes)>,
}

/// Lower-cased attribute name → value, from a fence info string or a tag.
pub(crate) type Attrs = HashMap<String, String>;

/// `size="small|large"` — the F13 sites' size attribute, scaling a
/// generator's intrinsic size by ½ / 1½.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Size {
    Small,
    Normal,
    Large,
}

impl Size {
    pub(crate) fn from_attrs(attrs: &Attrs) -> Result<Self> {
        match attrs.get("size").map(|s| s.to_ascii_lowercase()).as_deref() {
            None => Ok(Size::Normal),
            Some("small" | "sm") => Ok(Size::Small),
            Some("large" | "lg") => Ok(Size::Large),
            Some(other) => bail!("unknown size {other:?} (expected small or large)"),
        }
    }

    pub(crate) fn scale(self, base: f32) -> f32 {
        match self {
            Size::Small => base * 0.5,
            Size::Normal => base,
            Size::Large => base * 1.5,
        }
    }
}

/// One fence or tag found in the source.
#[derive(Debug, PartialEq)]
struct Block {
    range: Range<usize>,
    attrs: Attrs,
    source: Source,
}

#[derive(Debug, PartialEq)]
enum Source {
    Inline(String),
    Path(String),
}

/// Find, resolve and render every `name` block, splicing an image reference
/// over each one that renders. `render` gets the block's content and
/// attributes and returns SVG text.
pub(crate) async fn expand(
    markdown: &str,
    name: &str,
    key_prefix: &str,
    assets: &dyn AssetProvider,
    render: impl Fn(&str, &Attrs) -> Result<String>,
) -> Generated {
    let blocks = scan(markdown, name);
    let contents = join_all(blocks.iter().map(|b| content(&b.source, assets))).await;

    let mut out = markdown.to_string();
    let mut svgs = Vec::new();
    // Splice back-to-front so earlier ranges stay valid; keys follow
    // document order, and a failed block still consumes its index so later
    // keys don't shift.
    for (i, (block, content)) in blocks.iter().zip(contents).enumerate().rev() {
        match content.and_then(|c| render(&c, &block.attrs)) {
            Ok(svg) => {
                let key = format!("{key_prefix}-{i}.svg");
                // A fence's range ends after its trailing newline; keep it so
                // following content stays block-separated.
                let nl = if markdown[block.range.clone()].ends_with('\n') {
                    "\n"
                } else {
                    ""
                };
                out.replace_range(block.range.clone(), &format!("![]({key}){nl}"));
                svgs.push((key, Bytes::from(svg)));
            }
            Err(error) => tracing::warn!(
                generator = name,
                block = i,
                error = format!("{error:#}"),
                "generated image failed to render; leaving the block as written"
            ),
        }
    }
    svgs.reverse();
    Generated {
        markdown: out,
        svgs,
    }
}

async fn content(source: &Source, assets: &dyn AssetProvider) -> Result<String> {
    match source {
        Source::Inline(s) => Ok(s.clone()),
        Source::Path(key) => {
            let Some(bytes) = assets.get(key).await? else {
                bail!("path {key:?} not found in asset provider");
            };
            let text = String::from_utf8(bytes.to_vec())
                .map_err(|_| anyhow::anyhow!("path {key:?} is not valid UTF-8"))?;
            Ok(text.trim_end().to_string())
        }
    }
}

static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([a-zA-Z_:-]+)\s*=\s*(?:"([^"]*)"|([^\s"]+))"#).expect("static regex")
});

fn parse_attrs(s: &str) -> Attrs {
    ATTR_RE
        .captures_iter(s)
        .map(|c| {
            let value = c.get(2).or_else(|| c.get(3)).map_or("", |m| m.as_str());
            (c[1].to_ascii_lowercase(), value.to_string())
        })
        .collect()
}

/// Every `name` fence and tag in `markdown`, in document order.
fn scan(markdown: &str, name: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    // Byte ranges of all code (blocks and spans): tags inside them are text.
    let mut code: Vec<Range<usize>> = Vec::new();
    let mut fence: Option<(Range<usize>, Attrs, String)> = None;

    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                code.push(range.clone());
                if let CodeBlockKind::Fenced(info) = kind {
                    let mut parts = info.splitn(2, char::is_whitespace);
                    if parts.next().is_some_and(|t| t.eq_ignore_ascii_case(name)) {
                        fence = Some((
                            range,
                            parse_attrs(parts.next().unwrap_or("")),
                            String::new(),
                        ));
                    }
                }
            }
            Event::Text(text) => {
                if let Some((_, _, body)) = fence.as_mut() {
                    body.push_str(&text);
                }
            }
            Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                if let Some((range, attrs, body)) = fence.take() {
                    let body = body.strip_suffix('\n').unwrap_or(&body).to_string();
                    blocks.push(Block {
                        range,
                        attrs,
                        source: Source::Inline(body),
                    });
                }
            }
            Event::Code(_) => code.push(range),
            _ => {}
        }
    }

    let open = Regex::new(&format!(
        r#"(?i)<{name}(?P<attrs>(?:\s+[a-zA-Z_:-]+\s*=\s*"[^"]*")*)\s*(?P<selfclose>/)?>"#
    ))
    .expect("generator names are plain identifiers");
    let close =
        Regex::new(&format!(r"(?i)</{name}\s*>")).expect("generator names are plain identifiers");

    let mut from = 0;
    while let Some(m) = open.captures_at(markdown, from) {
        let tag = m.get(0).expect("group 0 always matches");
        from = tag.end();
        if code.iter().any(|r| r.contains(&tag.start())) {
            continue;
        }
        let mut attrs = parse_attrs(&m["attrs"]);
        let block = if let Some(key) = attrs.remove("path") {
            // `<qr path="…">` has no body; swallow a directly following close
            // tag so `<qr path="…"></qr>` works too.
            let end = close
                .find_at(markdown, tag.end())
                .filter(|c| markdown[tag.end()..c.start()].trim().is_empty())
                .map_or(tag.end(), |c| c.end());
            Block {
                range: tag.start()..end,
                attrs,
                source: Source::Path(key),
            }
        } else if m.name("selfclose").is_some() {
            tracing::warn!(
                generator = name,
                "self-closing tag without path=; ignoring it"
            );
            continue;
        } else {
            let Some(c) = close.find_at(markdown, tag.end()) else {
                tracing::warn!(generator = name, "tag without a closing tag; ignoring it");
                continue;
            };
            Block {
                range: tag.start()..c.end(),
                attrs,
                source: Source::Inline(markdown[tag.end()..c.start()].trim().to_string()),
            }
        };
        from = block.range.end;
        blocks.push(block);
    }

    blocks.sort_by_key(|b| b.range.start);
    blocks
}
