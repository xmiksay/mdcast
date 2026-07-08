//! Markdown → Typst-markup conversion, split out of `typst/mod.rs` to keep
//! that file under the project's line-count cap.

use std::collections::BTreeMap;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Accumulates a table's cells while its events stream through, so the whole
/// `#table(...)` literal can be emitted once the column count (from the
/// alignment vector) and every row are known.
#[derive(Default)]
struct TableBuilder {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
}

impl TableBuilder {
    fn columns(&self) -> usize {
        if !self.alignments.is_empty() {
            self.alignments.len()
        } else {
            self.header.len()
        }
        .max(1)
    }

    /// Pads short rows / truncates ragged-long ones to the column count so a
    /// header with more or fewer cells than a body row can't panic or emit a
    /// malformed `#table(...)` call.
    fn padded(row: &[String], cols: usize) -> Vec<String> {
        let mut v = row.to_vec();
        v.resize(cols, String::new());
        v
    }

    fn render(&self) -> String {
        let cols = self.columns();
        let mut s = String::from("#table(\n");
        s.push_str(&format!("  columns: {cols},\n"));
        if !self.alignments.is_empty() {
            let aligns: Vec<&str> = self
                .alignments
                .iter()
                .map(|a| match a {
                    Alignment::Left => "left",
                    Alignment::Center => "center",
                    Alignment::Right => "right",
                    Alignment::None => "auto",
                })
                .collect();
            s.push_str(&format!("  align: ({}),\n", aligns.join(", ")));
        }
        if !self.header.is_empty() {
            s.push_str("  table.header(\n");
            for cell in Self::padded(&self.header, cols) {
                s.push_str(&format!("    [{cell}],\n"));
            }
            s.push_str("  ),\n");
        }
        for row in &self.rows {
            for cell in Self::padded(row, cols) {
                s.push_str(&format!("    [{cell}],\n"));
            }
        }
        s.push_str(")\n\n");
        s
    }
}

/// Convert markdown to a Typst-markup string suitable for `eval(.., mode: "markup")`.
/// Image refs use the `images` map produced by `collect_images_for_typst`. Anything
/// the converter doesn't know about (HTML blocks, footnotes, …) is dropped — v1
/// scope, expanded as concrete fixtures demand it.
pub fn md_to_typst(md: &str, images: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut in_image = 0i32;
    let mut in_code_block = false;
    // While `Some`, table-cell inline content (text/emphasis/code/…) is
    // captured here instead of `out`, so the whole row can be re-emitted as
    // Typst cell literals once the cell/row/table closes.
    let mut cell_buf: Option<String> = None;
    let mut table: Option<TableBuilder> = None;
    let parser = Parser::new_ext(md, Options::ENABLE_TABLES);

    for ev in parser {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                out.push_str(&"=".repeat(heading_depth(level)));
                out.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("\n\n"),

            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),

            Event::Start(Tag::Emphasis) => push_char(&mut out, &mut cell_buf, '_'),
            Event::End(TagEnd::Emphasis) => push_char(&mut out, &mut cell_buf, '_'),
            Event::Start(Tag::Strong) => push_char(&mut out, &mut cell_buf, '*'),
            Event::End(TagEnd::Strong) => push_char(&mut out, &mut cell_buf, '*'),

            Event::Start(Tag::List(_)) => {}
            Event::End(TagEnd::List(_)) => out.push('\n'),
            Event::Start(Tag::Item) => out.push_str("- "),
            Event::End(TagEnd::Item) => out.push('\n'),

            Event::Start(Tag::BlockQuote(_)) => out.push_str("#quote(block: true)[\n"),
            Event::End(TagEnd::BlockQuote(_)) => out.push_str("\n]\n\n"),

            Event::Start(Tag::Image { dest_url, .. }) => {
                in_image += 1;
                let s = match images.get(dest_url.as_ref()) {
                    Some(vpath) => format!("#image({})", typst_string(vpath)),
                    None => format!("[image unresolved: {dest_url}]"),
                };
                push_str(&mut out, &mut cell_buf, &s);
            }
            Event::End(TagEnd::Image) => {
                in_image -= 1;
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                match kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                        out.push_str("```");
                        out.push_str(&lang);
                        out.push('\n');
                    }
                    _ => out.push_str("```\n"),
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                out.push_str("```\n\n");
            }

            Event::Start(Tag::Table(alignments)) => {
                table = Some(TableBuilder {
                    alignments,
                    ..Default::default()
                });
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    out.push_str(&t.render());
                }
            }
            Event::Start(Tag::TableHead) => {}
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.header = std::mem::take(&mut t.current_row);
                }
            }
            Event::Start(Tag::TableRow) => {}
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    let row = std::mem::take(&mut t.current_row);
                    t.rows.push(row);
                }
            }
            Event::Start(Tag::TableCell) => cell_buf = Some(String::new()),
            Event::End(TagEnd::TableCell) => {
                let cell = cell_buf.take().unwrap_or_default();
                if let Some(t) = table.as_mut() {
                    t.current_row.push(cell);
                }
            }

            Event::Text(t) => {
                if in_code_block {
                    out.push_str(&t);
                } else if in_image == 0 {
                    push_str(&mut out, &mut cell_buf, &escape_typst_inline(&t));
                }
            }
            Event::Code(c) => {
                // `raw()` sidesteps the backtick-delimiter counting rules of
                // Typst's ` `code` ` shorthand, which cannot represent a
                // backtick inside inline (non-block) raw text at all.
                let s = format!("#raw({})", typst_string(&c));
                push_str(&mut out, &mut cell_buf, &s);
            }
            Event::SoftBreak => push_char(&mut out, &mut cell_buf, ' '),
            Event::HardBreak => push_str(&mut out, &mut cell_buf, "\\\n"),

            _ => {}
        }
    }
    out
}

