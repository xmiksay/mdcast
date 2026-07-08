//! mdcast — Markdown → DOCX/PDF/PPTX/HTML with per-page layout templates.

pub mod assets;
pub mod backends;
pub mod brand;
pub mod frontmatter;
pub mod images;
pub mod pages;
pub mod preprocessor;

use std::path::{Path, PathBuf};

use anyhow::Result;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

pub use assets::{
    AssetProvider, BoxFuture, EmbeddedAssets, LayeredAssets, async_provider, sync_provider,
};
pub use brand::{AutoLayout, BrandSpec};
pub use pages::splitter::{DefaultSplitter, PageSplitter};
pub use pages::{Page, PageOrigin, RawPage};
pub use preprocessor::{Chain, HtmlImageTags, Identity, MarkdownPreprocessor};

/// Output target. One backend per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    Docx,
    Odt,
    Pdf,
    PdfPresentation,
    Pptx,
    HtmlReveal,
}

impl Target {
    pub fn as_str(&self) -> &'static str {
        match self {
            Target::Docx => "docx",
            Target::Odt => "odt",
            Target::Pdf => "pdf",
            Target::PdfPresentation => "pdf-presentation",
            Target::Pptx => "pptx",
            Target::HtmlReveal => "html-reveal",
        }
    }

    /// File extension (without the leading dot) for a suggested filename.
    pub fn extension(&self) -> &'static str {
        match self {
            Target::Docx => "docx",
            Target::Odt => "odt",
            Target::Pdf | Target::PdfPresentation => "pdf",
            Target::Pptx => "pptx",
            Target::HtmlReveal => "html",
        }
    }
}

/// Document-level metadata extracted from frontmatter / config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    #[serde(default)]
    pub extra: std::collections::BTreeMap<String, String>,
}

/// Opaque handle into a `BrandSpec` so backends don't carry the whole spec around.
#[derive(Debug, Clone)]
pub struct BrandHandle(pub std::sync::Arc<BrandSpec>);

/// Reference to an external asset that the renderer should resolve via the
/// `AssetProvider` (e.g. an SVG rendered from a Mermaid pre-step, or a brand
/// logo a typst layout wants to place outside the page body). Consumed today
/// by the typst backend only: each entry is fetched through the provider and
/// registered as a virtual file layouts can reach via `#import
/// "/context.typ": asset-path` (see README's "Typst layout context"). Pandoc
/// backends ignore this field — they already resolve images referenced from
/// markdown bodies via `images::resolve_images`.
#[derive(Debug, Clone)]
pub struct AssetRef {
    pub key: String,
}

/// The pipeline currency. OUR type — never pandoc's AST.
#[derive(Debug, Clone)]
pub struct ResolvedDoc {
    pub pages: Vec<Page>,
    pub meta: DocMeta,
    pub brand: BrandHandle,
    pub assets: Vec<AssetRef>,
    /// Font faces (`.ttf`/`.otf`, keyed by `AssetRef`) to register with the
    /// typst backend's font book before compiling, so a brand font resolves
    /// via `#set text(font: "<family>")` with no host install. Registered
    /// fonts take precedence over host-discovered/embedded fonts for an
    /// exact family match (see README's "Brand fonts" section). `Vec::new()`
    /// (the default) is a no-op — typst falls back to host + embedded font
    /// search exactly as before this field existed. Typst-only: pandoc
    /// backends render text with whatever font the target document format
    /// resolves and ignore this field.
    pub fonts: Vec<AssetRef>,
    /// Optional table-of-contents request (heading depth, 1-6). `None` (the
    /// default) means no TOC — output is byte-identical to before this field
    /// existed. Honoured by pandoc (docx/odt: `--toc --toc-depth=<n>`) and
    /// typst (`pdf` only: a leading `#outline(depth: <n>)` page).
    /// `pdf-presentation`/`pptx`/`html-reveal` ignore it — slide decks don't
    /// get a TOC.
    pub toc: Option<u8>,
}

/// Per-render input passed to the path-based render entry point. Holds the
/// doc, the asset provider, and the destination path. Borrowed so the
/// renderer never owns runtime state.
pub struct RenderRequest<'a> {
    pub doc: &'a ResolvedDoc,
    pub assets: &'a dyn AssetProvider,
    pub out: &'a Path,
}

/// What a backend produced, written to disk. Backends may write multiple
/// files (e.g. HTML + assets); the primary artifact is the one a caller
/// should hand back to the user.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub primary: PathBuf,
    pub extras: Vec<PathBuf>,
}

/// What a backend produced, held entirely in memory — the shape a server
/// embedder wants to hand straight back in an HTTP response body, with no
/// temp file to mint or clean up.
#[derive(Debug, Clone)]
pub struct RenderedArtifact {
    pub primary: Bytes,
    /// Suggested filename, including extension.
    pub filename: String,
    pub extras: Vec<(String, Bytes)>,
}

impl RenderedArtifact {
    /// Write this artifact to disk at `out`, extras alongside it in the same
    /// directory under their own filenames. Used to implement the path-based
    /// `RenderRequest` API over the bytes-first one.
    pub async fn write_to(&self, out: &Path) -> Result<Artifact> {
        if let Some(parent) = out.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(out, &self.primary).await?;

        let dir = out.parent().unwrap_or_else(|| Path::new("."));
        let mut extras = Vec::with_capacity(self.extras.len());
        for (name, bytes) in &self.extras {
            let p = dir.join(name);
            tokio::fs::write(&p, bytes).await?;
            extras.push(p);
        }
        Ok(Artifact {
            primary: out.to_path_buf(),
            extras,
        })
    }
}

/// Every output format is one impl. Pandoc is just one kind of guest.
/// Async because resolving templates and engine subprocesses are inherently async.
///
/// Bytes-first: a backend renders straight into memory. Typst already
/// produces bytes in-process; pandoc still needs a temp file at the
/// subprocess boundary, but owns that temp lifecycle internally and reads the
/// result back before returning — no file ever escapes to the caller.
pub trait Backend: Send + Sync {
    fn target(&self) -> Target;

    fn render_to_bytes<'a>(
        &'a self,
        doc: &'a ResolvedDoc,
        assets: &'a dyn AssetProvider,
    ) -> BoxFuture<'a, Result<RenderedArtifact>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_to_creates_parent_dirs_and_writes_primary_and_extras() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("nested").join("sub").join("out.html");
        let artifact = RenderedArtifact {
            primary: Bytes::from_static(b"<html></html>"),
            filename: "out.html".to_string(),
            extras: vec![
                ("out.css".to_string(), Bytes::from_static(b"body{}")),
                ("out.js".to_string(), Bytes::from_static(b"console.log(1)")),
            ],
        };

        let written = artifact.write_to(&out).await.unwrap();

        assert_eq!(written.primary, out);
        assert_eq!(tokio::fs::read(&out).await.unwrap(), b"<html></html>");

        let expected_extras = vec![
            dir.path().join("nested").join("sub").join("out.css"),
            dir.path().join("nested").join("sub").join("out.js"),
        ];
        assert_eq!(written.extras, expected_extras);
        assert_eq!(
            tokio::fs::read(&expected_extras[0]).await.unwrap(),
            b"body{}"
        );
        assert_eq!(
            tokio::fs::read(&expected_extras[1]).await.unwrap(),
            b"console.log(1)"
        );
    }
}
