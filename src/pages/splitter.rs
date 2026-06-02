//! Stage 1: split a markdown document into `RawPage`s.
//!
//! The behaviour is exposed as a `PageSplitter` trait so consumers can plug in
//! their own splitter (e.g. one that reads slide markers from a different
//! comment syntax, or that pre-resolves transclusions). `DefaultSplitter` is
//! the built-in line-based implementation.

use crate::pages::RawPage;

/// Pluggable page splitter. Implementors take raw markdown and return the
/// ordered list of pages that should drive rendering.
pub trait PageSplitter: Send + Sync {
    fn split(&self, markdown: &str) -> Vec<RawPage>;
}

/// Built-in line-based splitter.
///
/// Recognises three boundary syntaxes:
///   * `<page class="X"> … </page>`             — HTML-style wrapper
///   * `::: {.X}` … `:::`                       — Pandoc fenced div
///   * `---` thematic break                     — implicit page break
///
/// Done with a line-based pass — we don't need the full markdown AST, only
/// top-level structure. Code fences are respected so `---` / `:::` / `<page>`
/// inside code don't split pages.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultSplitter;

impl PageSplitter for DefaultSplitter {
    fn split(&self, markdown: &str) -> Vec<RawPage> {
        split(markdown)
    }
}

/// Convenience free function — equivalent to `DefaultSplitter.split(md)`.
///
/// Behaviour:
///   * Text before any boundary becomes the first page (with no explicit class).
///   * Each `---` thematic break starts a new page.
///   * An explicit `<page>` or `::: {.X}` wrapper produces a page with
///     `explicit_class = Some("X")` and resets surrounding context (so a `---`
///     directly before/after a wrapper doesn't create a phantom empty page).
///   * Empty trailing pages are dropped; an entirely empty input → zero pages.
pub fn split(markdown: &str) -> Vec<RawPage> {
    let mut pages: Vec<RawPage> = Vec::new();
    let mut buf = String::new();
    let mut state = State::Top;

    let mut wrapper: Option<Wrapper> = None;
    let mut wrapper_class: Option<String> = None;
    let mut wrapper_buf = String::new();

    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);

        match state {
            State::FencedCode { fence } => {
                if let Some(stripped) = trimmed.strip_prefix(&"`".repeat(fence)) {
                    if stripped.chars().all(|c| c == '`') {
                        state = State::Top;
                    }
                }
                push(&mut buf, &mut wrapper_buf, &wrapper, line);
                continue;
            }
            State::TildeCode { fence } => {
                if let Some(stripped) = trimmed.strip_prefix(&"~".repeat(fence)) {
                    if stripped.chars().all(|c| c == '~') {
                        state = State::Top;
                    }
                }
                push(&mut buf, &mut wrapper_buf, &wrapper, line);
                continue;
            }
            State::Top => {}
        }

        // Code-fence opener?
        if let Some(n) = leading_run(trimmed, '`')
            && n >= 3
        {
            state = State::FencedCode { fence: n };
            push(&mut buf, &mut wrapper_buf, &wrapper, line);
            continue;
        }
        if let Some(n) = leading_run(trimmed, '~')
            && n >= 3
        {
            state = State::TildeCode { fence: n };
            push(&mut buf, &mut wrapper_buf, &wrapper, line);
            continue;
        }

        // Wrapper handling — only when we are inside one.
        if let Some(w) = wrapper {
            if w.is_closer(trimmed) {
                pages.push(RawPage {
                    explicit_class: wrapper_class.take(),
                    body: std::mem::take(&mut wrapper_buf).trim_matches('\n').to_string(),
                });
                wrapper = None;
                continue;
            }
            wrapper_buf.push_str(line);
            continue;
        }

        // Outside a wrapper: detect wrapper openers, page tags, thematic breaks.
        if let Some(class) = parse_html_page_open(trimmed) {
            // Self-closing inline form not supported; treat <page class="X" /> as opening
            flush_thematic(&mut pages, &mut buf, None);
            wrapper = Some(Wrapper::Html);
            wrapper_class = Some(class);
            continue;
        }
        if let Some(class) = parse_fenced_div_open(trimmed) {
            flush_thematic(&mut pages, &mut buf, None);
            wrapper = Some(Wrapper::FencedDiv);
            wrapper_class = Some(class);
            continue;
        }
        if is_thematic_break(trimmed) {
            flush_thematic(&mut pages, &mut buf, None);
            continue;
        }

        buf.push_str(line);
    }

    // Trailing content
    if !buf.trim().is_empty() {
        pages.push(RawPage { explicit_class: None, body: buf.trim_matches('\n').to_string() });
    }

    pages
}

fn push(buf: &mut String, wrapper_buf: &mut String, wrapper: &Option<Wrapper>, line: &str) {
    if wrapper.is_some() {
        wrapper_buf.push_str(line);
    } else {
        buf.push_str(line);
    }
}

fn flush_thematic(pages: &mut Vec<RawPage>, buf: &mut String, class: Option<String>) {
    let body = std::mem::take(buf);
    let trimmed = body.trim_matches('\n');
    if !trimmed.is_empty() {
        pages.push(RawPage { explicit_class: class, body: trimmed.to_string() });
    } else if class.is_some() {
        pages.push(RawPage { explicit_class: class, body: String::new() });
    }
}

