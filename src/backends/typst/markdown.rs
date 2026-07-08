//! Markdown → Typst-markup conversion, split out of `typst/mod.rs` to keep
//! that file under the project's line-count cap.

use std::collections::BTreeMap;

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};

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
/// the converter doesn't know about (raw HTML blocks, …) is dropped — v1 scope,
/// expanded as concrete fixtures demand it.
///
/// Footnote definitions commonly appear *after* their reference site in
/// document order, so footnotes are resolved with two passes over the same
/// event list: the first only harvests `label -> rendered body` (its `out` is
/// discarded), the second does the real render using that map to expand each
/// `FootnoteReference` inline as `#footnote[...]`.
pub fn md_to_typst(md: &str, images: &BTreeMap<String, String>) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES;
    let events: Vec<Event> = Parser::new_ext(md, options).collect();
    let (_, footnote_defs) = render_events(&events, images, &BTreeMap::new());
    let (out, _) = render_events(&events, images, &footnote_defs);
    out
}

/// Runs the conversion state machine once over `events`. Returns the rendered
/// Typst markup together with whatever footnote definitions it collected
/// along the way (`label -> rendered body`), so callers can do the two-pass
/// dance in `md_to_typst`. `footnote_defs` supplies bodies for
/// `FootnoteReference` lookups (from a prior pass); pass an empty map to only
/// harvest definitions.
fn render_events(
    events: &[Event],
    images: &BTreeMap<String, String>,
    footnote_defs: &BTreeMap<String, String>,
) -> (String, BTreeMap<String, String>) {
    let mut out = String::new();
    let mut in_image = 0i32;
    let mut in_code_block = false;
    // Generic capture stack: while non-empty, inline content (text/emphasis/
    // code/links/…) is appended to its top buffer instead of `out`. Table
    // cells, link text, and footnote-definition bodies each push one frame at
    // Start and pop it at End, so nested captures (a link inside a table
    // cell, say) compose without special-casing.
    let mut stack: Vec<String> = Vec::new();
    let mut link_dest_stack: Vec<String> = Vec::new();
    let mut footnote_label_stack: Vec<String> = Vec::new();
    let mut table: Option<TableBuilder> = None;
    let mut collected_defs = BTreeMap::new();

    for ev in events {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                out.push_str(&"=".repeat(heading_depth(*level)));
                out.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("\n\n"),

            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),

            Event::Start(Tag::Emphasis) => push_char(&mut out, &mut stack, '_'),
            Event::End(TagEnd::Emphasis) => push_char(&mut out, &mut stack, '_'),
            Event::Start(Tag::Strong) => push_char(&mut out, &mut stack, '*'),
            Event::End(TagEnd::Strong) => push_char(&mut out, &mut stack, '*'),

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
                push_str(&mut out, &mut stack, &s);
            }
            Event::End(TagEnd::Image) => {
                in_image -= 1;
            }

            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                // pulldown-cmark leaves `<jane@example.com>`'s dest_url as
                // the bare address — its HTML writer prepends `mailto:`
                // itself, so we must too, or the link would just open the
                // address as a (nonsensical) relative URL.
                let dest = if *link_type == LinkType::Email {
                    format!("mailto:{dest_url}")
                } else {
                    dest_url.to_string()
                };
                link_dest_stack.push(dest);
                stack.push(String::new());
            }
            Event::End(TagEnd::Link) => {
                let text = stack.pop().unwrap_or_default();
                let dest = link_dest_stack.pop().unwrap_or_default();
                let s = format!("#link({})[{text}]", typst_string(&dest));
                push_str(&mut out, &mut stack, &s);
            }

            Event::Start(Tag::FootnoteDefinition(label)) => {
                footnote_label_stack.push(label.to_string());
                stack.push(String::new());
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                let body = stack.pop().unwrap_or_default();
                if let Some(label) = footnote_label_stack.pop() {
                    collected_defs.insert(label, body.trim().to_string());
                }
            }
            Event::FootnoteReference(label) => {
                if let Some(body) = footnote_defs.get(label.as_ref()) {
                    let s = format!("#footnote[{body}]");
                    push_str(&mut out, &mut stack, &s);
                }
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                match kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                        out.push_str("```");
                        out.push_str(lang);
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
                    alignments: alignments.clone(),
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
            Event::Start(Tag::TableCell) => stack.push(String::new()),
            Event::End(TagEnd::TableCell) => {
                let cell = stack.pop().unwrap_or_default();
                if let Some(t) = table.as_mut() {
                    t.current_row.push(cell);
                }
            }

            Event::Text(t) => {
                if in_code_block {
                    out.push_str(t);
                } else if in_image == 0 {
                    push_str(&mut out, &mut stack, &escape_typst_inline(t));
                }
            }
            Event::Code(c) => {
                // `raw()` sidesteps the backtick-delimiter counting rules of
                // Typst's ` `code` ` shorthand, which cannot represent a
                // backtick inside inline (non-block) raw text at all.
                let s = format!("#raw({})", typst_string(c));
                push_str(&mut out, &mut stack, &s);
            }
            Event::SoftBreak => push_char(&mut out, &mut stack, ' '),
            Event::HardBreak => push_str(&mut out, &mut stack, "\\\n"),

            _ => {}
        }
    }
    (out, collected_defs)
}

