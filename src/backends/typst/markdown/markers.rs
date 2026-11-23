//! Typst block markers in converted output: list-item emission (`- ` /
//! `N. ` with continuation lines indented past the marker) and defusing
//! marker-shaped literal text at the start of a line.

/// Emits one list item: `marker`, then the item's rendered `body` with every
/// non-empty continuation line indented by the marker's width — typst keeps
/// a line in an item only while it is indented past the marker's column, so
/// nested lists and later paragraphs stay inside the item.
pub(super) fn push_item(out: &mut String, marker: &str, body: &str) {
    let indent = " ".repeat(marker.len());
    out.push_str(marker);
    for (i, line) in body.trim_matches('\n').trim_end().lines().enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(&indent);
            }
        }
        out.push_str(line);
    }
    out.push('\n');
}

/// Appends already-escaped prose `text` to `buf`, defusing a typst block
/// marker it would otherwise form at the start of a line: `- `, `+ `, `/ `,
/// `= ` (any `=` run) and `N. `. Markdown text can only begin with one of
/// these when the author escaped it (`\- x`, `1\. x`), after a hard break, or
/// as the text of a list item (`- \- x`) — in each case typst would read it
/// as a list / heading / term instead of literal text. pulldown-cmark splits
/// `1\. x` into `"1"` + `". x"`, so the digit run is checked against what
/// `buf` already holds on the current line rather than within `text` alone.
pub(super) fn push_text(buf: &mut String, text: &str) {
    let line = &buf[buf.rfind('\n').map_or(0, |i| i + 1)..];
    let marker_end = |rest: &str| rest.chars().next().is_none_or(char::is_whitespace);
    if line.is_empty() {
        let rest = text.trim_start_matches('=');
        let first = text.chars().next();
        let ends_marker = match first {
            Some('=') => marker_end(rest),
            Some('-' | '+' | '/') => marker_end(&text[1..]),
            _ => false,
        };
        if ends_marker {
            buf.push('\\');
        }
    }
    let line = &buf[buf.rfind('\n').map_or(0, |i| i + 1)..];
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let numbered = (line.is_empty() || line.bytes().all(|b| b.is_ascii_digit()))
        && (!line.is_empty() || digits > 0)
        && text[digits..].starts_with('.')
        && marker_end(&text[digits + 1..]);
    if numbered {
        buf.push_str(&text[..digits]);
        buf.push('\\');
        buf.push_str(&text[digits..]);
    } else {
        buf.push_str(text);
    }
}
