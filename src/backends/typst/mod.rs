//! Typst backend, in-process. Uses the `typst` compiler crate via
//! `typst-as-lib`'s templating wrapper — no subprocess, no `typst` binary on
//! PATH, no temp-dir gymnastics for the typst source.
//!
//! Per-page layout templates are pulled from the `AssetProvider` and registered
//! as static sources on the engine; the driver `main.typ` is built in memory
//! and points at each layout via `#import`. Compilation runs on a blocking
//! thread (`spawn_blocking`) so the executor stays responsive.
//!
//! Three kinds of files reach the engine: the per-class layout sources, the
//! synthetic `/context.typ`, and virtual files resolved by `virtual_files` —
//! images found by scanning page bodies plus `ResolvedDoc.assets`, the latter
//! for chrome a layout owns directly (a logo, a background) rather than
//! something referenced from markdown.

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::future::try_join_all;
use futures::try_join;
use typst::syntax::{FileId, Source, VirtualPath};
use typst_as_lib::TypstEngine;

use crate::assets::{AssetProvider, BoxFuture};
use crate::pages::Page;
use crate::{Backend, RenderedArtifact, ResolvedDoc, Target};

mod context;
mod fonts;
mod markdown;
mod virtual_files;
use context::{CONTEXT_VIRTUAL_PATH, build_context_source};
use fonts::collect_fonts;
pub use markdown::md_to_typst;
use markdown::typst_string;
use virtual_files::{collect_images_for_typst, collect_layout_assets};

pub struct TypstBackend {
    target: Target,
}

impl TypstBackend {
    pub fn new(target: Target) -> Self {
        assert!(matches!(target, Target::Pdf | Target::PdfPresentation));
        Self { target }
    }

    /// Returns `(requested_class, bytes)` — the bytes may be from the fallback
    /// layout, but the requested class is preserved so the driver's import path
    /// (`layouts/<class>.typ`) matches what the engine has registered.
    async fn fetch_layout(
        assets: &dyn AssetProvider,
        target_dir: &str,
        class: &str,
        fallback: &str,
    ) -> Result<(String, Vec<u8>)> {
        let key = format!("typst/layouts/{target_dir}/{class}.typ");
        if let Some(b) = assets.get(&key).await? {
            return Ok((class.to_string(), b.to_vec()));
        }
        tracing::warn!(
            class,
            target_dir,
            "typst layout not found; falling back to {fallback}"
        );
        let fallback_key = format!("typst/layouts/{target_dir}/{fallback}.typ");
        let Some(b) = assets.get(&fallback_key).await? else {
            bail!(
                "fallback typst layout '{fallback}' for target '{target_dir}' missing from asset provider"
            );
        };
        Ok((class.to_string(), b.to_vec()))
    }
}

impl Backend for TypstBackend {
    fn target(&self) -> Target {
        self.target
    }

