// Dumps the typst driver source that the backend would feed to the compiler,
// for the cover-deck.md fixture.

use mdcast::pages::auto::classify;
use mdcast::pages::splitter::DefaultSplitter;
use mdcast::{AutoLayout, PageSplitter};

fn main() {
    let md = std::fs::read_to_string("tests/golden/cover-deck.md").unwrap();
    let raw = DefaultSplitter.split(&md);
    let pages = classify(raw, &AutoLayout::default());

    let driver = build_driver(&pages);
    println!("=== driver ===\n{driver}\n=== /driver ===");
}

// Mirror of src/backends/typst.rs::build_driver — kept in sync manually.
fn build_driver(pages: &[mdcast::Page]) -> String {
    let mut s = String::new();
    let mut classes: Vec<&str> = pages.iter().map(|p| p.class.as_str()).collect();
    classes.sort();
    classes.dedup();
    for class in &classes {
        s.push_str(&format!(
            "#import \"layouts/{}.typ\": layout as {}\n",
            class.replace(['/', '\\'], "_"),
            alias_for(class)
        ));
    }
    s.push('\n');
    for page in pages {
        let alias = alias_for(&page.class);
        let escaped = typst_string(&page.body);
        s.push_str(&format!("#{alias}({escaped})\n"));
    }
    s
}

fn alias_for(class: &str) -> String {
    let mut out = String::from("layout_");
    for c in class.chars() {
        if c.is_ascii_alphanumeric() { out.push(c) } else { out.push('_') }
    }
    out
}

fn typst_string(s: &str) -> String {
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
