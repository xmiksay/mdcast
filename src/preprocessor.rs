//! User-pluggable markdown preprocessor.
//!
//! Runs *before* `PageSplitter` so the transform is target-agnostic — a
//! preprocessor that rewrites `<img path="X">` into `![](X)` benefits PDF
//! (typst), DOCX/PPTX/HTML-reveal (pandoc), and the auto-classifier (it
//! sees real image nodes again).
//!
//! Sync by design. If async resolution is ever needed, add a sibling
//! `AsyncMarkdownPreprocessor` trait — making the existing one async would
//! cascade an await through the whole pipeline for zero benefit in the
//! common (pure-string-rewrite) case.

use std::sync::LazyLock;

use regex::Regex;

pub trait MarkdownPreprocessor: Send + Sync {
    fn preprocess(&self, markdown: &str) -> String;
}

/// No-op preprocessor. Used as the default when the library caller doesn't
/// supply one.
#[derive(Debug, Default, Clone, Copy)]
pub struct Identity;

impl MarkdownPreprocessor for Identity {
    fn preprocess(&self, markdown: &str) -> String {
        markdown.to_string()
    }
}

/// Run `first`, then `second`. Compose with `MarkdownPreprocessor::then(...)`
/// or by constructing directly.
#[derive(Debug, Clone, Copy)]
pub struct Chain<A, B> {
    pub first: A,
    pub second: B,
}

impl<A: MarkdownPreprocessor, B: MarkdownPreprocessor> MarkdownPreprocessor for Chain<A, B> {
    fn preprocess(&self, markdown: &str) -> String {
        self.second.preprocess(&self.first.preprocess(markdown))
    }
}

/// Built-in preprocessor: rewrite `<img>` / `<image>` HTML tags into standard
/// markdown image syntax `![alt](src)`. Recognises a small set of attributes
/// (`src`, `path`, `alt`); ignores anything else.
///
/// Provided because the F13 markdown content uses HTML-style image tags as a
/// platform convention. Other consumers can disable by not adding it to the
/// chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct HtmlImageTags;

// <img …/> or <img …> (no closing — self-closing in practice).
// Captured attrs: src or path → url, alt → alt text.
static IMG_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<\s*(?:img|image)(?P<attrs>(?:\s+[a-zA-Z_:-]+\s*=\s*"[^"]*")*)\s*/?\s*>"#)
        .unwrap()
});
static ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)([a-zA-Z_:-]+)\s*=\s*"([^"]*)""#).unwrap());

impl MarkdownPreprocessor for HtmlImageTags {
    fn preprocess(&self, markdown: &str) -> String {
        IMG_TAG_RE
            .replace_all(markdown, |caps: &regex::Captures<'_>| {
                let attrs = caps.name("attrs").map(|m| m.as_str()).unwrap_or("");
                let mut src: Option<String> = None;
                let mut alt: Option<String> = None;
                for ac in ATTR_RE.captures_iter(attrs) {
                    let name = ac.get(1).unwrap().as_str().to_ascii_lowercase();
                    let value = ac.get(2).unwrap().as_str();
                    match name.as_str() {
                        "src" | "path" => src = Some(value.to_string()),
                        "alt" => alt = Some(value.to_string()),
                        _ => {}
                    }
                }
                match src {
                    Some(s) => format!("![{}]({})", alt.unwrap_or_default(), s),
                    None => caps[0].to_string(), // no src/path — leave unchanged
                }
            })
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_returns_input_unchanged() {
        let md = "# Heading\n\nparagraph";
        assert_eq!(Identity.preprocess(md), md);
    }

    #[test]
    fn html_image_tag_with_src() {
        let out = HtmlImageTags.preprocess(r#"before <img src="logo.png" alt="Logo"/> after"#);
        assert_eq!(out, "before ![Logo](logo.png) after");
    }

    #[test]
    fn html_image_tag_with_path() {
        let out = HtmlImageTags.preprocess(r#"<image path="diagram.svg" />"#);
        assert_eq!(out, "![](diagram.svg)");
    }

    #[test]
    fn html_image_tag_without_src_is_left_alone() {
        let input = r#"<img alt="nothing"/>"#;
        assert_eq!(HtmlImageTags.preprocess(input), input);
    }

    #[test]
    fn chain_composes_in_order() {
        struct Upper;
        impl MarkdownPreprocessor for Upper {
            fn preprocess(&self, md: &str) -> String {
                md.to_uppercase()
            }
        }
        let chain = Chain {
            first: HtmlImageTags,
            second: Upper,
        };
        let out = chain.preprocess(r#"<img src="a.png"/>"#);
        assert_eq!(out, "![](A.PNG)");
    }
}
