//! mdcast CLI. Thin wrapper around the library; default `EmbeddedAssets` is
//! used unless `--assets DIR` layers a filesystem provider on top. `--brand`
//! supplies the `brand.toml` (auto-layout rules, palette, fonts).

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mdcast::backends::Registry;
use mdcast::{EmbeddedAssets, LayeredAssets, RenderRequest, Target};

mod fs_assets;
mod load_doc;
#[cfg(feature = "typst")]
mod render_template;
#[cfg(feature = "typst")]
mod typst_compiler_args;

use fs_assets::FsAssets;
use load_doc::{PreSteps, load_doc};
#[cfg(feature = "typst")]
use typst_compiler_args::TypstCompilerArgs;

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
        #[command(flatten)]
        steps: PreSteps,
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
        /// Ignored by pandoc targets.
        #[cfg(feature = "typst")]
        #[command(flatten)]
        typst_compiler: TypstCompilerArgs,
    },
    /// Print per-page (class, origin) for an input — useful for debugging the
    /// auto-classifier and explicit-wrapper parsing.
    Explain {
        input: PathBuf,
        #[arg(long)]
        brand: Option<PathBuf>,
        /// Same flags as `render` — lets the auto-classifier see a diagram or
        /// QR code or chess board as an image (e.g. a diagram-only page
        /// classifies image-full). `<qr path="…">` / `<fen path="…">` resolve
        /// against the embedded catalog only.
        #[command(flatten)]
        steps: PreSteps,
    },
    /// Render a user-supplied typst template against structured data — no
    /// markdown involved. `template` is an AssetProvider key (e.g.
    /// "templates/invoice.typ"); its own directory is scanned for sibling
    /// `#import`s/images (see `TemplateDoc`'s docs).
    #[cfg(feature = "typst")]
    RenderTemplate {
        #[command(flatten)]
        args: render_template::RenderTemplateArgs,
        #[command(flatten)]
        typst_compiler: TypstCompilerArgs,
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
            steps,
            toc_depth,
            layout_assets,
            layout_fonts,
            #[cfg(feature = "typst")]
            typst_compiler,
        } => {
            // Provider stack, innermost first: embedded catalog, `--assets`
            // dir override, then the SVGs the pre-steps render. Boxed because
            // each layer is optional.
            let mut provider: Box<dyn mdcast::AssetProvider> = Box::new(EmbeddedAssets);
            if let Some(dir) = assets {
                provider = Box::new(LayeredAssets {
                    over: FsAssets(dir),
                    base: provider,
                });
            }
            let (doc, generated) = load_doc(
                &input,
                brand.as_deref(),
                steps,
                &provider,
                toc_depth,
                layout_assets,
                layout_fonts,
            )
            .await?;
            #[cfg(feature = "typst")]
            let registry = Registry::with_typst_compiler(typst_compiler.build().await);
            #[cfg(not(feature = "typst"))]
            let registry = Registry::with_defaults();
            if !generated.is_empty() {
                let svgs: std::collections::HashMap<String, bytes::Bytes> =
                    generated.into_iter().collect();
                provider = Box::new(LayeredAssets {
                    over: mdcast::sync_provider(move |k| Ok(svgs.get(k).cloned())),
                    base: provider,
                });
            }
            let req = RenderRequest {
                doc: &doc,
                assets: &provider,
                out: &out,
            };
            let artifact = registry.render(target.into(), &req).await?;
            println!("wrote {}", artifact.primary.display());
        }
        Cmd::Explain {
            input,
            brand,
            steps,
        } => {
            let (doc, _) = load_doc(
                &input,
                brand.as_deref(),
                steps,
                &EmbeddedAssets,
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
            args,
            typst_compiler,
        } => {
            let compiler = typst_compiler.build().await;
            let artifact = render_template::run(args, &compiler).await?;
            println!("wrote {}", artifact.primary.display());
        }
    }
    Ok(())
}
