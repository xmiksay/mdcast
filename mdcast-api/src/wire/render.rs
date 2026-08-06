//! Render request bodies — one per entry point mdcast exposes: the markdown
//! pipeline (`mdcast render`) and the data-driven typst template pipeline
//! (`mdcast render-template`).

use serde::{Deserialize, Serialize};

use crate::brand::BrandSpec;
use crate::doc::{DocMeta, Target};
use crate::manifest::AssetManifest;

/// Render a markdown document to `target`. Mirrors what the CLI's `render`
/// subcommand builds before handing a `ResolvedDoc` to a backend: the body
/// after frontmatter extraction, the metadata that extraction produced, the
/// brand spec, and the flags that change the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderMarkdownRequest {
    /// Markdown body. Frontmatter may still be present — the server runs the
    /// same `frontmatter::extract` the CLI does, and anything it finds wins
    /// over an empty `meta`.
    pub markdown: String,
    #[serde(default)]
    pub meta: DocMeta,
    /// `None` renders with `BrandSpec::default()` — the same as omitting
    /// `--brand`.
    #[serde(default)]
    pub brand: Option<BrandSpec>,
    pub target: Target,
    /// Assets this render may reach through the `AssetProvider`: images the
    /// markdown references, layout/font keys named in `options`, an override
    /// for a built-in template. Keys absent here resolve against the
    /// server's embedded catalog as usual.
    #[serde(default)]
    pub assets: AssetManifest,
    #[serde(default)]
    pub options: RenderOptions,
}

/// The pipeline flags `mdcast render` exposes, minus the ones that are
/// meaningless over HTTP (`--out`, `--assets DIR`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderOptions {
    /// `--html-image-tags`: rewrite `<img src="X">` / `<image path="X">` to
    /// `![](X)` before splitting.
    #[serde(default)]
    pub html_image_tags: bool,
    /// `--mermaid`: render ```` ```mermaid ```` fences to SVG before
    /// splitting.
    #[serde(default)]
    pub mermaid: bool,
    /// `--toc-depth N`: request a table of contents at heading depth 1-6.
    #[serde(default)]
    pub toc_depth: Option<u8>,
    /// `--layout-asset KEY` (repeatable): `ResolvedDoc.assets` entries a
    /// typst layout reaches via `asset-path`.
    #[serde(default)]
    pub layout_assets: Vec<String>,
    /// `--layout-font KEY` (repeatable): `ResolvedDoc.fonts` entries to
    /// register with the typst font book.
    #[serde(default)]
    pub layout_fonts: Vec<String>,
}

/// Render a user-supplied typst template over structured data — the
/// `render-template` path (`TemplateDoc`), which bypasses `ResolvedDoc`
/// entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderTemplateRequest {
    /// Typst source of the main template. Inline source rather than
    /// `TemplateDoc.template`'s provider key: the server has no view of the
    /// caller's filesystem, and the template is the one file a caller always
    /// authors itself.
    pub template_source: String,
    /// The JSON document the template reads with `json("/data.json")`,
    /// carried verbatim as text. A string, not a parsed value, so this crate
    /// needs no JSON implementation of its own — the bytes go straight into
    /// the virtual `/data.json`.
    pub data: String,
    #[serde(default)]
    pub meta: DocMeta,
    #[serde(default)]
    pub brand: Option<BrandSpec>,
    #[serde(default)]
    pub format: TemplateFormat,
    /// Sibling files the template `#import`s or `#image`s, under the provider
    /// keys it references them by.
    #[serde(default)]
    pub assets: AssetManifest,
    /// Font faces to register with the typst compile, as manifest keys into
    /// `assets`, in declaration order (typst tie-breaks by it). The template
    /// path's counterpart to [`RenderOptions::layout_fonts`]: without it a
    /// brand's non-system faces silently fall back to whatever the server
    /// host has installed.
    #[serde(default)]
    pub fonts: Vec<String>,
}

/// Output format for a template render. `Html` needs a server built with the
/// `typst-html` feature; one built without it answers
/// [`crate::ErrorCode::UnsupportedTarget`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TemplateFormat {
    #[default]
    Pdf,
    Html,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::Digest;

    fn manifest() -> AssetManifest {
        AssetManifest::from_iter([("img/logo.png".to_string(), Digest::from_sha256([7; 32]))])
    }

    #[test]
    fn markdown_request_round_trips_through_json() {
        let req = RenderMarkdownRequest {
            markdown: "# Hi".to_string(),
            meta: DocMeta {
                title: Some("Hi".to_string()),
                ..DocMeta::default()
            },
            brand: Some(BrandSpec::default()),
            target: Target::HtmlReveal,
            assets: manifest(),
            options: RenderOptions {
                html_image_tags: true,
                mermaid: true,
                toc_depth: Some(3),
                layout_assets: vec!["img/logo.png".to_string()],
                layout_fonts: vec!["fonts/Brand-Regular.ttf".to_string()],
            },
        };

        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<RenderMarkdownRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn markdown_request_needs_only_markdown_and_target() {
        let req: RenderMarkdownRequest =
            serde_json::from_str(r##"{"markdown":"# Hi","target":"pdf-presentation"}"##)
                .expect("every other field defaults");

        assert_eq!(req.target, Target::PdfPresentation);
        assert_eq!(req.meta, DocMeta::default());
        assert_eq!(req.brand, None);
        assert!(req.assets.is_empty());
        assert_eq!(req.options, RenderOptions::default());
    }

    #[test]
    fn template_request_round_trips_through_json() {
        let req = RenderTemplateRequest {
            template_source: "= Invoice #json(\"/data.json\").number".to_string(),
            data: r#"{"number":"INV-042"}"#.to_string(),
            meta: DocMeta::default(),
            brand: None,
            format: TemplateFormat::Html,
            assets: manifest(),
            fonts: vec!["fonts/Brand-Regular.ttf".to_string()],
        };

        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<RenderTemplateRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn template_format_defaults_to_pdf() {
        let req: RenderTemplateRequest =
            serde_json::from_str(r#"{"template_source":"= Hi","data":"{}"}"#)
                .expect("format is optional");

        assert_eq!(req.format, TemplateFormat::Pdf);
        assert_eq!(serde_json::to_string(&req.format).unwrap(), "\"pdf\"");
        assert!(
            req.fonts.is_empty(),
            "fonts default to none (0.4.1 wire compat)"
        );
    }
}