#[derive(Clone, Copy)]
enum State {
    Top,
    FencedCode { fence: usize },
    TildeCode { fence: usize },
}

#[derive(Clone, Copy)]
enum Wrapper {
    Html,
    FencedDiv,
}

impl Wrapper {
    fn is_closer(self, line: &str) -> bool {
        let t = line.trim();
        match self {
            Wrapper::Html => t.eq_ignore_ascii_case("</page>"),
            Wrapper::FencedDiv => t == ":::" || t.starts_with(":::") && t.chars().all(|c| c == ':'),
        }
    }
}

fn leading_run(s: &str, c: char) -> Option<usize> {
    let mut n = 0;
    for ch in s.chars() {
        if ch == c {
            n += 1;
        } else {
            break;
        }
    }
    if n > 0 { Some(n) } else { None }
}

fn is_thematic_break(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let c = t.chars().next().unwrap();
    if !matches!(c, '-' | '*' | '_') {
        return false;
    }
    t.chars().all(|ch| ch == c || ch.is_whitespace())
        && t.chars().filter(|ch| *ch == c).count() >= 3
}

/// Match `<page class="X">` (also `class='X'`, extra whitespace).
fn parse_html_page_open(line: &str) -> Option<String> {
    let t = line.trim();
    if !t.starts_with("<page") {
        return None;
    }
    if !t.ends_with('>') {
        return None;
    }
    let inner = &t[5..t.len() - 1]; // strip "<page" and ">"
    let inner = inner.trim_end_matches('/').trim();
    extract_class_attr(inner)
}

fn extract_class_attr(s: &str) -> Option<String> {
    // very small attr parser: look for class= and read a quoted value
    let key_pos = s.find("class")?;
    let after = &s[key_pos + 5..];
    let after = after.trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let (quote, rest) = if let Some(r) = after.strip_prefix('"') {
        ('"', r)
    } else if let Some(r) = after.strip_prefix('\'') {
        ('\'', r)
    } else {
        return None;
    };
    let end = rest.find(quote)?;
    Some(rest[..end].trim().to_string())
}

/// Match `::: {.X}` or `::: {.X .other}` — first class wins.
fn parse_fenced_div_open(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix(":::")?.trim_start();
    // Only treat as opener if there's an attribute spec — `:::` alone is a closer.
    if rest.is_empty() {
        return None;
    }
    let body = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}'))?;
    for tok in body.split_whitespace() {
        if let Some(class) = tok.strip_prefix('.') {
            return Some(class.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thematic_break_splits() {
        let md = "page one\n\n---\n\npage two\n";
        let pages = split(md);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].body, "page one");
        assert_eq!(pages[1].body, "page two");
        assert!(pages.iter().all(|p| p.explicit_class.is_none()));
    }

    #[test]
    fn html_page_wrapper_extracts_class() {
        let md = "<page class=\"hero\">\n# Quarterly\n</page>\n";
        let pages = split(md);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].explicit_class.as_deref(), Some("hero"));
        assert_eq!(pages[0].body, "# Quarterly");
    }

    #[test]
    fn fenced_div_extracts_class() {
        let md = "::: {.thanks}\nThank you!\n:::\n";
        let pages = split(md);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].explicit_class.as_deref(), Some("thanks"));
        assert_eq!(pages[0].body, "Thank you!");
    }

    #[test]
    fn mixed_syntaxes_with_thematic_breaks() {
        let md = "intro paragraph\n\n---\n\n<page class=\"hero\">\n# Big\n</page>\n\nmore body\n\n---\n\n::: {.thanks}\nbye\n:::\n";
        let pages = split(md);
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0].explicit_class, None);
        assert_eq!(pages[0].body, "intro paragraph");
        assert_eq!(pages[1].explicit_class.as_deref(), Some("hero"));
        assert_eq!(pages[2].explicit_class, None);
        assert_eq!(pages[2].body, "more body");
        assert_eq!(pages[3].explicit_class.as_deref(), Some("thanks"));
    }

    #[test]
    fn dashes_in_code_fence_are_not_breaks() {
        let md = "page one\n\n```\n---\n```\n\nstill page one\n";
        let pages = split(md);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].body.contains("---"));
    }

    #[test]
    fn empty_input_produces_no_pages() {
        assert!(split("").is_empty());
        assert!(split("\n\n").is_empty());
    }

    #[test]
    fn trait_default_matches_free_function() {
        let md = "a\n\n---\n\nb\n";
        assert_eq!(DefaultSplitter.split(md), split(md));
    }

    #[test]
    fn custom_splitter_can_be_plugged_in() {
        struct LineSplitter;
        impl PageSplitter for LineSplitter {
            fn split(&self, md: &str) -> Vec<RawPage> {
                md.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| RawPage { explicit_class: None, body: l.to_string() })
                    .collect()
            }
        }
        let pages = LineSplitter.split("a\nb\nc\n");
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[1].body, "b");
    }
}
