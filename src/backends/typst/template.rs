//! Data-driven template rendering: a user-supplied typst template + structured
//! data → PDF (or, behind the `typst-html` feature, HTML — issue #53),
//! bypassing the markdown pipeline (splitter, classifier, `md_to_typst`,
//! driver) entirely. A parallel entry point into the same engine plumbing the
//! markdown path uses — in-process compile, `AssetProvider`-only file access,
//! `/context.typ` for `DocMeta`/`BrandSpec` — rather than a second
//! `ResolvedDoc`-shaped IR. `Registry`/`Backend` stay untouched: they're
//! `Target`-keyed and markdown-shaped, and there is no `Target` variant a
//! data-driven render belongs under.
//!
//! [`render_template`] and [`render_template_html`] share everything up to
//! the export step (`assemble` + `build_engine`) — same template, same data,
//! same sibling/asset resolution. Only the final `typst_pdf::pdf(...)` vs
//! `typst_html::html(...)` call differs, so a template author can point the
//! same `.typ` file at both without maintaining two sources of truth.
//!
//! `data` is serialized to JSON and registered as a virtual `/data.json` the
//! template reads with typst's own `json()` — no custom serialization dialect
//! to design or escape, unlike `/context.typ`'s flat string-dict projection.
//!
//! Sibling files under the template's own directory (partials it `#import`s,
//! images it `#image`s) are discovered via `AssetProvider::list` on that
//! directory and registered at the same virtual path as their provider key,
//! so a relative reference like `#import "partials/header.typ"` from
//! `templates/invoice.typ` resolves exactly as it would on a real filesystem
//! — typst joins a relative import against the *importing* file's own
//! virtual path, and that's the provider key here. `.typ` siblings register
//! as typst sources (so `#import` can parse them); everything else registers
//! as a binary file (so `#image` can read it). A template with no `/` in its
//! key has no directory to scope discovery to, so every provider key is
//! listed — fine for a small filesystem-backed provider, wasteful against a
//! large embedded catalog, so keep templates under their own subdirectory.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::future::try_join_all;
use typst::syntax::{FileId, Source, VirtualPath};
use typst_as_lib::TypstEngine;

use crate::assets::AssetProvider;
use crate::{BrandHandle, DocMeta, RenderedArtifact};

use super::context::{CONTEXT_VIRTUAL_PATH, build_context_source};

/// Virtual path the serialized `TemplateDoc.data` is registered under. A
/// template reads it with typst's native `json("/data.json")`.
pub const DATA_VIRTUAL_PATH: &str = "data.json";

/// Everything a data-driven template render needs — no markdown, no pages.
/// The caller's "data loader" (a DB row, an API response, …) produces `data`;
/// mdcast's contract starts at `(template, data, brand) → bytes`.
pub struct TemplateDoc {
    /// `AssetProvider` key of the main `.typ` file, e.g.
    /// `"templates/invoice.typ"`.
    pub template: String,
    /// Arbitrary structured payload, serialized to `/data.json`.
    pub data: serde_json::Value,
    pub meta: DocMeta,
    pub brand: BrandHandle,
}

/// Render `doc.template` against `doc.data` to a PDF. Reuses the same
/// `/context.typ` (`doc-meta`, `brand`, and their accessor helpers) as the
/// markdown pipeline; `asset-path` degrades every key to its `default:`
/// since template mode has no `ResolvedDoc.assets` — a template reaches its
/// own chrome via sibling files instead (see module docs).
pub async fn render_template(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
) -> Result<RenderedArtifact> {
    let (main_id, sources, binaries) = assemble(doc, assets).await?;

    let dispatch = tracing::dispatcher::get_default(|d| d.clone());
    let pdf_bytes = tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&dispatch, || {
            compile_template_pdf(&main_id, sources, binaries)
        })
    })
    .await
    .context("typst compile thread panicked")??;

    Ok(RenderedArtifact {
        primary: Bytes::from(pdf_bytes),
        filename: "output.pdf".to_string(),
        extras: vec![],
    })
}

/// Render `doc.template` against `doc.data` to an HTML page — same template,
/// same data, only the export step differs from [`render_template`] (issue
/// #53). Experimental upstream: typst's HTML export and the `target()`
/// function it relies on for dual-target branching are unstable and may
/// shift across typst versions; see README's "Data-driven template
/// rendering" section for the `target()` branching pattern a shared template
/// needs.
#[cfg(feature = "typst-html")]
pub async fn render_template_html(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
) -> Result<RenderedArtifact> {
    let (main_id, sources, binaries) = assemble(doc, assets).await?;

    let dispatch = tracing::dispatcher::get_default(|d| d.clone());
    let html_bytes = tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&dispatch, || {
            compile_template_html(&main_id, sources, binaries)
        })
    })
    .await
    .context("typst compile thread panicked")??;

    Ok(RenderedArtifact {
        primary: Bytes::from(html_bytes),
        filename: "output.html".to_string(),
        extras: vec![],
    })
}

