//! End-to-end check of splitter + auto-classifier on the golden fixture.
//! Locks the per-page (class, origin) sequence; renderer byte-stability is
//! a later golden test once typst/pandoc binaries are in CI.

use mdcast::pages::auto::classify;
use mdcast::pages::splitter::DefaultSplitter;
use mdcast::{AutoLayout, PageOrigin, PageSplitter};

#[test]
fn cover_deck_per_page_classification() {
    let md = std::fs::read_to_string("tests/golden/cover-deck.md").unwrap();
    let raw = DefaultSplitter.split(&md);
    let pages = classify(raw, &AutoLayout::default());

    let summary: Vec<(String, PageOrigin)> =
        pages.iter().map(|p| (p.class.clone(), p.origin)).collect();

    let expected: Vec<(&str, PageOrigin)> = vec![
        ("hero", PageOrigin::Explicit),             // <page class="hero">
        ("content", PageOrigin::AutoDefault),       // # Agenda
        ("content", PageOrigin::AutoDefault),       // # Headlines (one h1 + paragraph)
        ("section-divider", PageOrigin::AutoShape), // # Section: Margin  (single h1 only)
        ("image-full", PageOrigin::AutoShape),      // ![…](…)
        ("callout", PageOrigin::AutoShape),         // > quote
        ("two-column", PageOrigin::Explicit),       // <page class="two-column">
        ("thanks", PageOrigin::AutoPositional),     // last page, no class
    ];

    assert_eq!(
        summary.len(),
        expected.len(),
        "page count mismatch: {:#?}",
        summary
    );
    for (i, ((got_c, got_o), (exp_c, exp_o))) in summary.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got_c, exp_c, "page {i} class");
        assert_eq!(got_o, exp_o, "page {i} origin");
    }
}
