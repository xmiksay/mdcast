//! Compiler seam (issue #73): a Typst compile runs either in-process
//! (`TypstCompiler::Library`, the default — today's behaviour, unchanged) or
//! by shelling out to a `typst` binary (`TypstCompiler::Cli`). Both the
//! markdown path (`super::render_to_bytes`) and the template path
//! (`template.rs::assemble`) reduce their inputs to the same
//! [`CompileInputs`] — a flat, already-resolved virtual filesystem (typst
//! sources, binary files, font bytes, the id of the file to compile, and the
//! export format) — and hand it to whichever mode is configured. Neither
//! compiler implementation needs to know how its inputs were produced.
//!
//! `Cli` is what unlocks a killable, memory-boundable compile:
//! `tokio::task::spawn_blocking` (what `Library` runs on) cannot be
//! cancelled, so a caller-supplied template that never terminates wedges a
//! thread for the life of the process — see the crate's "Known limitations".
//! A child process, by contrast, can be killed on timeout and constrained
//! with `setrlimit`. It also sidesteps `typst-html` being a process-wide
//! compile-time toggle (issue #53): a real `typst` binary either supports
//! `--features html` or it does not, decided per invocation rather than per
//! build — see `cli::compile`.

use anyhow::Result;

mod cli;
mod library;

pub use cli::{CliCompiler, OutlineCommand};

/// Everything a compile needs, already resolved to plain data — no
/// `AssetProvider`, no `ResolvedDoc`. Paths are relative, `/`-separated, and
/// carry no leading slash; a Typst `#import "/foo.typ"` (root-relative) and
/// `#import "foo.typ"` (relative to the importing file) both resolve
/// correctly once these are either registered as static sources at that path
/// (`library`) or written to that path under a common root (`cli`).
pub(super) struct CompileInputs {
    /// Path of the file to compile — e.g. `"main.typ"` for the markdown
    /// driver, or the template's own `AssetProvider` key (e.g.
    /// `"templates/invoice.typ"`).
    pub main: String,
    /// Every `.typ` source, main included: path -> UTF-8 content.
    pub sources: Vec<(String, String)>,
    /// Every other file a source references: images, layout assets,
    /// `/data.json`.
    pub files: Vec<(String, Vec<u8>)>,
    /// Font face bytes to make available to the compile, in declaration
    /// order — `library::compile` relies on that order for tie-breaking
    /// (see `fonts.rs`'s module docs).
    pub fonts: Vec<Vec<u8>>,
    pub format: CompileFormat,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CompileFormat {
    Pdf,
    Html,
}

/// Which Typst compiler a render uses. `Library` is the default and is
/// exactly today's behaviour — selecting `Cli` is opt-in (issue #73).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TypstCompiler {
    #[default]
    Library,
    Cli(CliCompiler),
}

impl TypstCompiler {
    pub(super) async fn compile(&self, inputs: CompileInputs) -> Result<Vec<u8>> {
        match self {
            TypstCompiler::Library => super::spawn_compile(move || library::compile(inputs)).await,
            TypstCompiler::Cli(cli) => cli.compile(inputs).await,
        }
    }

    /// Best-effort: logs the `typst` binary's reported version so version
    /// skew between it and the linked `typst` crate is visible up front
    /// instead of discovered as a rendering difference (issue #73 notes this
    /// is now an operational concern). A no-op for `Library`.
    pub async fn log_startup_info(&self) {
        if let TypstCompiler::Cli(cli) = self {
            cli.log_version().await;
        }
    }
}
