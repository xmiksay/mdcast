fn main() {
    let md = std::fs::read_to_string("tests/golden/cover-deck.md").unwrap();
    let raw = mdcast::pages::splitter::DefaultSplitter::default();
    use mdcast::PageSplitter;
    let pages = raw.split(&md);
    let classified = mdcast::pages::auto::classify(pages, &mdcast::AutoLayout::default());
    let s = mdcast::backends::pandoc::build_input(&classified, mdcast::Target::HtmlReveal);
    println!("{}", s);
}
