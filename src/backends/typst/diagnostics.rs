//! A failed compile's typst diagnostics as a typed error both compiler modes
//! produce, so an embedder (`mdcast-server`'s opt-in `detail`, issue #96) can
//! `downcast_ref` them out of the `anyhow` chain instead of parsing prose.
//!
//! Every entry is `path:line:col: severity: message` — the same shape the
//! `typst` CLI's `--diagnostic-format short` prints, so `Library` and `Cli`
//! report alike. Paths are the compile's *virtual* paths (`main.typ`,
//! `partials/header.typ`); a `Cli` compile's temp directory is stripped, since
//! it says nothing to a template author and everything about the host.

use std::fmt;
use std::path::Path;

use typst::diag::{Severity, SourceDiagnostic};
use typst::syntax::Source;

/// The errors and warnings of one failed compile, already sanitised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileDiagnostics {
    pub diagnostics: Vec<String>,
}

impl fmt::Display for CompileDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "typst compilation errors:")?;
        for d in &self.diagnostics {
            writeln!(f, "  {d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CompileDiagnostics {}

/// `Library` mode: errors first, then the warnings that came with them.
/// `sources` resolves a span to a line/column; a diagnostic whose file is not
/// among them (or that has no span) keeps whatever location it can.
pub(super) fn from_source(
    errors: impl IntoIterator<Item = SourceDiagnostic>,
    warnings: &[SourceDiagnostic],
    sources: &[Source],
) -> anyhow::Error {
    let diagnostics = errors
        .into_iter()
        .collect::<Vec<_>>()
        .iter()
        .chain(warnings)
        .map(|d| describe(d, sources))
        .collect();
    CompileDiagnostics { diagnostics }.into()
}

fn describe(d: &SourceDiagnostic, sources: &[Source]) -> String {
    let severity = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let mut out = match d.span.id() {
        Some(id) => {
            let path = id.vpath().as_rootless_path().display().to_string();
            let position = sources
                .iter()
                .find(|s| s.id() == id)
                .and_then(|s| {
                    let start = s.range(d.span)?.start;
                    s.lines().byte_to_line_column(start)
                })
                // Line 1-based, column 0-based: what the typst CLI prints.
                .map(|(line, col)| format!(":{}:{col}", line + 1))
                .unwrap_or_default();
            format!("{path}{position}: {severity}: {}", d.message)
        }
        None => format!("{severity}: {}", d.message),
    };
    for hint in &d.hints {
        out.push_str(&format!(" (hint: {hint})"));
    }
    out
}

/// `Cli` mode: picks the diagnostic lines out of a `--diagnostic-format
/// short` stderr — anything else there (package download progress, a
/// panic) is not a diagnostic — and strips `root` from each. `None` when
/// there are none, i.e. the child failed for some other reason.
pub(super) fn from_cli_stderr(stderr: &str, root: &Path) -> Option<CompileDiagnostics> {
    let mut roots = vec![root.to_path_buf()];
    if let Ok(canonical) = root.canonicalize()
        && canonical != root
    {
        roots.push(canonical);
    }
    let diagnostics: Vec<String> = stderr
        .lines()
        .filter(|l| is_diagnostic(l))
        .map(|l| strip_roots(l, &roots))
        .collect();
    (!diagnostics.is_empty()).then_some(CompileDiagnostics { diagnostics })
}

fn is_diagnostic(line: &str) -> bool {
    ["error: ", "warning: "]
        .iter()
        .any(|tag| line.starts_with(tag) || line.contains(&format!(": {tag}")))
}

fn strip_roots(line: &str, roots: &[std::path::PathBuf]) -> String {
    roots.iter().fold(line.to_string(), |acc, root| {
        let root = root.to_string_lossy();
        acc.replace(&format!("{root}/"), "")
            .replace(root.as_ref(), "")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use typst::syntax::{FileId, VirtualPath};

    #[test]
    fn cli_stderr_keeps_diagnostics_only_and_strips_the_temp_dir() {
        let root = Path::new("/tmp/mdcast-typst-abc123");
        let stderr = "downloading @preview/cetz:0.3.0\n\
                      main.typ:1:9: error: unclosed delimiter\n\
                      main.typ:2:1: error: file not found (searched at /tmp/mdcast-typst-abc123/img/x.png)\n\
                      warning: unknown font family: nope\n";

        let diags = from_cli_stderr(stderr, root).unwrap();

        assert_eq!(
            diags.diagnostics,
            [
                "main.typ:1:9: error: unclosed delimiter",
                "main.typ:2:1: error: file not found (searched at img/x.png)",
                "warning: unknown font family: nope",
            ]
        );
    }

    #[test]
    fn cli_stderr_without_diagnostics_is_none() {
        assert_eq!(
            from_cli_stderr("thread 'main' panicked\n", Path::new("/tmp/x")),
            None
        );
    }

    #[test]
    fn library_diagnostics_carry_virtual_path_line_and_column() {
        let id = FileId::new(None, VirtualPath::new("partials/head.typ"));
        let source = Source::new(id, "= Hi\n#let x = (\n".into());
        // A real span needs a parsed source: take the parser's own error.
        let parse_error = source
            .root()
            .errors()
            .into_iter()
            .next()
            .expect("unclosed delimiter is a parse error");
        let error =
            SourceDiagnostic::error(parse_error.span, parse_error.message).with_hint("close it");
        let warning = SourceDiagnostic::warning(parse_error.span, "spanned warning");

        let err = from_source([error], &[warning], &[source]);
        let diags = err.downcast_ref::<CompileDiagnostics>().unwrap();

        assert_eq!(
            diags.diagnostics,
            [
                "partials/head.typ:2:9: error: unclosed delimiter (hint: close it)",
                "partials/head.typ:2:9: warning: spanned warning",
            ]
        );
    }
}
