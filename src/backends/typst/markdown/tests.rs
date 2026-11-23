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

// A rustdoc example inside a fenced block: CommonMark keeps the outer fence
// open (the inner ``` are not at line start), but typst would close the raw
// block at the first inner run — the emitted fence must outgrow the content
// (issue #88).
#[test]
fn code_block_containing_backtick_runs_gets_a_longer_fence() {
    let md = "````rust\n/// # Examples\n/// ```\n/// let x = 1;\n/// ```\npub fn f() {}\n````\n";
    let out = md_to_typst(md, &BTreeMap::new());
    assert!(
        out.starts_with("````rust\n"),
        "fence must be strictly longer than the inner ``` runs, got: {out}"
    );
    assert!(
        out.contains("/// ```\n"),
        "inner backticks survive verbatim"
    );
    assert!(
        out.trim_end().ends_with("````"),
        "closing fence matches: {out}"
    );
}

#[test]
fn longest_backtick_run_counts_the_longest_consecutive_run() {
    assert_eq!(longest_backtick_run(""), 0);
    assert_eq!(longest_backtick_run("no ticks"), 0);
    assert_eq!(longest_backtick_run("a ` b `` c"), 2);
    assert_eq!(longest_backtick_run("x ``` y ````` z ``"), 5);
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
    assert!(out.contains(r#"[#link("https://f13.tech")[F13];]"#));
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

// Issue #104: in typst markup an embedded expression (`#raw(...)`,
// `#link(...)[...]`, …) swallows a directly following `;` as its terminator,
// and a directly following `(` / `[` / `.ident` extends it (call args,
// trailing content block, field access). Every emitted call therefore ends
// in an explicit `;`, so whatever text follows is plain markup again.
#[test]
fn inline_code_is_terminated_before_following_text() {
    for (md, expected) in [
        (
            "Use `foo`; then `bar`.\n",
            r#"Use #raw("foo");; then #raw("bar");."#,
        ),
        ("`f`(x)\n", r#"#raw("f");(x)"#),
        ("`a`[b]\n", r#"#raw("a");\[b\]"#),
        ("`s`.len\n", r#"#raw("s");.len"#),
    ] {
        let out = md_to_typst(md, &BTreeMap::new());
        assert!(out.contains(expected), "{md:?} → {out:?}");
    }
}

#[test]
fn link_footnote_and_image_calls_are_terminated() {
    let out = md_to_typst("[F13](https://f13.tech); (x).y\n", &BTreeMap::new());
    assert!(
        out.contains(r#"#link("https://f13.tech")[F13];; (x).y"#),
        "{out}"
    );

    let out = md_to_typst("See[^1]; (x).\n\n[^1]: Note.\n", &BTreeMap::new());
    assert!(out.contains("See#footnote[Note.];; (x)."), "{out}");

    let images = BTreeMap::from([("a.png".to_string(), "/images/a.png".to_string())]);
    let out = md_to_typst("![](a.png).png; ok\n", &images);
    assert!(out.contains(r#"#image("/images/a.png");.png; ok"#), "{out}");
}

/// Compiles `md_to_typst`'s output in-process and returns the laid-out
/// text with all whitespace removed (typst splits runs into separate
/// `TextItem`s, and inter-item spacing is not text).
fn rendered_text(md: &str) -> String {
    use typst::layout::{Frame, FrameItem, PagedDocument};
    use typst_as_lib::TypstEngine;

    fn walk(frame: &Frame, out: &mut String) {
        for (_, item) in frame.items() {
            match item {
                FrameItem::Text(t) => out.push_str(&t.text),
                FrameItem::Group(g) => walk(&g.frame, out),
                _ => {}
            }
        }
    }

    let src = md_to_typst(md, &BTreeMap::new());
    let engine = TypstEngine::builder()
        .main_file(src)
        .search_fonts_with(typst_as_lib::typst_kit_options::TypstKitFontOptions::default())
        .build();
    let doc: PagedDocument = engine
        .compile()
        .output
        .unwrap_or_else(|e| panic!("{md:?} failed to compile: {e:?}"));
    let mut text = String::new();
    for page in &doc.pages {
        walk(&page.frame, &mut text);
    }
    text.split_whitespace().collect()
}

#[test]
fn punctuation_after_inline_constructs_survives_typst_compile() {
    for (md, expected) in [
        ("Use `foo`; then `bar`.\n", "Usefoo;thenbar."),
        (
            "Call `f`(x) and `g`[y] and `s`.len here.\n",
            "Callf(x)andg[y]ands.lenhere.",
        ),
        ("[F13](https://f13.tech); (x).len\n", "F13;(x).len"),
        ("See[^1]; (x).\n\n[^1]: Note.\n", "See1;(x)."),
        // The same call in every capture/block context the converter emits.
        ("# Head `h`; (x)\n", "Headh;(x)"),
        ("- item `i`; (x)\n- `j`.len\n", "•itemi;(x)•j.len"),
        ("> quoted `q`; (x)\n", "quotedq;(x)"),
        (
            "Some _em `e`; (x)_ and **`s`[y]**.\n",
            "Someeme;(x)ands[y].",
        ),
        ("[`c`; (x)](https://f13.tech); z\n", "c;(x);z"),
        ("| H |\n|---|\n| `t`; (x) |\n", "Ht;(x)"),
        ("A[^n].\n\n[^n]: Uses `n`; (x).\n", "A1.1Usesn;(x)."),
        // A call ending its paragraph/line leaves no stray `;`.
        ("End `x`\n\nNext `y`\nline\n", "EndxNextyline"),
    ] {
        let got = rendered_text(md);
        assert!(got.contains(expected), "{md:?} rendered as {got:?}");
    }
}

// Issue #105: ordered lists used to come out as `- ` bullets.
#[test]
fn ordered_list_emits_numbered_markers() {
    let out = md_to_typst("1. Alpha\n2. Beta\n3. Gamma\n", &BTreeMap::new());
    assert_eq!(out, "1. Alpha\n2. Beta\n3. Gamma\n\n");
}

#[test]
fn ordered_list_keeps_start_number_without_leaking() {
    let out = md_to_typst("3. Three\n4. Four\n\npara\n\n1. One\n", &BTreeMap::new());
    assert!(out.starts_with("3. Three\n4. Four\n"), "{out:?}");
    assert!(out.contains("\n1. One\n"), "{out:?}");
}

#[test]
fn bullet_list_still_emits_dashes() {
    let out = md_to_typst("- a\n- b\n", &BTreeMap::new());
    assert_eq!(out, "- a\n- b\n\n");
}

#[test]
fn nested_lists_are_indented_past_their_marker() {
    let md = "- outer\n  1. one\n  2. two\n     - deep\n- next\n";
    let out = md_to_typst(md, &BTreeMap::new());
    assert_eq!(
        out, "- outer\n  1. one\n  2. two\n     - deep\n- next\n\n",
        "{out:?}"
    );
}

#[test]
fn multi_paragraph_item_keeps_continuation_indented() {
    let md = "1. first `x`; para\n\n   second para\n2. next\n";
    let out = md_to_typst(md, &BTreeMap::new());
    assert!(
        out.starts_with("1. first #raw(\"x\");; para\n\n   second para\n2. next\n"),
        "{out:?}"
    );
}

#[test]
fn ordered_lists_render_numbers_through_typst() {
    for (md, expected) in [
        (
            "# T\n\n1. Alpha\n2. Beta\n3. Gamma\n",
            "T1.Alpha2.Beta3.Gamma",
        ),
        ("3. Three\n4. Four\n", "3.Three4.Four"),
        // Start number doesn't leak into a following list.
        (
            "3. Three\n\ntext\n\n1. One\n2. Two\n",
            "3.Threetext1.One2.Two",
        ),
        // Ordered inside ordered.
        (
            "1. Outer\n   1. Inner a\n   2. Inner b\n2. Outer two\n",
            "1.Outer1.Innera2.Innerb2.Outertwo",
        ),
        // Ordered inside bullet.
        (
            "- Bullet\n  1. Step one\n  2. Step two\n",
            "•Bullet1.Stepone2.Steptwo",
        ),
        // Bullet inside ordered.
        (
            "1. Step\n   - detail\n2. Step two\n",
            "1.Step•detail2.Steptwo",
        ),
        // Multi-paragraph item and inline code inside an item.
        (
            "1. Run `make`; then\n\n   wait.\n2. Done\n",
            "1.Runmake;thenwait.2.Done",
        ),
        // Code block inside an item.
        (
            "1. Code:\n\n   ```\n   x = 1\n   ```\n2. After\n",
            "1.Code:x=12.After",
        ),
    ] {
        let got = rendered_text(md);
        assert_eq!(
            got,
            expected,
            "{md:?} -> {:?}",
            md_to_typst(md, &BTreeMap::new())
        );
    }
}
