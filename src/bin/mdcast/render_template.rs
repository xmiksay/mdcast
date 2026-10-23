//! `render-template` subcommand: user-supplied typst template + a JSON data
//! file → PDF or HTML, no markdown involved. Thin wrapper over
//! `mdcast::backends::typst::render_template_with`/`render_template_html_with`
//! — mirrors `load_doc`'s `--assets`/`--brand` wiring in `main.rs`, but
//! there's no `ResolvedDoc` to build here (see `TemplateDoc`'s module docs
//! for why).
//!
//! `--generated FILE.json` takes the wire's `[GeneratedAsset, …]` array. Each
//! `path` is relative to the template's directory — so the same
//! `#image("qr.svg")` works here and against `mdcast-server`, where the
//! template sits at the root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use mdcast::backends::typst::{
    TemplateDoc, TypstCompiler, render_template_html_with, render_template_with,
};
use mdcast::mdcast_api::wire::GeneratedAsset;
use mdcast::{
    Artifact, AssetProvider, BrandHandle, DocMeta, EmbeddedAssets, LayeredAssets, MemoryAssets,
    RenderedArtifact,
};

use crate::fs_assets::FsAssets;
use crate::load_doc::load_brand;

#[derive(clap::Args)]
pub struct RenderTemplateArgs {
    /// AssetProvider key of the main `.typ` file.
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
    /// JSON array of generated images (`[{"path": "qr.svg", "kind": "qr",
    /// "data": "…"}]`), each served at its `path` relative to the template's
    /// directory. Needs the `qr` feature for `qr` entries.
    #[arg(long)]
    generated: Option<PathBuf>,
    /// Output format. `html` always works with `--typst-compiler cli`;
    /// with the default (`library`) it's only a valid value when the
    /// binary was built with the `typst-html` feature (off by
    /// default — see README's "Data-driven template rendering" section).
    #[arg(long, value_enum, default_value_t = Format::Pdf)]
    format: Format,
}

/// Output format for `render-template`. Both variants are always available —
/// selecting `Html` with `--typst-compiler library` (the default) still
/// needs the `typst-html` cargo feature and fails at compile time otherwise
/// (from `mdcast::backends::typst::render_template_html_with`); with
/// `--typst-compiler cli` it needs no such build feature (issue #73).
#[derive(Clone, Copy, Default, clap::ValueEnum)]
pub enum Format {
    #[default]
    Pdf,
    Html,
}

pub async fn run(args: RenderTemplateArgs, compiler: &TypstCompiler) -> Result<Artifact> {
    let RenderTemplateArgs {
        template,
        data,
        out,
        brand,
        assets,
        generated,
        format,
    } = args;
    let brand_spec = load_brand(brand.as_deref()).await?;
    let data = load_data(&data).await?;
    let doc = TemplateDoc {
        template,
        data,
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(brand_spec)),
        // The CLI reads templates from a real filesystem, where host font
        // search already sees any face the user cares about — no flag yet.
        fonts: Vec::new(),
    };

    let mut provider: Box<dyn AssetProvider> = Box::new(EmbeddedAssets);
    if let Some(dir) = assets {
        provider = Box::new(LayeredAssets {
            over: FsAssets(dir),
            base: provider,
        });
    }
    if let Some(path) = generated {
        let files = load_generated(&path, &doc.template, &provider).await?;
        provider = Box::new(LayeredAssets {
            over: files,
            base: provider,
        });
    }
    render(&doc, &provider, format, compiler)
        .await?
        .write_to(&out)
        .await
}

/// Render every entry of a `--generated` file, keyed under the template's
/// directory. A path the provider already serves is an error rather than a
/// silent shadow — same rule as the server's manifest collision check.
async fn load_generated(
    path: &Path,
    template: &str,
    provider: &dyn AssetProvider,
) -> Result<MemoryAssets> {
    let s = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let entries: Vec<GeneratedAsset> =
        serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?;
    let dir = template.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut files = BTreeMap::new();
    for GeneratedAsset { path: rel, spec } in entries {
        let key = if dir.is_empty() {
            rel.clone()
        } else {
            format!("{dir}/{rel}")
        };
        if files.contains_key(&key) || provider.get(&key).await?.is_some() {
            bail!("generated path {rel:?} collides with an existing asset {key:?}");
        }
        let bytes = generate(&spec).with_context(|| format!("generated entry {rel:?}"))?;
        files.insert(key, bytes);
    }
    Ok(MemoryAssets(files))
}

#[cfg(feature = "qr")]
fn generate(spec: &mdcast::mdcast_api::wire::GeneratedSpec) -> Result<bytes::Bytes> {
    mdcast::generators::render_spec(spec)
}

#[cfg(not(feature = "qr"))]
fn generate(spec: &mdcast::mdcast_api::wire::GeneratedSpec) -> Result<bytes::Bytes> {
    bail!("generator {spec:?} is not available: this mdcast build has no generator features")
}

async fn render(
    doc: &TemplateDoc,
    provider: &dyn AssetProvider,
    format: Format,
    compiler: &TypstCompiler,
) -> Result<RenderedArtifact> {
    match format {
        Format::Pdf => render_template_with(doc, provider, compiler).await,
        Format::Html => render_template_html_with(doc, provider, compiler).await,
    }
}

