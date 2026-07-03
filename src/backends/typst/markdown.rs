//! Markdown → Typst-markup conversion, split out of `typst/mod.rs` to keep
//! that file under the project's line-count cap.

use std::collections::BTreeMap;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};

/// Convert markdown to a Typst-markup string suitable for `eval(.., mode: "markup")`.
/// Image refs use the `images` map produced by `collect_images_for_typst`. Anything
/// the converter doesn't know about (HTML blocks, footnotes, …) is dropped — v1
/// scope, expanded as concrete fixtures demand it.
pub fn md_to_typst(md: &str, images: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut in_image = 0i32;
    let mut in_code_block = false;
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

            Event::Text(t) => {
                if in_code_block {
                    out.push_str(&t);
                } else if in_image == 0 {
                    out.push_str(&escape_typst_inline(&t));
                }
            }
            Event::Code(c) => {
                // `raw()` sidesteps the backtick-delimiter counting rules of
                // Typst's ` `code` ` shorthand, which cannot represent a
                // backtick inside inline (non-block) raw text at all.
                out.push_str("#raw(");
                out.push_str(&typst_string(&c));
                out.push(')');
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
}
