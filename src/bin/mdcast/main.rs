//! mdcast CLI. Thin wrapper around the library; default `EmbeddedAssets` is
//! used unless `--assets DIR` layers a filesystem provider on top. `--brand`
//! supplies the `brand.toml` (auto-layout rules, palette, fonts).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mdcast::backends::Registry;
use mdcast::pages::auto::classify;
use mdcast::pages::splitter::DefaultSplitter;
use mdcast::{
    AssetRef, BrandHandle, BrandSpec, EmbeddedAssets, HtmlImageTags, Identity, LayeredAssets,
    MarkdownPreprocessor, PageSplitter, RenderRequest, ResolvedDoc, Target,
};

mod fs_assets;
#[cfg(feature = "typst")]
mod render_template;

use fs_assets::FsAssets;

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
        /// Request a table of contents at the given heading depth (1-6).
        /// Honoured by docx/odt (`--toc --toc-depth`) and by `pdf` (a leading
        /// `#outline(depth: N)` page); ignored by pdf-presentation/pptx/html-reveal.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=6))]
        toc_depth: Option<u8>,
        /// Asset key a typst layout may reach via `#import "/context.typ":
        /// asset-path` (e.g. a brand logo). Repeatable. Resolved through the
        /// same `--assets`/embedded provider; ignored by pandoc targets.
        #[arg(long = "layout-asset")]
        layout_assets: Vec<String>,
        /// Font asset key (`.ttf`/`.otf`) to register with the typst font
        /// book before compiling, so `#set text(font: "...")` resolves it
        /// with no host install. Repeatable. Resolved through the same
        /// `--assets`/embedded provider; ignored by pandoc targets.
        #[arg(long = "layout-font")]
        layout_fonts: Vec<String>,
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
    /// Render a user-supplied typst template against structured data — no
    /// markdown involved. `template` is an AssetProvider key (e.g.
    /// "templates/invoice.typ"); its own directory is scanned for sibling
    /// `#import`s/images (see `TemplateDoc`'s docs).
    #[cfg(feature = "typst")]
    RenderTemplate {
        template: String,
        /// JSON file read into the template as `json("/data.json")`.
        #[arg(long)]
        data: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        brand: Option<PathBuf>,
        /// Directory layered over `EmbeddedAssets`. Provider keys map to
        /// relative paths inside this dir — the same `--assets` flag `render`
        /// uses.
        #[arg(long)]
        assets: Option<PathBuf>,
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
        Cmd::Render {
            input,
            target,
            out,
            brand,
            assets,
            html_image_tags,
            toc_depth,
            layout_assets,
            layout_fonts,
        } => {
            let doc = load_doc(
                &input,
                brand.as_deref(),
                html_image_tags,
                toc_depth,
                layout_assets,
                layout_fonts,
            )
            .await?;
            let registry = Registry::with_defaults();
            let artifact = match assets {
                Some(dir) => {
                    let provider = LayeredAssets {
                        over: FsAssets(dir),
                        base: EmbeddedAssets,
                    };
                    let req = RenderRequest {
                        doc: &doc,
                        assets: &provider,
                        out: &out,
                    };
                    registry.render(target.into(), &req).await?
                }
                None => {
                    let req = RenderRequest {
                        doc: &doc,
                        assets: &EmbeddedAssets,
                        out: &out,
                    };
                    registry.render(target.into(), &req).await?
                }
            };
            println!("wrote {}", artifact.primary.display());
        }
        Cmd::Explain {
            input,
            brand,
            html_image_tags,
        } => {
            let doc = load_doc(
                &input,
                brand.as_deref(),
                html_image_tags,
                None,
                Vec::new(),
                Vec::new(),
            )
            .await?;
            for (i, page) in doc.pages.iter().enumerate() {
                println!(
                    "page {:>3}  class={:<20}  origin={:?}",
                    i, page.class, page.origin
                );
            }
        }
        #[cfg(feature = "typst")]
        Cmd::RenderTemplate {
            template,
            data,
            out,
            brand,
            assets,
        } => {
            let artifact = render_template::run(template, data, out, brand, assets).await?;
            println!("wrote {}", artifact.primary.display());
        }
    }
    Ok(())
}

/// Shared by `load_doc` (markdown pipeline) and `render_template::run`
/// (data-driven template pipeline) — both accept the same `--brand brand.toml`.
async fn load_brand(brand: Option<&std::path::Path>) -> Result<BrandSpec> {
    match brand {
        Some(p) => {
            let s = tokio::fs::read_to_string(p)
                .await
                .with_context(|| format!("read {}", p.display()))?;
            BrandSpec::from_toml(&s).with_context(|| format!("parse {}", p.display()))
        }
        None => Ok(BrandSpec::default()),
    }
}

async fn load_doc(
    input: &std::path::Path,
    brand: Option<&std::path::Path>,
    html_image_tags: bool,
    toc_depth: Option<u8>,
    layout_assets: Vec<String>,
    layout_fonts: Vec<String>,
) -> Result<ResolvedDoc> {
    let md = tokio::fs::read_to_string(input)
        .await
        .with_context(|| format!("read {}", input.display()))?;
    let (meta, md) = mdcast::frontmatter::extract(&md);
    let brand_spec = load_brand(brand).await?;
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
        meta,
        brand: BrandHandle(Arc::new(brand_spec)),
        assets: layout_assets
            .into_iter()
            .map(|key| AssetRef { key })
            .collect(),
        fonts: layout_fonts
            .into_iter()
            .map(|key| AssetRef { key })
            .collect(),
        toc: toc_depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_doc_turns_layout_asset_flags_into_asset_refs() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.md");
        tokio::fs::write(&input, "# Hi").await.unwrap();

        let doc = load_doc(
            &input,
            None,
            false,
            None,
            vec!["branding/logo.svg".to_string(), "bg.png".to_string()],
            Vec::new(),
        )
        .await
        .unwrap();

        let keys: Vec<&str> = doc.assets.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["branding/logo.svg", "bg.png"]);
    }

    #[tokio::test]
    async fn load_doc_turns_layout_font_flags_into_font_refs() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.md");
        tokio::fs::write(&input, "# Hi").await.unwrap();

        let doc = load_doc(
            &input,
            None,
            false,
            None,
            Vec::new(),
            vec!["fonts/Brand-Regular.ttf".to_string()],
        )
        .await
        .unwrap();

        let keys: Vec<&str> = doc.fonts.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["fonts/Brand-Regular.ttf"]);
    }
}
