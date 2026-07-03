//! Typst backend, in-process. Uses the `typst` compiler crate via
//! `typst-as-lib`'s templating wrapper — no subprocess, no `typst` binary on
//! PATH, no temp-dir gymnastics for the typst source.
//!
//! Per-page layout templates are pulled from the `AssetProvider` and registered
//! as static sources on the engine; the driver `main.typ` is built in memory
//! and points at each layout via `#import`. Compilation runs on a blocking
//! thread (`spawn_blocking`) so the executor stays responsive.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::future::try_join_all;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use regex::Regex;
use typst::syntax::{FileId, Source, VirtualPath};
use typst_as_lib::TypstEngine;

use crate::assets::{AssetProvider, BoxFuture};
use crate::pages::Page;
use crate::{Backend, RenderedArtifact, ResolvedDoc, Target};

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
        tracing::warn!(class, target_dir, "typst layout not found; falling back to {fallback}");
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
                classes.iter().map(|c| Self::fetch_layout(assets, tdir, c, "content")),
            )
            .await?;

            // Resolve image refs through the provider. Returns the bytes to
            // register with the typst engine and a (url → virtual-path) map for
            // md→typst conversion.
            let (image_map, image_files) = collect_images_for_typst(&doc.pages, assets).await?;

            // Convert each page body from markdown → typst markup.
            let typst_bodies: Vec<String> =
                doc.pages.iter().map(|p| md_to_typst(&p.body, &image_map)).collect();

            // Build the driver source.
            let driver = build_driver(&doc.pages, &typst_bodies);

            // Compile on a blocking thread — typst's compiler is sync and
            // CPU-bound. Produced entirely in memory: no temp file, nothing
            // to clean up.
            let pdf_bytes =
                tokio::task::spawn_blocking(move || compile_pdf(driver, layouts, image_files))
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

/// Walk every page, find `![alt](key)` image references, fetch bytes via the
/// provider, and produce (a) a `url → virtual_path` map for the md→typst
/// converter, (b) the bytes to register with the typst engine.
async fn collect_images_for_typst(
    pages: &[Page],
    provider: &dyn AssetProvider,
) -> Result<(BTreeMap<String, String>, Vec<(String, Vec<u8>)>)> {
    let re = Regex::new(r"!\[[^\]]*\]\((?P<url>[^)\s]+)\)").unwrap();
    let mut urls: BTreeMap<String, ()> = BTreeMap::new();
    for page in pages {
        for cap in re.captures_iter(&page.body) {
            let url = cap.name("url").unwrap().as_str();
            if is_remote(url) {
                continue;
            }
            urls.insert(url.to_string(), ());
        }
    }
    let url_vec: Vec<String> = urls.into_keys().collect();
    let fetched: Vec<(String, Option<Vec<u8>>)> = try_join_all(url_vec.iter().map(|u| async move {
        let bytes = provider.get(u).await?.map(|b| b.to_vec());
        Ok::<_, anyhow::Error>((u.clone(), bytes))
    }))
    .await?;

    let mut map = BTreeMap::new();
    let mut files = Vec::new();
    for (url, bytes) in fetched {
        if let Some(bytes) = bytes {
            // Register under `images/...` but emit `/images/...` in #image() calls
            // — the leading slash makes typst resolve relative to the project root
            // instead of the layout file's directory.
            let vpath = format!("images/{}", sanitize_path(&url));
            map.insert(url, format!("/{vpath}"));
            files.push((vpath, bytes));
        }
    }
    Ok((map, files))
}

fn is_remote(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with("file://")
}

fn target_dir(target: Target) -> &'static str {
    match target {
        Target::Pdf => "pdf",
        Target::PdfPresentation => "pdf-presentation",
        _ => unreachable!("typst backend only handles pdf and pdf-presentation"),
    }
}

fn build_driver(pages: &[Page], typst_bodies: &[String]) -> String {
    let mut s = String::new();

    // Import every used layout once under a sanitized alias.
    let mut classes: Vec<&str> = pages.iter().map(|p| p.class.as_str()).collect();
    classes.sort();
    classes.dedup();
    for class in &classes {
        s.push_str(&format!(
            "#import \"layouts/{}.typ\": layout as {}\n",
            sanitize_path(class),
            alias_for(class),
        ));
    }
    s.push('\n');

    // One call per page. The body is a typst-markup string; the layout calls
    // `eval(body, mode: "markup")` to actually parse and lay it out.
    for (page, body) in pages.iter().zip(typst_bodies) {
        let alias = alias_for(&page.class);
        let escaped = typst_string(body);
        s.push_str(&format!("#{alias}({escaped})\n"));
    }
    s
}

