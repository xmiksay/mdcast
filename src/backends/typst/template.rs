//! Data-driven template rendering: a user-supplied typst template + structured
//! data → PDF (or, on HTML-capable configurations, HTML — issue #53),
//! bypassing the markdown pipeline (splitter, classifier, `md_to_typst`,
//! driver) entirely. A parallel entry point into the same compiler plumbing
//! the markdown path uses — `AssetProvider`-only file access, `/context.typ`
//! for `DocMeta`/`BrandSpec`, the `TypstCompiler` seam (issue #73) — rather
//! than a second `ResolvedDoc`-shaped IR. `Registry`/`Backend` stay
//! untouched: they're `Target`-keyed and markdown-shaped, and there is no
//! `Target` variant a data-driven render belongs under.
//!
//! [`render_template`]/[`render_template_with`] and
//! [`render_template_html`]/[`render_template_html_with`] share everything up
//! to the export step (`assemble` produces the same [`CompileInputs`] shape
//! either way) — same template, same data, same sibling/asset resolution.
//! Only `CompileFormat::Pdf` vs `CompileFormat::Html` differs, so a template
//! author can point the same `.typ` file at both without maintaining two
//! sources of truth. The `_with` variants take an explicit `TypstCompiler`;
//! the plain ones default to `Library`, matching every entry point's
//! pre-issue-#73 behaviour.
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

use crate::assets::AssetProvider;
use crate::{BrandHandle, DocMeta, RenderedArtifact};

use super::compiler::{CompileFormat, CompileInputs, TypstCompiler};
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
    /// `AssetProvider` keys of font faces to register with the compile, in
    /// declaration order (typst tie-breaks by it) — the template path's
    /// counterpart to `ResolvedDoc.fonts` (issue #93). Empty means host /
    /// embedded font search only, the pre-#93 behaviour.
    pub fonts: Vec<String>,
}

/// Render `doc.template` against `doc.data` to a PDF via the default
/// (`Library`) compiler. Reuses the same `/context.typ` (`doc-meta`, `brand`,
/// and their accessor helpers) as the markdown pipeline; `asset-path`
/// degrades every key to its `default:` since template mode has no
/// `ResolvedDoc.assets` — a template reaches its own chrome via sibling files
/// instead (see module docs).
pub async fn render_template(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
) -> Result<RenderedArtifact> {
    render_template_with(doc, assets, &TypstCompiler::default()).await
}

/// Same as [`render_template`], against an explicitly selected
/// [`TypstCompiler`] rather than always `Library` (issue #73) — what
/// `mdcast-server` and the CLI call once a compiler is configured.
pub async fn render_template_with(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
    compiler: &TypstCompiler,
) -> Result<RenderedArtifact> {
    compile_template(doc, assets, CompileFormat::Pdf, compiler).await
}

/// Render `doc.template` against `doc.data` to an HTML page via the default
/// (`Library`) compiler — same template, same data, only the export step
/// differs from [`render_template`] (issue #53). Experimental upstream:
/// typst's HTML export and the `target()` function it relies on for
/// dual-target branching are unstable and may shift across typst versions;
/// see README's "Data-driven template rendering" section for the `target()`
/// branching pattern a shared template needs. Gated behind the `typst-html`
/// cargo feature because the in-process compiler needs it (see
/// `compiler/library.rs`'s module docs) — [`render_template_html_with`] has
/// no such restriction.
#[cfg(feature = "typst-html")]
pub async fn render_template_html(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
) -> Result<RenderedArtifact> {
    render_template_html_with(doc, assets, &TypstCompiler::default()).await
}

/// Same as [`render_template_html`], against an explicitly selected
/// [`TypstCompiler`], and — unlike [`render_template_html`] — not gated
/// behind the `typst-html` cargo feature: a `TypstCompiler::Cli` compile
/// shells out to a real `typst` binary and needs no such build-time toggle
/// (issue #73). This is what lets `mdcast-server` answer `format: "html"` in
/// `cli` mode on a standard build. Passing `TypstCompiler::Library` without
/// the `typst-html` feature still fails, from `compiler::library::compile`.
pub async fn render_template_html_with(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
    compiler: &TypstCompiler,
) -> Result<RenderedArtifact> {
    compile_template(doc, assets, CompileFormat::Html, compiler).await
}