/// Fetches the template, its siblings, and `doc.data`/`/context.typ` from the
/// `AssetProvider` and turns them into the `(main file id, sources,
/// binaries)` a `TypstEngine` needs to compile — shared by [`render_template`]
/// and [`render_template_html`], which differ only in the export step.
async fn assemble(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
) -> Result<(String, Vec<Source>, Vec<(String, Vec<u8>)>)> {
    let Some(template_bytes) = assets.get(&doc.template).await? else {
        bail!(
            "typst template '{}' not found in asset provider",
            doc.template
        );
    };
    let template_src = String::from_utf8(template_bytes.to_vec())
        .with_context(|| format!("template '{}' is not valid UTF-8", doc.template))?;

    let prefix = match doc.template.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    };
    let sibling_keys: Vec<String> = assets
        .list(&prefix)
        .await?
        .into_iter()
        .filter(|k| k != &doc.template)
        .collect();
    let siblings: Vec<(String, Bytes)> =
        try_join_all(sibling_keys.into_iter().map(|key| async move {
            let Some(bytes) = assets.get(&key).await? else {
                bail!("template asset '{key}' listed by provider but not found on fetch");
            };
            Ok::<(String, Bytes), anyhow::Error>((key, bytes))
        }))
        .await?;

    let mut sources = Vec::with_capacity(siblings.len() + 2);
    let context_source = build_context_source(&doc.meta, &doc.brand.0, &BTreeMap::new());
    sources.push(Source::new(
        FileId::new(None, VirtualPath::new(CONTEXT_VIRTUAL_PATH)),
        context_source,
    ));
    sources.push(Source::new(
        FileId::new(None, VirtualPath::new(doc.template.as_str())),
        template_src,
    ));

    let mut binaries: Vec<(String, Vec<u8>)> = vec![(
        DATA_VIRTUAL_PATH.to_string(),
        serde_json::to_vec(&doc.data).context("serialize TemplateDoc.data to JSON")?,
    )];
    for (key, bytes) in siblings {
        if key.ends_with(".typ") {
            let src = String::from_utf8(bytes.to_vec())
                .with_context(|| format!("template partial '{key}' is not valid UTF-8"))?;
            sources.push(Source::new(
                FileId::new(None, VirtualPath::new(key.as_str())),
                src,
            ));
        } else {
            binaries.push((key, bytes.to_vec()));
        }
    }

    Ok((doc.template.clone(), sources, binaries))
}

/// Builds a `TypstEngine` over the assembled sources/binaries. Shared by both
/// export paths — a template has no per-class layouts and brings its own
/// fonts via the host/embedded search only (v1 scope; see module docs).
fn build_engine(
    sources: Vec<Source>,
    binaries: Vec<(String, Vec<u8>)>,
) -> TypstEngine<typst_as_lib::TypstTemplateCollection> {
    let binary_refs: Vec<(&str, Vec<u8>)> = binaries
        .iter()
        .map(|(p, b)| (p.as_str(), b.clone()))
        .collect();

    TypstEngine::builder()
        .with_static_source_file_resolver(sources)
        .with_static_file_resolver(binary_refs)
        .search_fonts_with(typst_as_lib::typst_kit_options::TypstKitFontOptions::default())
        .build()
}