/// Writes into the current table cell's buffer if one is open, `out` otherwise.
fn push_str(out: &mut String, cell_buf: &mut Option<String>, s: &str) {
    match cell_buf {
        Some(buf) => buf.push_str(s),
        None => out.push_str(s),
    }
}

fn push_char(out: &mut String, cell_buf: &mut Option<String>, c: char) {
    match cell_buf {
        Some(buf) => buf.push(c),
        None => out.push(c),
    }
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
/// a known v1 limitation. `[` / `]` are escaped because table cells wrap their
/// content in a `[...]` literal — an unescaped bracket there would unbalance
/// the enclosing content block instead of rendering as a literal character.
fn escape_typst_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '#' | '@' | '<' | '>' | '$' | '\\' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

pub(super) fn typst_string(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_block_passes_text_through_unescaped() {
        let md = "```c\n#include <stdio.h>\nint main() { return 1 < 2; }\n```\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("#include <stdio.h>"));
        assert!(out.contains("int main() { return 1 < 2; }"));
        assert!(!out.contains(r"\#include"));
        assert!(!out.contains(r"\<stdio"));
    }

    #[test]
    fn code_block_emits_fence_language() {
        let md = "```c\nint x;\n```\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.starts_with("```c\n"));
    }

    #[test]
    fn code_block_without_language_falls_back_to_bare_fence() {
        let md = "```\nplain\n```\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.starts_with("```\n"));
    }

    #[test]
    fn prose_outside_code_block_still_escaped() {
        let md = "1 < 2 and a # b\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r"1 \< 2 and a \# b"));
    }

    #[test]
    fn inline_code_uses_raw_function() {
        let md = "`foo`\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("#raw(\"foo\")"));
    }

    #[test]
    fn inline_code_with_backtick_is_safe() {
        let md = "``a`b``\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"#raw("a`b")"#));
    }

    #[test]
    fn table_emits_gridded_table_with_columns_and_alignment() {
        let md = "\
| Left | Center | Right |
|:-----|:------:|------:|
| a | b | c |
";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("#table("));
        assert!(out.contains("columns: 3"));
        assert!(out.contains("align: (left, center, right)"));
        assert!(out.contains("table.header("));
        assert!(out.contains("[Left]"));
        assert!(out.contains("[Center]"));
        assert!(out.contains("[Right]"));
        assert!(out.contains("[a]"));
        assert!(out.contains("[b]"));
        assert!(out.contains("[c]"));
    }

    #[test]
    fn table_without_alignment_markers_uses_auto() {
        let md = "| H |\n|---|\n| x |\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("align: (auto)"));
    }

    #[test]
    fn table_cell_inline_marks_render_styled() {
        let md = "| H |\n|---|\n| **bold** and _em_ and `code` |\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"[*bold* and _em_ and #raw("code")]"#));
    }

    #[test]
    fn table_cell_special_chars_do_not_break_out() {
        let md = "| H |\n|---|\n| a\\|b #c [d] \\\\ |\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r"[a|b \#c \[d\] \\]"));
    }

    #[test]
    fn ragged_row_pads_to_header_column_count() {
        let md = "| A | B | C |\n|---|---|---|\n| x |\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("columns: 3"));
        assert!(out.contains("[x]"));
        // The short row must still contribute two empty cells, not panic.
        let body_cells = out
            .rsplit("),\n")
            .next()
            .expect("body section after table.header(...)");
        assert_eq!(body_cells.matches("[]").count(), 2);
    }

    #[test]
    fn empty_cell_renders_as_empty_bracket() {
        let md = "| A | B |\n|---|---|\n| | y |\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("[]"));
        assert!(out.contains("[y]"));
    }
}
