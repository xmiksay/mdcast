//! Typst backend. Uses the `typst` compiler crate via `typst-as-lib`'s
//! templating wrapper by default — no subprocess, no `typst` binary on PATH,
//! no temp-dir gymnastics for the typst source — or, when `TypstCompiler::Cli`
//! is selected (issue #73), shells out to a real `typst compile` in a temp
//! dir. `render_to_bytes` doesn't know which: it always ends by handing a
//! `compiler::CompileInputs` to `self.compiler.compile(...)`.
//!
//! Per-page layout templates are pulled from the `AssetProvider` and folded
//! into `CompileInputs.sources`; the driver `main.typ` is built in memory and
//! points at each layout via `#import`. In `Library` mode compilation runs on
//! a blocking thread (`spawn_blocking`) so the executor stays responsive; in
//! `Cli` mode it's a child process instead, which is what makes it killable
//! (see `compiler/cli.rs`).
//!
//! Three kinds of files reach the compiler: the per-class layout sources, the
//! synthetic `/context.typ`, and virtual files resolved by `virtual_files` —
//! images found by scanning page bodies plus `ResolvedDoc.assets`, the latter
//! for chrome a layout owns directly (a logo, a background) rather than
//! something referenced from markdown.

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::future::try_join_all;
use futures::try_join;

use crate::assets::{AssetProvider, BoxFuture};
use crate::pages::Page;
use crate::{Backend, RenderedArtifact, ResolvedDoc, Target};

mod compiler;
mod context;
mod diagnostics;
mod fonts;
mod markdown;
mod svg_outline;
mod template;
#[cfg(test)]
mod test_support;
mod virtual_files;
pub use compiler::{
    CliCompiler, DEFAULT_MAX_OUTPUT_BYTES, MIN_TYPST_CLI_VERSION, OutlineCommand, TypstCompiler,
};
use compiler::{CompileFormat, CompileInputs};
use context::{CONTEXT_VIRTUAL_PATH, build_context_source};
pub use diagnostics::CompileDiagnostics;
use fonts::collect_fonts;
pub use markdown::md_to_typst;
use markdown::typst_string;
use svg_outline::outline_svgs;
#[doc(hidden)]
pub use svg_outline::run_child as run_outline_child;
#[cfg(feature = "typst-html")]
pub use template::render_template_html;
pub use template::{TemplateDoc, render_template, render_template_html_with, render_template_with};
use virtual_files::{collect_images_for_typst, collect_layout_assets};

/// The argument that puts the `mdcast`/`mdcast-server` binary into its
/// hidden SVG-outline child mode ([`run_outline_child`] on stdin/stdout) —
/// what [`OutlineCommand::current_exe`] passes.
#[doc(hidden)]
pub const OUTLINE_CHILD_ARG: &str = "__outline-svg";

/// The SVG-outline step on its own, dispatched exactly as a render
/// dispatches it for `compiler` — the seam integration tests drive a real
/// outline child through. Returns the files and the paths dropped.
#[doc(hidden)]
pub async fn outline_svg_files(
    compiler: &TypstCompiler,
    files: Vec<(String, Vec<u8>)>,
    fonts: Vec<Vec<u8>>,
) -> Result<(Vec<(String, Vec<u8>)>, std::collections::HashSet<String>)> {
    let outlined = outline_svgs(files, fonts, compiler).await?;
    Ok((outlined.files, outlined.dropped))
}

pub struct TypstBackend {
    target: Target,
    compiler: TypstCompiler,
}

impl TypstBackend {
    pub fn new(target: Target) -> Self {
        Self::with_compiler(target, TypstCompiler::default())
    }

    pub fn with_compiler(target: Target, compiler: TypstCompiler) -> Self {
        assert!(matches!(target, Target::Pdf | Target::PdfPresentation));
        Self { target, compiler }
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
            let ((mut image_map, mut virtual_files), (mut asset_map, asset_files), fonts) = try_join!(
                collect_images_for_typst(&doc.pages, assets, self.target),
                collect_layout_assets(&doc.assets, assets),
                collect_fonts(&doc.fonts, assets),
            )?;
            virtual_files.extend(asset_files);

            // Before the maps feed `md_to_typst`/`/context.typ`: an SVG that
            // failed to parse then degrades like an unresolved image (body
            // image dropped, `asset-path` to its default) instead of a
            // missing-file compile error (issue #92).
            let outlined = outline_svgs(virtual_files, fonts, &self.compiler).await?;
            let kept = |vpath: &String| !outlined.dropped.contains(vpath.trim_start_matches('/'));
            image_map.retain(|_, vpath| kept(vpath));
            asset_map.retain(|_, vpath| kept(vpath));

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

            let mut sources: Vec<(String, String)> = Vec::with_capacity(layouts.len() + 2);
            sources.push((CONTEXT_VIRTUAL_PATH.to_string(), context_source));
            sources.push(("main.typ".to_string(), driver));
            for (class, bytes) in layouts {
                let path = format!("layouts/{}.typ", sanitize_class(&class));
                let src = String::from_utf8(bytes)
                    .with_context(|| format!("layout '{class}' is not valid UTF-8"))?;
                sources.push((path, src));
            }

            let pdf_bytes = self
                .compiler
                .compile(CompileInputs {
                    main: "main.typ".to_string(),
                    sources,
                    files: outlined.files,
                    fonts: outlined.fonts,
                    format: CompileFormat::Pdf,
                })
                .await?;

            Ok(RenderedArtifact {
                primary: Bytes::from(pdf_bytes),
                filename: format!("output.{}", self.target.extension()),
                extras: vec![],
            })
        })
    }
}

/// Run a sync typst compile on a blocking thread — used by
/// `TypstCompiler::Library` only (`Cli` runs a child process instead, which
/// needs no blocking thread of its own). typst's compiler is sync and
/// CPU-bound; everything is produced in memory — no temp file, nothing to
/// clean up. `spawn_blocking` runs on its own OS thread and does not inherit
/// the calling task's `tracing` dispatcher, so it's captured here and
/// re-installed inside the closure — otherwise the compile's warnings would
/// only surface under a process-wide `set_global_default` subscriber, never a
/// task-scoped one.
async fn spawn_compile<T, F>(compile: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let dispatch = tracing::dispatcher::get_default(|d| d.clone());
    tokio::task::spawn_blocking(move || tracing::dispatcher::with_default(&dispatch, compile))
        .await
        .context("typst compile thread panicked")?
}

/// Typst warnings (e.g. "unknown font family: ...") arrive on a separate
/// channel from the compile error path — surface them via `tracing` so a
/// brand font that failed to resolve isn't silently dropped. Shared by every
/// compile site (markdown pdf, template pdf, template html).
fn forward_warnings(warnings: &[typst::diag::SourceDiagnostic]) {
    for w in warnings {
        tracing::warn!("typst: {}", w.message);
        for hint in &w.hints {
            tracing::warn!("typst: hint: {hint}");
        }
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
