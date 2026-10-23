//! `TypstCompiler::Library`: the in-process compile, unchanged in substance
//! from before issue #73 — only reshaped to read from [`CompileInputs`]
//! instead of the markdown and template paths each building their own
//! `TypstEngine` call. Synchronous; the caller runs this on a blocking
//! thread (`super::spawn_compile`).

use anyhow::Result;
use typst::syntax::{FileId, Source, VirtualPath};
use typst_as_lib::TypstEngine;

use super::{CompileFormat, CompileInputs};
use crate::backends::typst::diagnostics;
use crate::backends::typst::forward_warnings;

pub(super) fn compile(inputs: CompileInputs) -> Result<Vec<u8>> {
    let mut sources: Vec<Source> = Vec::with_capacity(inputs.sources.len());
    for (path, content) in inputs.sources {
        let id = FileId::new(None, VirtualPath::new(&path));
        sources.push(Source::new(id, content));
    }
    // Kept for span → line/column in a failure's diagnostics; `Source` is a
    // cheap `Arc` clone.
    let lookup = sources.clone();
    let file_refs: Vec<(&str, Vec<u8>)> = inputs
        .files
        .iter()
        .map(|(p, b)| (p.as_str(), b.clone()))
        .collect();

    // Registered before `search_fonts_with`: `TypstEngine::builder().build()`
    // pushes explicit `.fonts(...)` faces into the font book first, so an
    // exact family match resolves to the provider-supplied font even when
    // the same family is also discoverable on the host (FontBook::select's
    // tie-break keeps the first-inserted candidate).
    let engine = TypstEngine::builder()
        .with_static_source_file_resolver(sources)
        .with_static_file_resolver(file_refs)
        .fonts(inputs.fonts)
        .search_fonts_with(typst_as_lib::typst_kit_options::TypstKitFontOptions::default())
        .build();

    match inputs.format {
        CompileFormat::Pdf => {
            let result = engine.compile(inputs.main.as_str());
            forward_warnings(&result.warnings);
            let doc = result
                .output
                .map_err(|e| lib_error(e, &result.warnings, &lookup))?;
            let options = typst_pdf::PdfOptions::default();
            typst_pdf::pdf(&doc, &options)
                .map_err(|e| diagnostics::from_source(e, &result.warnings, &lookup))
        }
        CompileFormat::Html => compile_html(&engine, &inputs.main, &lookup),
    }
}

#[cfg(feature = "typst-html")]
fn compile_html(
    engine: &TypstEngine<typst_as_lib::TypstTemplateCollection>,
    main: &str,
    lookup: &[Source],
) -> Result<Vec<u8>> {
    let result: typst::diag::Warned<Result<typst_html::HtmlDocument, _>> = engine.compile(main);
    forward_warnings(&result.warnings);
    let doc = result
        .output
        .map_err(|e| lib_error(e, &result.warnings, lookup))?;
    let html = typst_html::html(&doc)
        .map_err(|e| diagnostics::from_source(e, &result.warnings, lookup))?;
    Ok(html.into_bytes())
}

/// HTML export needs `typst-as-lib`'s own `typst-html` feature — a
/// process-wide compile-time toggle (issue #53's module docs explain why),
/// so a build without it cannot produce an `HtmlDocument` at all. `Cli` mode
/// has no such restriction (see `cli::compile`); this is `Library`-only.
#[cfg(not(feature = "typst-html"))]
fn compile_html(
    _engine: &TypstEngine<typst_as_lib::TypstTemplateCollection>,
    _main: &str,
    _lookup: &[Source],
) -> Result<Vec<u8>> {
    anyhow::bail!(
        "HTML export via the in-process Typst compiler requires mdcast's `typst-html` build \
         feature; select the `cli` compiler instead to export HTML without it"
    )
}

/// Only a source-level failure has diagnostics to report; anything else
/// (`typst-as-lib` itself failing to find the main file, say) stays prose.
fn lib_error(
    err: typst_as_lib::TypstAsLibError,
    warnings: &[typst::diag::SourceDiagnostic],
    lookup: &[Source],
) -> anyhow::Error {
    match err {
        typst_as_lib::TypstAsLibError::TypstSource(errors) => {
            diagnostics::from_source(errors, warnings, lookup)
        }
        other => anyhow::anyhow!(other.to_string()),
    }
}