    fn render_to_bytes<'a>(
        &'a self,
        doc: &'a ResolvedDoc,
        assets: &'a dyn AssetProvider,
    ) -> BoxFuture<'a, Result<RenderedArtifact>> {
        Box::pin(async move {
            // Resolve all layouts in parallel (deduped by class).
            let tdir = target_dir(self.target);
            let mut classes: Vec<&str> = doc.pages.iter().map(|p| p.class.as_str()).collect();
            classes.sort();
            classes.dedup();
            let layouts: Vec<(String, Vec<u8>)> = try_join_all(
                classes
                    .iter()
                    .map(|c| Self::fetch_layout(assets, tdir, c, "content")),
            )
            .await?;

            // Resolve page-body image refs and declared layout assets (logos,
            // backgrounds — chrome the layout owns, not the page body)
            // concurrently — both are independent provider fetches. Missing
            // layout-asset keys warn and are simply absent from `asset_map`,
            // so a layout's `asset-path(key)` degrades to its `default:`
            // instead of failing the whole compile.
            let ((image_map, mut virtual_files), (asset_map, asset_files), fonts) = try_join!(
                collect_images_for_typst(&doc.pages, assets),
                collect_layout_assets(&doc.assets, assets),
                collect_fonts(&doc.fonts, assets),
            )?;
            virtual_files.extend(asset_files);

            // Convert each page body from markdown → typst markup.
            let typst_bodies: Vec<String> = doc
                .pages
                .iter()
                .map(|p| md_to_typst(&p.body, &image_map))
                .collect();

            // TOC is a document-level concept — only the `pdf` target (not
            // `pdf-presentation`) renders one, even if requested.
            let toc_depth = match self.target {
                Target::Pdf => doc.toc,
                _ => None,
            };

            // Build the driver source and the doc-meta/brand/assets context
            // that layouts can optionally `#import "/context.typ": ...`.
            let driver = build_driver(&doc.pages, &typst_bodies, toc_depth);
            let context_source = build_context_source(&doc.meta, &doc.brand.0, &asset_map);

            // Compile on a blocking thread — typst's compiler is sync and
            // CPU-bound. Produced entirely in memory: no temp file, nothing
            // to clean up. `spawn_blocking` runs on its own OS thread and
            // does not inherit the calling task's `tracing` dispatcher, so
            // it's captured here and re-installed inside the closure —
            // otherwise `compile_pdf`'s warnings would only surface under a
            // process-wide `set_global_default` subscriber, never a
            // task-scoped one.
            let dispatch = tracing::dispatcher::get_default(|d| d.clone());
            let pdf_bytes = tokio::task::spawn_blocking(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    compile_pdf(driver, context_source, layouts, virtual_files, fonts)
                })
            })
            .await
            .context("typst compile thread panicked")??;

            Ok(RenderedArtifact {
                primary: Bytes::from(pdf_bytes),
                filename: format!("output.{}", self.target.extension()),
                extras: vec![],
            })
        })
    }
}

fn target_dir(target: Target) -> &'static str {
    match target {
        Target::Pdf => "pdf",
        Target::PdfPresentation => "pdf-presentation",
        _ => unreachable!("typst backend only handles pdf and pdf-presentation"),
    }
}

pub fn build_driver(pages: &[Page], typst_bodies: &[String], toc_depth: Option<u8>) -> String {
    let mut s = String::new();

    // Import every used layout once under a sanitized alias.
    let mut classes: Vec<&str> = pages.iter().map(|p| p.class.as_str()).collect();
    classes.sort();
    classes.dedup();
    for class in &classes {
        s.push_str(&format!(
            "#import \"layouts/{}.typ\": layout as {}\n",
            sanitize_class(class),
            alias_for(class),
        ));
    }
    s.push('\n');

    // Rendered as its own page ahead of the document body — a bare
    // `#outline()` call would otherwise share the first page's flow (and
    // that page's own `#set page(...)`) since none of the per-class layouts
    // are wrapped in anything that starts a fresh page on their own.
    if let Some(depth) = toc_depth {
        s.push_str(&format!("#outline(depth: {depth})\n#pagebreak()\n\n"));
    }

    // One call per page. The body is a typst-markup string; the layout calls
    // `eval(body, mode: "markup")` to actually parse and lay it out.
    for (page, body) in pages.iter().zip(typst_bodies) {
        let alias = alias_for(&page.class);
        let escaped = typst_string(body);
        s.push_str(&format!("#{alias}({escaped})\n"));
    }
    s
}

fn alias_for(class: &str) -> String {
    let mut out = String::from("layout_");
    for c in class.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c)
        } else {
            out.push('_')
        }
    }
    out
}

/// Flatten a page/layout class name into a safe typst import path segment.
/// Distinct from `images::sanitize_key` (image provider keys) — different
/// input domain, kept separate deliberately rather than reused.
fn sanitize_class(class: &str) -> String {
    class.replace(['/', '\\'], "_")
}

