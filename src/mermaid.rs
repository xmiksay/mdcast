//! Mermaid → SVG pre-step (`mermaid` feature).
//!
//! Renders ```` ```mermaid ```` fenced code blocks to SVG via the pure-Rust
//! [`mermaid-svg`](https://crates.io/crates/mermaid-svg) crate — no Node.js,
//! no Chromium — and rewrites each fence into a standard markdown image
//! reference. Runs *before* `PageSplitter`, like a `MarkdownPreprocessor`, so
//! the transform is target-agnostic: the auto-classifier sees a real image
//! node, and both engines resolve the SVG bytes through the existing
//! `AssetProvider`/images pipeline (`images::collect_images`) exactly like
//! any other image. It is a standalone function rather than a
//! `MarkdownPreprocessor` impl because it produces bytes alongside the
//! rewritten markdown — the trait's string → string contract can't carry
//! them out.
//!
//! A diagram that fails to parse/render warns and keeps its original fence
//! (it degrades to a code block in the output) — one bad diagram never sinks
//! the render.

use bytes::Bytes;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Asset-key prefix for rendered diagrams (`mermaid/diagram-N.svg`). Keys are
/// index-based, so output is deterministic across runs.
const KEY_PREFIX: &str = "mermaid/diagram-";

/// Result of [`render_diagrams`]: the rewritten markdown plus the rendered
/// SVGs, keyed ready for the `AssetProvider` layer the caller supplies (e.g.
/// via [`crate::assets::sync_provider`] over a map, layered with
/// [`crate::LayeredAssets`]).
#[derive(Debug, Clone)]
pub struct RenderedDiagrams {
    pub markdown: String,
    /// `(asset key, SVG bytes)` per successfully rendered diagram, in
    /// document order.
    pub svgs: Vec<(String, Bytes)>,
}

/// One mermaid fence found in the source: the byte range it spans and its
/// (fence-stripped) source text as pulldown-cmark reports it.
struct Fence {
    range: std::ops::Range<usize>,
    source: String,
}

/// Find every ```` ```mermaid ```` fence, render it to SVG, and splice a
/// `![](mermaid/diagram-N.svg)` image reference in its place. Fences whose
/// info string doesn't start with `mermaid` (case-insensitive) are left
/// untouched, as are fences that fail to render (with a `tracing::warn!`).
pub fn render_diagrams(markdown: &str) -> RenderedDiagrams {
    let mut fences: Vec<Fence> = Vec::new();
    let mut current: Option<Fence> = None;
    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if is_mermaid(&info) => {
                current = Some(Fence {
                    range,
                    source: String::new(),
                });
            }
            Event::Text(text) => {
                if let Some(f) = current.as_mut() {
                    f.source.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(f) = current.take() {
                    fences.push(f);
                }
            }
            _ => {}
        }
    }

    let mut svgs: Vec<(String, Bytes)> = Vec::new();
    let mut out = markdown.to_string();
    // Splice back-to-front so earlier byte ranges stay valid, but key by
    // document order so `diagram-0` is the first diagram in the file.
    let total = fences.len();
    for (i, f) in fences.iter().enumerate().rev() {
        match mermaid_svg::render(&f.source) {
            Ok(svg) => {
                let key = format!("{KEY_PREFIX}{i}.svg");
                // A fenced block's range ends after its trailing newline;
                // keep that newline so following content stays block-separated.
                let trailing_nl = markdown[f.range.clone()].ends_with('\n');
                let replacement = format!("![]({key}){}", if trailing_nl { "\n" } else { "" });
                out.replace_range(f.range.clone(), &replacement);
                svgs.push((key, Bytes::from(svg)));
            }
            Err(error) => {
                tracing::warn!(
                    diagram = i,
                    total,
                    %error,
                    "mermaid diagram failed to render; leaving the fence as a code block"
                );
            }
        }
    }
    svgs.reverse();
    RenderedDiagrams {
        markdown: out,
        svgs,
    }
}

/// A fence is a mermaid fence when the first whitespace-separated token of
/// its info string is `mermaid` (case-insensitive) — tolerating trailing
/// attributes like ```` ```mermaid theme=dark ````.
fn is_mermaid(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|t| t.eq_ignore_ascii_case("mermaid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIE: &str = "pie\n\"A\" : 1\n\"B\" : 2\n";

    #[test]
    fn renders_fence_to_image_ref_and_svg_bytes() {
        let md = format!("# Title\n\n```mermaid\n{PIE}```\n\nAfter.\n");
        let r = render_diagrams(&md);
        assert_eq!(
            r.markdown,
            "# Title\n\n![](mermaid/diagram-0.svg)\n\nAfter.\n"
        );
        assert_eq!(r.svgs.len(), 1);
        assert_eq!(r.svgs[0].0, "mermaid/diagram-0.svg");
        assert!(r.svgs[0].1.starts_with(b"<svg"));
    }

    #[test]
    fn non_mermaid_fences_are_untouched() {
        let md = "```rust\nfn main() {}\n```\n";
        let r = render_diagrams(md);
        assert_eq!(r.markdown, md);
        assert!(r.svgs.is_empty());
    }

    #[test]
    fn no_fences_returns_input_unchanged() {
        let md = "# Just text\n\nparagraph\n";
        let r = render_diagrams(md);
        assert_eq!(r.markdown, md);
        assert!(r.svgs.is_empty());
    }

    #[test]
    fn multiple_fences_keyed_in_document_order() {
        let md = format!("```mermaid\n{PIE}```\n\nmiddle\n\n```mermaid\n{PIE}```\n");
        let r = render_diagrams(&md);
        assert_eq!(
            r.markdown,
            "![](mermaid/diagram-0.svg)\n\nmiddle\n\n![](mermaid/diagram-1.svg)\n"
        );
        let keys: Vec<&str> = r.svgs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["mermaid/diagram-0.svg", "mermaid/diagram-1.svg"]);
    }

    #[test]
    fn invalid_diagram_keeps_its_fence() {
        let md = "```mermaid\nnot a diagram type at all\n```\n";
        let r = render_diagrams(md);
        assert_eq!(r.markdown, md);
        assert!(r.svgs.is_empty());
    }

    #[test]
    fn invalid_diagram_does_not_shift_keys_of_later_valid_ones() {
        let md = format!("```mermaid\nnope nope\n```\n\n```mermaid\n{PIE}```\n");
        let r = render_diagrams(&md);
        // First fence stays; second still gets its document-order index.
        assert!(r.markdown.starts_with("```mermaid\nnope nope\n```\n"));
        assert!(r.markdown.contains("![](mermaid/diagram-1.svg)"));
        assert_eq!(r.svgs.len(), 1);
        assert_eq!(r.svgs[0].0, "mermaid/diagram-1.svg");
    }

    #[test]
    fn info_string_attributes_and_case_are_tolerated() {
        let md = format!("```Mermaid theme=dark\n{PIE}```\n");
        let r = render_diagrams(&md);
        assert_eq!(r.markdown, "![](mermaid/diagram-0.svg)\n");
        assert_eq!(r.svgs.len(), 1);
    }

    #[test]
    fn tilde_fences_work_too() {
        let md = format!("~~~mermaid\n{PIE}~~~\n");
        let r = render_diagrams(&md);
        assert_eq!(r.markdown, "![](mermaid/diagram-0.svg)\n");
        assert_eq!(r.svgs.len(), 1);
    }
}
