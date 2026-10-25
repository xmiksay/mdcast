//! The markdown pandoc reads: every page's body, with its class projected
//! the way the target's writer understands. Split out of `pandoc.rs` to keep
//! it under the size cap.

use super::pandoc_filter::PAGE_BREAK_CLASS;
use crate::Target;
use crate::pages::Page;

/// Build the markdown input fed to pandoc, projecting per-page class according
/// to the target's idiom.
///
/// * **docx / odt** — page-based outputs; wrap each page's body in
///   `::: {custom-style="<class>"}` so the matching paragraph style from the
///   reference doc is applied. Pages are separated by an empty
///   `.mdcast-page-break` div that `pandoc_filter`'s Lua filter turns into
///   the writer's raw markup (the reader no longer accepts raw blocks from
///   the document — see `pandoc_filter`'s module docs). Docx gets
///   `<w:br w:type="page"/>` directly (no reference doc needed). Odt gets a
///   paragraph referencing the `PageBreak` style (defined in
///   `embedded/reference/reference.odt` with `fo:break-before="page"`) — ODF
///   has no reference-doc-free way to force a break, since page-break
///   formatting can only live on a named style, not inline on the paragraph.
/// * **pptx / html-reveal** — slide-based outputs; with `--slide-level=1` only
///   an h1 starts a new slide, so we project the class *onto the h1* of each
///   page, synthesising an empty h1 when the page has none. This is the
///   pandoc-native way to put a class on a slide section.
pub fn build_input(pages: &[Page], target: Target) -> String {
    let mut s = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 {
            s.push('\n');
            if matches!(target, Target::Docx | Target::Odt) {
                s.push_str(&format!("::: {{.{PAGE_BREAK_CLASS}}}\n:::\n\n"));
            }
        }
        match target {
            Target::Docx | Target::Odt => {
                s.push_str(&format!("::: {{custom-style=\"{}\"}}\n", page.class));
                s.push_str(&page.body);
                if !page.body.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(":::\n");
            }
            Target::Pptx | Target::HtmlReveal => {
                s.push_str(&project_slide(&page.body, &page.class));
                if !s.ends_with('\n') {
                    s.push('\n');
                }
            }
            _ => unreachable!(),
        }
    }
    s
}

/// Attach `{.<class>}` to the first h1 of `body`, or prepend an empty
/// `# {.<class>}` if none exists. Only handles ATX-style (`#`) headings; setext
/// underlining is rare enough to defer.
fn project_slide(body: &str, class: &str) -> String {
    let trimmed = body.trim_start_matches('\n');
    let mut lines = trimmed.lines();
    let Some(first) = lines.next() else {
        return format!("# {{.{class}}}\n");
    };
    if let Some(title) = first.strip_prefix("# ") {
        // Page already has an h1: append the class. We don't try to merge with
        // an existing attribute spec — if authors hand-write `{...}` on an h1,
        // the auto-class will collide; in that case they should set the class
        // explicitly on the wrapper instead.
        let rest: String = lines.collect::<Vec<_>>().join("\n");
        let trailing_nl = if body.ends_with('\n') { "\n" } else { "" };
        if rest.is_empty() {
            format!("# {title} {{.{class}}}\n")
        } else {
            format!("# {title} {{.{class}}}\n{rest}{trailing_nl}")
        }
    } else {
        // No h1: synthesise an empty one so pandoc starts a new slide.
        let trailing_nl = if body.ends_with('\n') { "" } else { "\n" };
        format!("# {{.{class}}}\n\n{body}{trailing_nl}")
    }
}
