// Dumps the typst driver source the backend would feed to the compiler, by
// calling the real code path (classify -> md_to_typst -> build_driver) in
// src/backends/typst instead of a hand-copied mirror. Image refs aren't
// resolved here (no AssetProvider), so they show up as "unresolved" in the
// output — that's expected for a dump tool.

use std::collections::BTreeMap;

use mdcast::backends::typst::{build_driver, md_to_typst};
use mdcast::pages::auto::classify;
use mdcast::pages::splitter::DefaultSplitter;
use mdcast::{AutoLayout, PageSplitter};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/golden/cover-deck.md".to_string());
    let md = std::fs::read_to_string(&path).unwrap();
    let raw = DefaultSplitter.split(&md);
    let pages = classify(raw, &AutoLayout::default());

    let images = BTreeMap::new();
    let typst_bodies: Vec<String> = pages
        .iter()
        .map(|p| md_to_typst(&p.body, &images))
        .collect();

    let driver = build_driver(&pages, &typst_bodies);
    println!("=== driver ===\n{driver}\n=== /driver ===");
}