/// Writes into the innermost open capture buffer (table cell / link text /
/// footnote body) if one is open, `out` otherwise.
fn push_str(out: &mut String, stack: &mut [String], s: &str) {
    match stack.last_mut() {
        Some(buf) => buf.push_str(s),
        None => out.push_str(s),
    }
}

fn push_char(out: &mut String, stack: &mut [String], c: char) {
    match stack.last_mut() {
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

/// Escape characters that have special meaning in Typst markup. `_` and `*`
/// are escaped here because this only ever runs over literal `Event::Text`
/// content — the emphasis/strong markers the converter itself emits go
/// through `push_char` directly and never pass through this function, so
/// escaping them here can't defang intentional emphasis. `[` / `]` are
/// escaped because table cells (and link text) wrap their content in a
/// `[...]` literal — an unescaped bracket there would unbalance the enclosing
/// content block instead of rendering as a literal character.
fn escape_typst_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '#' | '@' | '<' | '>' | '$' | '\\' | '[' | ']' | '_' | '*' => {
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

    #[test]
    fn inline_link_renders_as_typst_link() {
        let md = "[F13](https://f13.tech)\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"#link("https://f13.tech")[F13]"#));
    }

    #[test]
    fn reference_style_link_resolves_via_reference_map() {
        let md = "[F13][f13]\n\n[f13]: https://f13.tech\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"#link("https://f13.tech")[F13]"#));
    }

    #[test]
    fn link_text_supports_emphasis_and_escaping() {
        let md = "[**bold** text](https://example.com)\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"#link("https://example.com")[*bold* text]"#));
    }

    #[test]
    fn link_inside_table_cell_stays_scoped_to_the_cell() {
        let md = "| H |\n|---|\n| [F13](https://f13.tech) |\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"[#link("https://f13.tech")[F13]]"#));
    }

    #[test]
    fn autolink_renders_as_typst_link() {
        let md = "<https://example.com>\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r#"#link("https://example.com")[https://example.com]"#));
    }

    #[test]
    fn email_autolink_renders_as_mailto_link() {
        let md = "<jane@example.com>\n";
        let out = md_to_typst(md, &BTreeMap::new());
        // The `@` in the display text goes through the same prose escaping
        // as any other text run (Typst's `@` introduces a reference).
        assert!(out.contains(r#"#link("mailto:jane@example.com")[jane\@example.com]"#));
    }

    #[test]
    fn footnote_reference_expands_to_typst_footnote_at_reference_site() {
        let md = "See here.[^1]\n\n[^1]: Extra detail.\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains("See here.#footnote[Extra detail.]"));
    }

    #[test]
    fn footnote_definition_body_is_not_emitted_inline() {
        let md = "See here.[^1]\n\n[^1]: Extra detail.\n";
        let out = md_to_typst(md, &BTreeMap::new());
        // The definition body must appear once, wrapped in `#footnote[...]`
        // at the reference site — not a second time where it was declared.
        assert_eq!(out.matches("Extra detail.").count(), 1);
    }

    #[test]
    fn snake_case_identifier_is_not_italicised() {
        let md = "snake_case_name and a*b\n";
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(r"snake\_case\_name and a\*b"));
    }
}
