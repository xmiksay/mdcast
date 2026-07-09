//! Unit tests for `md_to_typst` — split out of `mod.rs` to keep the module
//! under the 400-line cap (same convention as `backends/*_tests.rs`).

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

#[test]
fn resolved_image_renders_as_typst_image_call() {
    let images = BTreeMap::from([("img/logo.png".to_string(), "/images/logo.png".to_string())]);
    let out = md_to_typst("![alt](img/logo.png)", &images);
    assert!(out.contains(r#"#image("/images/logo.png")"#));
}

/// Issue #54: an image the map has no entry for (missing provider key,
/// or a remote URL left unfetched) must not leak placeholder prose into
/// the rendered document — it's dropped, and a `tracing::warn!` is the
/// only trace (asserted end-to-end in
/// `tests/typst_unresolved_image.rs`, which can observe the log).
#[test]
fn unresolved_image_leaves_no_placeholder_text_in_output() {
    let out = md_to_typst("![alt](https://example.com/missing.png)", &BTreeMap::new());
    assert!(!out.contains("unresolved"));
    assert!(!out.contains("https://example.com/missing.png"));
}
