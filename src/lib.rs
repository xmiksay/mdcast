//! mdcast — Markdown → DOCX/PDF/PPTX/HTML with per-page layout templates.

pub mod assets;
pub mod backends;
pub mod brand;
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
/// `AssetProvider` (e.g. an SVG rendered from a Mermaid pre-step).
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
        Ok(Artifact { primary: out.to_path_buf(), extras })
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
