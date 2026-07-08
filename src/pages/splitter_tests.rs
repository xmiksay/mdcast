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
fn self_closing_html_page_is_an_empty_page() {
    let md = "<page class=\"section\" />\n\nafter\n";
    let pages = split(md);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].explicit_class.as_deref(), Some("section"));
    assert_eq!(pages[0].body, "");
    assert_eq!(pages[1].explicit_class, None);
    assert_eq!(pages[1].body, "after");
}

#[test]
fn self_closing_html_page_no_space_before_slash() {
    let md = "<page class=\"section\"/>\n\nafter\n";
    let pages = split(md);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].explicit_class.as_deref(), Some("section"));
    assert_eq!(pages[0].body, "");
}

#[test]
fn nested_fenced_divs_do_not_close_page_early() {
    let md =
        "::: {.columns}\n::: {.column}\nleft\n:::\n::: {.column}\nright\n:::\n:::\n\nnext page\n";
    let pages = split(md);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].explicit_class.as_deref(), Some("columns"));
    assert!(pages[0].body.contains("left"));
    assert!(pages[0].body.contains("right"));
    assert_eq!(pages[1].explicit_class, None);
    assert_eq!(pages[1].body, "next page");
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
                .map(|l| RawPage {
                    explicit_class: None,
                    body: l.to_string(),
                })
                .collect()
        }
    }
    let pages = LineSplitter.split("a\nb\nc\n");
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[1].body, "b");
}