async fn compile_template(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
    format: CompileFormat,
    compiler: &TypstCompiler,
) -> Result<RenderedArtifact> {
    let inputs = assemble(doc, assets, format, compiler).await?;
    let bytes = compiler.compile(inputs).await?;
    let filename = match format {
        CompileFormat::Pdf => "output.pdf",
        CompileFormat::Html => "output.html",
    };
    Ok(RenderedArtifact {
        primary: Bytes::from(bytes),
        filename: filename.to_string(),
        extras: vec![],
    })
}

/// Fetches the template, its siblings, and `doc.data`/`/context.typ` from the
/// `AssetProvider` and turns them into the [`CompileInputs`] either compiler
/// needs.
async fn assemble(
    doc: &TemplateDoc,
    assets: &dyn AssetProvider,
    format: CompileFormat,
    compiler: &TypstCompiler,
) -> Result<CompileInputs> {
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

    let mut sources: Vec<(String, String)> = Vec::with_capacity(siblings.len() + 2);
    let context_source = build_context_source(&doc.meta, &doc.brand.0, &BTreeMap::new());
    sources.push((CONTEXT_VIRTUAL_PATH.to_string(), context_source));
    sources.push((doc.template.clone(), template_src));

    let mut files: Vec<(String, Vec<u8>)> = vec![(
        DATA_VIRTUAL_PATH.to_string(),
        serde_json::to_vec(&doc.data).context("serialize TemplateDoc.data to JSON")?,
    )];
    for (key, bytes) in siblings {
        if key.ends_with(".typ") {
            let src = String::from_utf8(bytes.to_vec())
                .with_context(|| format!("template partial '{key}' is not valid UTF-8"))?;
            sources.push((key, src));
        } else {
            files.push((key, bytes.to_vec()));
        }
    }

    // Sibling discovery above may have registered the same font keys as plain
    // binary `files` — harmless duplication; what makes them *fonts* is being
    // listed here, in `doc.fonts` declaration order. Same helper and
    // missing-key semantics (warn + degrade to host/embedded search) as the
    // markdown path's `ResolvedDoc.fonts`.
    let font_refs: Vec<crate::AssetRef> = doc
        .fonts
        .iter()
        .map(|key| crate::AssetRef { key: key.clone() })
        .collect();
    let fonts = super::fonts::collect_fonts(&font_refs, assets).await?;

    // A dropped (unparseable) SVG sibling just vanishes: a template that
    // references it fails on the missing file, as it would have failed on
    // typst parsing the same bytes.
    let outlined = super::svg_outline::outline_svgs(files, fonts, compiler).await?;

    Ok(CompileInputs {
        main: doc.template.clone(),
        sources,
        files: outlined.files,
        fonts: outlined.fonts,
        format,
    })
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
            fonts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn fonts_resolve_into_compile_inputs_in_declaration_order() {
        let assets = sync_provider(|key| {
            Ok(match key {
                "t.typ" => Some(bytes::Bytes::from_static(b"= Hi")),
                "fonts/b.ttf" => Some(bytes::Bytes::from_static(b"BBB")),
                "fonts/a.ttf" => Some(bytes::Bytes::from_static(b"AAA")),
                _ => None,
            })
        });
        let mut d = doc("t.typ", serde_json::json!({}));
        d.fonts = vec!["fonts/b.ttf".to_string(), "fonts/a.ttf".to_string()];

        let inputs = assemble(&d, &assets, CompileFormat::Pdf, &TypstCompiler::Library)
            .await
            .unwrap();

        assert_eq!(inputs.fonts, vec![b"BBB".to_vec(), b"AAA".to_vec()]);
    }

    #[tokio::test]
    async fn missing_font_key_degrades_to_host_search_not_an_error() {
        let assets =
            sync_provider(|key| Ok((key == "t.typ").then(|| bytes::Bytes::from_static(b"= Hi"))));
        let mut d = doc("t.typ", serde_json::json!({}));
        d.fonts = vec!["fonts/gone.ttf".to_string()];

        let inputs = assemble(&d, &assets, CompileFormat::Pdf, &TypstCompiler::Library)
            .await
            .unwrap();

        assert!(inputs.fonts.is_empty(), "missing font warns and is skipped");
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
