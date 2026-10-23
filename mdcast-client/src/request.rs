//! Constructors for the two wire request bodies.
//!
//! [`RenderMarkdownRequest`] and [`RenderTemplateRequest`] are plain serde
//! structs in `mdcast-api` with no `Default` (markdown and target are not
//! optional), so building one by hand means naming every field. These give the
//! common case one expression; everything else is a struct-update:
//!
//! ```
//! use mdcast_client::request;
//! use mdcast_api::{Target, wire::RenderOptions};
//!
//! let mut req = request::markdown("# Q3", Target::Pdf);
//! req.options = RenderOptions {
//!     mermaid: true,
//!     toc_depth: Some(2),
//!     ..RenderOptions::default()
//! };
//! ```
//!
//! The `assets` manifest is deliberately left empty: the client fills it from
//! the [`crate::AssetBundle`] handed to the render call.

use mdcast_api::wire::{RenderMarkdownRequest, RenderTemplateRequest};
use mdcast_api::{AssetManifest, DocMeta, Target};

/// Render `markdown` to `target`, with no brand and no options.
pub fn markdown(markdown: impl Into<String>, target: Target) -> RenderMarkdownRequest {
    RenderMarkdownRequest {
        markdown: markdown.into(),
        meta: DocMeta::default(),
        brand: None,
        target,
        assets: AssetManifest::new(),
        options: Default::default(),
    }
}

/// Render a typst template over `data` (raw JSON text, the bytes that become
/// the virtual `/data.json`) to PDF.
pub fn template(
    template_source: impl Into<String>,
    data: impl Into<String>,
) -> RenderTemplateRequest {
    RenderTemplateRequest {
        template_source: template_source.into(),
        data: data.into(),
        meta: DocMeta::default(),
        brand: None,
        format: Default::default(),
        assets: AssetManifest::new(),
        fonts: Vec::new(),
        generated: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdcast_api::wire::{RenderOptions, TemplateFormat};

    #[test]
    fn markdown_defaults_everything_but_the_body_and_target() {
        let req = markdown("# Hi", Target::HtmlReveal);

        assert_eq!(req.markdown, "# Hi");
        assert_eq!(req.target, Target::HtmlReveal);
        assert_eq!(req.meta, DocMeta::default());
        assert_eq!(req.brand, None);
        assert_eq!(req.options, RenderOptions::default());
        assert!(
            req.assets.is_empty(),
            "the bundle fills the manifest, not the caller"
        );
    }

    #[test]
    fn template_defaults_to_pdf() {
        let req = template("= Invoice", r#"{"number":"INV-042"}"#);

        assert_eq!(req.format, TemplateFormat::Pdf);
        assert_eq!(req.data, r#"{"number":"INV-042"}"#);
        assert!(req.assets.is_empty());
        assert!(req.fonts.is_empty());
        assert!(req.generated.is_empty());
    }
}
