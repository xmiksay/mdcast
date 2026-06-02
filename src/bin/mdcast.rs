//! mdcast CLI. Thin wrapper around the library; default `EmbeddedAssets` is
//! used unless overridden by `--brand` (which can layer custom assets on top).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use bytes::Bytes;
use mdcast::backends::Registry;
use mdcast::pages::auto::classify;
use mdcast::pages::splitter::DefaultSplitter;
use mdcast::{
    AssetProvider, AssetRef, BrandHandle, BrandSpec, DocMeta, EmbeddedAssets, HtmlImageTags,
    Identity, LayeredAssets, MarkdownPreprocessor, PageSplitter, RenderRequest, ResolvedDoc,
    Target,
};

#[derive(Parser)]
#[command(name = "mdcast", about = "Markdown → DOCX/PDF/PPTX/HTML")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render an input markdown file to a target format.
    Render {
        input: PathBuf,
        #[arg(short, long)]
        target: TargetArg,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        brand: Option<PathBuf>,
        /// Directory layered over `EmbeddedAssets`. Asset keys map to relative
        /// paths inside this dir. Lets you override built-in templates or
        /// supply images referenced from markdown without writing code.
        #[arg(long)]
        assets: Option<PathBuf>,
        /// Enable the built-in HtmlImageTags preprocessor: `<img src="X">` /
        /// `<image path="X">` become standard `![](X)` before splitting.
        #[arg(long, default_value_t = false)]
        html_image_tags: bool,
    },
    /// Print per-page (class, origin) for an input — useful for debugging the
    /// auto-classifier and explicit-wrapper parsing.
    Explain {
        input: PathBuf,
        #[arg(long)]
        brand: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        html_image_tags: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum TargetArg {
    Docx,
    Odt,
    Pdf,
    PdfPresentation,
    Pptx,
    HtmlReveal,
}

impl From<TargetArg> for Target {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Docx => Target::Docx,
            TargetArg::Odt => Target::Odt,
            TargetArg::Pdf => Target::Pdf,
            TargetArg::PdfPresentation => Target::PdfPresentation,
            TargetArg::Pptx => Target::Pptx,
            TargetArg::HtmlReveal => Target::HtmlReveal,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Render { input, target, out, brand, assets, html_image_tags } => {
            let doc = load_doc(&input, brand.as_deref(), html_image_tags).await?;
            let registry = Registry::with_defaults();
            let artifact = match assets {
                Some(dir) => {
                    let provider = LayeredAssets { over: FsAssets(dir), base: EmbeddedAssets };
                    let req = RenderRequest { doc: &doc, assets: &provider, out: &out };
                    registry.render(target.into(), &req).await?
                }
                None => {
                    let req = RenderRequest { doc: &doc, assets: &EmbeddedAssets, out: &out };
                    registry.render(target.into(), &req).await?
                }
            };
            println!("wrote {}", artifact.primary.display());
        }
        Cmd::Explain { input, brand, html_image_tags } => {
            let doc = load_doc(&input, brand.as_deref(), html_image_tags).await?;
            for (i, page) in doc.pages.iter().enumerate() {
                println!("page {:>3}  class={:<20}  origin={:?}", i, page.class, page.origin);
            }
        }
    }
    Ok(())
}

/// Minimal filesystem-backed provider. Keys map to relative paths inside `root`.
/// Used by the CLI's `--assets` flag; libraries should impl `AssetProvider`
/// themselves for real overrides (DB, S3, in-memory map …).
struct FsAssets(PathBuf);

impl AssetProvider for FsAssets {
    fn get<'a>(&'a self, key: &'a str) -> mdcast::BoxFuture<'a, anyhow::Result<Option<Bytes>>> {
        Box::pin(async move {
            let path = self.0.join(key);
            match tokio::fs::read(&path).await {
                Ok(b) => Ok(Some(Bytes::from(b))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn list<'a>(&'a self, _prefix: &'a str) -> mdcast::BoxFuture<'a, anyhow::Result<Vec<String>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

async fn load_doc(
    input: &std::path::Path,
    brand: Option<&std::path::Path>,
    html_image_tags: bool,
) -> Result<ResolvedDoc> {
    let md = tokio::fs::read_to_string(input)
        .await
        .with_context(|| format!("read {}", input.display()))?;
    let brand_spec: BrandSpec = match brand {
        Some(p) => {
            let s = tokio::fs::read_to_string(p).await?;
            BrandSpec::from_toml(&s)?
        }
        None => BrandSpec::default(),
    };
    // Preprocessor stage: rewrites the whole document before any other pipeline
    // step sees it. The CLI exposes only the built-in HtmlImageTags via flag;
    // library callers compose their own MarkdownPreprocessor.
    let preprocessor: Box<dyn MarkdownPreprocessor> = if html_image_tags {
        Box::new(HtmlImageTags)
    } else {
        Box::new(Identity)
    };
    let md = preprocessor.preprocess(&md);

    let raw = DefaultSplitter.split(&md);
    let pages = classify(raw, &brand_spec.auto_layout);
    Ok(ResolvedDoc {
        pages,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(brand_spec)),
        assets: Vec::<AssetRef>::new(),
    })
}