async fn load_data(path: &Path) -> Result<serde_json::Value> {
    let s = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parse {} as JSON", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_data_parses_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        tokio::fs::write(&path, r#"{"number": "INV-042", "total": 12.5}"#)
            .await
            .unwrap();

        let value = load_data(&path).await.unwrap();

        assert_eq!(value["number"], "INV-042");
        assert_eq!(value["total"], 12.5);
    }

    #[tokio::test]
    async fn load_data_rejects_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        tokio::fs::write(&path, "not json").await.unwrap();

        let err = load_data(&path).await.unwrap_err();
        assert!(err.to_string().contains("data.json"));
    }

    fn invoice_template() -> &'static str {
        "#let invoice = json(\"/data.json\")\n= Invoice #invoice.number"
    }

    fn args(dir: &Path, data: PathBuf, out: PathBuf, format: Format) -> RenderTemplateArgs {
        RenderTemplateArgs {
            template: "templates/invoice.typ".to_string(),
            data,
            out,
            brand: None,
            assets: Some(dir.to_path_buf()),
            generated: None,
            format,
        }
    }

    #[cfg(feature = "qr")]
    #[tokio::test]
    async fn generated_qr_is_served_next_to_the_template() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("templates"))
            .await
            .unwrap();
        tokio::fs::write(
            dir.path().join("templates/invoice.typ"),
            "#image(\"qr.svg\", width: 3cm)",
        )
        .await
        .unwrap();
        let data = dir.path().join("data.json");
        tokio::fs::write(&data, "{}").await.unwrap();
        let generated = dir.path().join("generated.json");
        tokio::fs::write(
            &generated,
            r#"[{"path": "qr.svg", "kind": "qr", "data": "SPD*1.0*AM:1"}]"#,
        )
        .await
        .unwrap();
        let mut a = args(dir.path(), data, dir.path().join("out.pdf"), Format::Pdf);
        a.generated = Some(generated);

        let artifact = run(a, &TypstCompiler::default())
            .await
            .unwrap_or_else(|e| panic!("run failed: {e:#}"));

        let bytes = tokio::fs::read(&artifact.primary).await.unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[cfg(feature = "qr")]
    #[tokio::test]
    async fn generated_path_colliding_with_an_existing_asset_is_an_error() {
        let dir = invoice_dir().await;
        let generated = dir.path().join("generated.json");
        tokio::fs::write(
            &generated,
            r#"[{"path": "invoice.typ", "kind": "qr", "data": "x"}]"#,
        )
        .await
        .unwrap();
        let provider = FsAssets(dir.path().to_path_buf());

        let err = load_generated(&generated, "templates/invoice.typ", &provider)
            .await
            .unwrap_err();

        assert!(format!("{err:#}").contains("collides"), "{err:#}");
    }

    async fn invoice_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("templates"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("templates/invoice.typ"), invoice_template())
            .await
            .unwrap();
        dir
    }

    /// End-to-end through the `--format html` flag: `run` picks
    /// `render_template_html_with` over `render_template_with` and the file
    /// written to `--out` is real HTML, not a PDF.
    #[cfg(feature = "typst-html")]
    #[tokio::test]
    async fn run_dispatches_to_html_export_when_format_is_html() {
        let dir = invoice_dir().await;
        let data_path = dir.path().join("data.json");
        tokio::fs::write(&data_path, r#"{"number": "INV-042"}"#)
            .await
            .unwrap();
        let out = dir.path().join("out.html");

        let artifact = run(
            args(dir.path(), data_path, out, Format::Html),
            &TypstCompiler::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("run failed: {e:#}"));

        let bytes = tokio::fs::read(&artifact.primary).await.unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(html.contains("<html"), "not an html document: {html}");
        assert!(html.contains("Invoice INV-042"), "{html}");
    }

    /// The same `--format html` flag works with `--typst-compiler cli`
    /// without the `typst-html` cargo feature (issue #73) — a real `typst`
    /// binary supports `--features html` regardless of how mdcast itself was
    /// built. Skips gracefully when no `typst` binary is on `PATH`, matching
    /// the pandoc convention in `tests/render_smoke.rs`.
    #[tokio::test]
    async fn run_dispatches_to_html_export_in_cli_mode_without_the_feature() {
        let Some(bin) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("typst"))
                .find(|p| p.is_file())
                .map(|_| "typst".to_string())
        }) else {
            eprintln!("skipping: no `typst` binary on PATH");
            return;
        };

        let dir = invoice_dir().await;
        let data_path = dir.path().join("data.json");
        tokio::fs::write(&data_path, r#"{"number": "INV-042"}"#)
            .await
            .unwrap();
        let out = dir.path().join("out.html");
        let compiler = TypstCompiler::Cli(mdcast::backends::typst::CliCompiler::new(
            bin,
            std::time::Duration::from_secs(20),
        ));

        let artifact = run(args(dir.path(), data_path, out, Format::Html), &compiler)
            .await
            .unwrap_or_else(|e| panic!("run failed: {e:#}"));

        let bytes = tokio::fs::read(&artifact.primary).await.unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(html.contains("<html"), "not an html document: {html}");
        assert!(html.contains("Invoice INV-042"), "{html}");
    }
}
