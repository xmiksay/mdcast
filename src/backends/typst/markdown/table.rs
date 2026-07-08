//! Typst `#table(...)` projection for GFM tables, split out of
//! `markdown/mod.rs` to keep that file under the project's line-count cap.

use pulldown_cmark::Alignment;

/// Accumulates a table's cells while its events stream through, so the whole
/// `#table(...)` literal can be emitted once the column count (from the
/// alignment vector) and every row are known.
#[derive(Default)]
pub(super) struct TableBuilder {
    pub(super) alignments: Vec<Alignment>,
    pub(super) header: Vec<String>,
    pub(super) rows: Vec<Vec<String>>,
    pub(super) current_row: Vec<String>,
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

    pub(super) fn render(&self) -> String {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::md_to_typst;

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