/// Convert markdown to a Typst-markup string suitable for `eval(.., mode: "markup")`.
/// Image refs use the `images` map produced by `collect_images_for_typst`. Anything
/// the converter doesn't know about (HTML blocks, footnotes, …) is dropped — v1
/// scope, expanded as concrete fixtures demand it.
fn md_to_typst(md: &str, images: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut in_image = 0i32;
    let parser = Parser::new(md);

    for ev in parser {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                out.push_str(&"=".repeat(heading_depth(level)));
                out.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("\n\n"),

            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),

            Event::Start(Tag::Emphasis) => out.push('_'),
            Event::End(TagEnd::Emphasis) => out.push('_'),
            Event::Start(Tag::Strong) => out.push('*'),
            Event::End(TagEnd::Strong) => out.push('*'),

            Event::Start(Tag::List(_)) => {}
            Event::End(TagEnd::List(_)) => out.push('\n'),
            Event::Start(Tag::Item) => out.push_str("- "),
            Event::End(TagEnd::Item) => out.push('\n'),

            Event::Start(Tag::BlockQuote(_)) => out.push_str("#quote(block: true)[\n"),
            Event::End(TagEnd::BlockQuote(_)) => out.push_str("\n]\n\n"),

            Event::Start(Tag::Image { dest_url, .. }) => {
                in_image += 1;
                match images.get(dest_url.as_ref()) {
                    Some(vpath) => {
                        out.push_str(&format!("#image({})", typst_string(vpath)));
                    }
                    None => {
                        out.push_str(&format!("[image unresolved: {dest_url}]"));
                    }
                }
            }
            Event::End(TagEnd::Image) => {
                in_image -= 1;
            }

            Event::Start(Tag::CodeBlock(_)) => out.push_str("```\n"),
            Event::End(TagEnd::CodeBlock) => out.push_str("```\n\n"),

            Event::Text(t) => {
                if in_image == 0 {
                    out.push_str(&escape_typst_inline(&t));
                }
            }
            Event::Code(c) => {
                out.push('`');
                out.push_str(&c);
                out.push('`');
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push_str("\\\n"),

            _ => {}
        }
    }
    out
}

fn heading_depth(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Escape characters that have special meaning in Typst markup. We do *not*
/// escape `_`, `*`, or `` ` `` because those are emitted intentionally by the
/// converter; markdown text containing literal `_` / `*` in inline contexts is
/// a known v1 limitation.
fn escape_typst_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '#' | '@' | '<' | '>' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn alias_for(class: &str) -> String {
    let mut out = String::from("layout_");
    for c in class.chars() {
        if c.is_ascii_alphanumeric() { out.push(c) } else { out.push('_') }
    }
    out
}

fn sanitize_path(class: &str) -> String {
    class.replace(['/', '\\'], "_")
}

fn typst_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Synchronous: builds the engine, compiles, exports to PDF bytes.
fn compile_pdf(
    driver: String,
    layouts: Vec<(String, Vec<u8>)>,
    image_files: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>> {
    // Pre-build owned `Source` values so we don't fight lifetimes against the
    // `(&str, String)` IntoSource impl that requires the path to outlive the iterator.
    let mut sources: Vec<Source> = Vec::with_capacity(layouts.len());
    for (class, bytes) in layouts {
        let path = format!("layouts/{}.typ", sanitize_path(&class));
        let src = String::from_utf8(bytes)
            .with_context(|| format!("layout '{class}' is not valid UTF-8"))?;
        let id = FileId::new(None, VirtualPath::new(path));
        sources.push(Source::new(id, src));
    }

    // Materialise image bytes for the engine. Keys (virtual paths) and bytes
    // are stored in `image_files`; we build (&str, Vec<u8>) tuples by leaking
    // the path strings — they live for the duration of the compile, which is
    // fine because the engine itself is dropped at the end of this function.
    let image_refs: Vec<(&str, Vec<u8>)> = image_files
        .iter()
        .map(|(p, b)| (p.as_str(), b.clone()))
        .collect();

    let engine = TypstEngine::builder()
        .main_file(driver)
        .with_static_source_file_resolver(sources)
        .with_static_file_resolver(image_refs)
        .search_fonts_with(typst_as_lib::typst_kit_options::TypstKitFontOptions::default())
        .build();

    let result = engine.compile();
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

fn format_diagnostics(diags: impl IntoIterator<Item = typst::diag::SourceDiagnostic>) -> anyhow::Error {
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