/// Synchronous: builds the engine, compiles, exports to PDF bytes.
fn compile_pdf(
    driver: String,
    context_source: String,
    layouts: Vec<(String, Vec<u8>)>,
    virtual_files: Vec<(String, Vec<u8>)>,
    fonts: Vec<Vec<u8>>,
) -> Result<Vec<u8>> {
    // Pre-build owned `Source` values so we don't fight lifetimes against the
    // `(&str, String)` IntoSource impl that requires the path to outlive the iterator.
    let mut sources: Vec<Source> = Vec::with_capacity(layouts.len() + 1);
    let context_id = FileId::new(None, VirtualPath::new(CONTEXT_VIRTUAL_PATH));
    sources.push(Source::new(context_id, context_source));
    for (class, bytes) in layouts {
        let path = format!("layouts/{}.typ", sanitize_class(&class));
        let src = String::from_utf8(bytes)
            .with_context(|| format!("layout '{class}' is not valid UTF-8"))?;
        let id = FileId::new(None, VirtualPath::new(path));
        sources.push(Source::new(id, src));
    }

    // Materialise image/asset bytes for the engine. Keys (virtual paths) and
    // bytes are stored in `virtual_files`; we build (&str, Vec<u8>) tuples by
    // leaking the path strings — they live for the duration of the compile,
    // which is fine because the engine itself is dropped at the end of this
    // function.
    let image_refs: Vec<(&str, Vec<u8>)> = virtual_files
        .iter()
        .map(|(p, b)| (p.as_str(), b.clone()))
        .collect();

    // Registered before `search_fonts_with`: `TypstEngine::builder().build()`
    // pushes explicit `.fonts(...)` faces into the font book first, so an
    // exact family match resolves to the provider-supplied font even when
    // the same family is also discoverable on the host (FontBook::select's
    // tie-break keeps the first-inserted candidate).
    let engine = TypstEngine::builder()
        .main_file(driver)
        .with_static_source_file_resolver(sources)
        .with_static_file_resolver(image_refs)
        .fonts(fonts)
        .search_fonts_with(typst_as_lib::typst_kit_options::TypstKitFontOptions::default())
        .build();

    let result = engine.compile();
    // Typst warnings (e.g. "unknown font family: ...") arrive on a separate
    // channel from the compile error path — surface them via `tracing` so a
    // brand font that failed to resolve isn't silently dropped.
    for w in &result.warnings {
        tracing::warn!("typst: {}", w.message);
        for hint in &w.hints {
            tracing::warn!("typst: hint: {hint}");
        }
    }
    let doc = result.output.map_err(format_lib_error)?;

    let options = typst_pdf::PdfOptions::default();
    let pdf = typst_pdf::pdf(&doc, &options).map_err(format_diagnostics)?;
    Ok(pdf)
}

fn format_lib_error(err: typst_as_lib::TypstAsLibError) -> anyhow::Error {
    use typst_as_lib::TypstAsLibError as E;
    match err {
        E::TypstSource(diags) => format_diagnostics(diags),
        other => anyhow::anyhow!(other.to_string()),
    }
}

fn format_diagnostics(
    diags: impl IntoIterator<Item = typst::diag::SourceDiagnostic>,
) -> anyhow::Error {
    let mut out = String::from("typst compilation errors:\n");
    for e in diags {
        let sev = match e.severity {
            typst::diag::Severity::Error => "error",
            typst::diag::Severity::Warning => "warning",
        };
        out.push_str(&format!("  [{sev}] {}\n", e.message));
        for hint in &e.hints {
            out.push_str(&format!("    hint: {hint}\n"));
        }
    }
    anyhow::anyhow!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::PageOrigin;

    fn p(class: &str, body: &str) -> Page {
        Page {
            class: class.into(),
            body: body.into(),
            origin: PageOrigin::Explicit,
        }
    }

    #[test]
    fn no_toc_request_omits_outline() {
        let pages = vec![p("content", "# Hi")];
        let bodies = vec!["= Hi".to_string()];
        let driver = build_driver(&pages, &bodies, None);
        assert!(!driver.contains("#outline"));
    }

    #[test]
    fn toc_request_emits_outline_with_requested_depth_before_pages() {
        let pages = vec![p("content", "# Hi")];
        let bodies = vec!["= Hi".to_string()];
        let driver = build_driver(&pages, &bodies, Some(2));
        assert!(driver.contains("#outline(depth: 2)"), "{driver}");
        assert!(driver.contains("#pagebreak()"), "{driver}");
        let outline_pos = driver.find("#outline").unwrap();
        let page_call_pos = driver.find("#layout_content").unwrap();
        assert!(
            outline_pos < page_call_pos,
            "outline must precede the first page's layout call:\n{driver}"
        );
    }
}