/// Synchronous: builds the engine, compiles, exports to PDF bytes. Mirrors
/// `super::compile_pdf`, minus the driver/layouts/fonts the markdown path
/// needs.
fn compile_template_pdf(
    main_id: &str,
    sources: Vec<Source>,
    binaries: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>> {
    let engine = build_engine(sources, binaries);

    let result = engine.compile(main_id);
    for w in &result.warnings {
        tracing::warn!("typst: {}", w.message);
        for hint in &w.hints {
            tracing::warn!("typst: hint: {hint}");
        }
    }
    let compiled = result.output.map_err(super::format_lib_error)?;

    let options = typst_pdf::PdfOptions::default();
    let pdf = typst_pdf::pdf(&compiled, &options).map_err(super::format_diagnostics)?;
    Ok(pdf)
}

/// Synchronous: builds the engine, compiles to typst's experimental
/// `HtmlDocument`, exports to an HTML string. `typst-as-lib`'s `typst-html`
/// cargo feature (enabled transitively by this crate's own `typst-html`
/// feature) flips `typst::Feature::Html` on for the engine's library, which
/// is what makes the `target()` function a template branches on available in
/// the first place — see README's "writing a dual-target template" note.
#[cfg(feature = "typst-html")]
fn compile_template_html(
    main_id: &str,
    sources: Vec<Source>,
    binaries: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>> {
    let engine = build_engine(sources, binaries);

    let result: typst::diag::Warned<Result<typst_html::HtmlDocument, _>> = engine.compile(main_id);
    for w in &result.warnings {
        tracing::warn!("typst: {}", w.message);
        for hint in &w.hints {
            tracing::warn!("typst: hint: {hint}");
        }
    }
    let compiled = result.output.map_err(super::format_lib_error)?;

    let html = typst_html::html(&compiled).map_err(super::format_diagnostics)?;
    Ok(html.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrandSpec;
    use crate::assets::sync_provider;
    use std::sync::Arc;

    fn doc(template: &str, data: serde_json::Value) -> TemplateDoc {
        TemplateDoc {
            template: template.to_string(),
            data,
            meta: DocMeta::default(),
            brand: BrandHandle(Arc::new(BrandSpec::default())),
        }
    }

    #[tokio::test]
    async fn missing_template_key_is_a_clear_error() {
        let assets = sync_provider(|_| Ok(None));
        let d = doc("templates/does-not-exist.typ", serde_json::json!({}));

        let err = render_template(&d, &assets).await.unwrap_err();

        assert!(
            err.to_string().contains("templates/does-not-exist.typ"),
            "error should name the missing key: {err}"
        );
    }

    #[tokio::test]
    async fn non_utf8_template_is_a_clear_error() {
        let assets = sync_provider(|key| match key {
            "templates/bad.typ" => Ok(Some(Bytes::from_static(&[0xff, 0xfe, 0xfd]))),
            _ => Ok(None),
        });
        let d = doc("templates/bad.typ", serde_json::json!({}));

        let err = render_template(&d, &assets).await.unwrap_err();

        assert!(err.to_string().contains("templates/bad.typ"));
    }

    #[tokio::test]
    async fn real_compile_reads_data_json_and_context() {
        const TEMPLATE: &str = r#"
#import "/context.typ": doc-meta
#let invoice = json("/data.json")
= Invoice #invoice.number
#doc-meta.title
"#;
        let assets = sync_provider(|key| match key {
            "templates/invoice.typ" => Ok(Some(Bytes::from_static(TEMPLATE.as_bytes()))),
            _ => Ok(None),
        });
        let mut d = doc(
            "templates/invoice.typ",
            serde_json::json!({"number": "INV-042"}),
        );
        d.meta.title = Some("Q3 Invoice".to_string());

        let artifact = render_template(&d, &assets).await.unwrap();

        assert!(artifact.primary.starts_with(b"%PDF-"));
    }

    /// Unlike `sync_provider` (whose `list` is always empty), this backs
    /// `list` with a real prefix scan — needed to exercise sibling discovery,
    /// which `sync_provider`-based tests above can't reach.
    struct MapAssets(BTreeMap<&'static str, &'static [u8]>);

    impl AssetProvider for MapAssets {
        fn get<'a>(&'a self, key: &'a str) -> crate::assets::BoxFuture<'a, Result<Option<Bytes>>> {
            let v = self.0.get(key).map(|b| Bytes::from_static(b));
            Box::pin(async move { Ok(v) })
        }

        fn list<'a>(
            &'a self,
            prefix: &'a str,
        ) -> crate::assets::BoxFuture<'a, Result<Vec<String>>> {
            let out: Vec<String> = self
                .0
                .keys()
                .filter(|k| k.starts_with(prefix))
                .map(|k| k.to_string())
                .collect();
            Box::pin(async move { Ok(out) })
        }
    }

    #[tokio::test]
    async fn sibling_typ_partial_and_image_resolve_by_relative_path() {
        const MAIN: &str = r#"
#import "partials/header.typ": greeting
#figure(image("logo.svg", width: 1cm))
#greeting
"#;
        const PARTIAL: &str = "#let greeting = [Hello]";
        const LOGO: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;

        let assets = MapAssets(BTreeMap::from([
            ("templates/invoice.typ", MAIN.as_bytes()),
            ("templates/partials/header.typ", PARTIAL.as_bytes()),
            ("templates/logo.svg", LOGO),
        ]));
        let d = doc("templates/invoice.typ", serde_json::json!({}));

        let artifact = render_template(&d, &assets).await.unwrap();

        assert!(artifact.primary.starts_with(b"%PDF-"));
    }
}
