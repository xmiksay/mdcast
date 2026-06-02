//! Stage 2: assign a `class` to every `RawPage`, producing `Page`s.
//!
//! Rule order per page:
//!   1. Explicit class wins.
//!   2. Content-shape rules (configurable in `[auto_layout.rules]`).
//!   3. Positional class (first/last) from `[auto_layout]`.
//!   4. Default class.
//!
//! Shape rules are placed *above* positional so that, say, a single-image
//! cover page still becomes `image-full` instead of being forced into `hero`.
//! The author can always override either by adding an explicit class.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

use crate::brand::{AutoLayout, ShapePredicate};
use crate::pages::{Page, PageOrigin, RawPage};

pub fn classify(raw: Vec<RawPage>, cfg: &AutoLayout) -> Vec<Page> {
    let last_idx = raw.len().saturating_sub(1);
    raw.into_iter()
        .enumerate()
        .map(|(idx, page)| classify_one(idx, last_idx, page, cfg))
        .collect()
}

fn classify_one(idx: usize, last_idx: usize, page: RawPage, cfg: &AutoLayout) -> Page {
    if let Some(c) = page.explicit_class {
        return Page { class: c, body: page.body, origin: PageOrigin::Explicit };
    }
    let shape = detect_shape(&page.body);
    if let Some(rule) = cfg.rules.iter().find(|r| r.when == shape && shape != ShapePredicate::Empty)
    {
        return Page { class: rule.class.clone(), body: page.body, origin: PageOrigin::AutoShape };
    }
    // Empty pages still go through positional/default — empty hero/thanks are
    // legitimate (image-only cover via assets, etc.).
    if idx == 0
        && let Some(c) = &cfg.first
    {
        return Page { class: c.clone(), body: page.body, origin: PageOrigin::AutoPositional };
    }
    if idx == last_idx
        && last_idx != 0
        && let Some(c) = &cfg.last
    {
        return Page { class: c.clone(), body: page.body, origin: PageOrigin::AutoPositional };
    }
    Page { class: cfg.default.clone(), body: page.body, origin: PageOrigin::AutoDefault }
}

fn detect_shape(body: &str) -> ShapePredicate {
    if body.trim().is_empty() {
        return ShapePredicate::Empty;
    }

    // Walk the markdown event stream and classify the page by what top-level
    // block(s) it contains. We only need to distinguish a handful of shapes;
    // anything ambiguous falls through to "not a special shape" (returns Empty
    // sentinel meaning "no shape match" — checked against in classify_one).
    let parser = Parser::new_ext(body, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH);

    let mut top_blocks: Vec<TopBlock> = Vec::new();
    let mut depth: i32 = 0;
    let mut current: Option<TopBlock> = None;

    for ev in parser {
        match ev {
            Event::Start(tag) => {
                if depth == 0 {
                    current = Some(match &tag {
                        Tag::Heading { level: HeadingLevel::H1, .. } => TopBlock::H1,
                        Tag::Heading { .. } => TopBlock::OtherHeading,
                        Tag::Paragraph => TopBlock::Paragraph,
                        Tag::BlockQuote(_) => TopBlock::BlockQuote,
                        Tag::Image { .. } => TopBlock::Image,
                        _ => TopBlock::Other,
                    });
                }
                depth += 1;
                if let Tag::Image { .. } = tag
                    && depth == 2
                    && matches!(current, Some(TopBlock::Paragraph))
                {
                    // image inside a paragraph at top level
                    current = Some(TopBlock::ParagraphWithImage);
                }
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0
                    && let Some(b) = current.take()
                {
                    top_blocks.push(b);
                }
            }
            _ => {}
        }
    }

    match top_blocks.as_slice() {
        [TopBlock::H1] => ShapePredicate::SingleH1Only,
        [TopBlock::Image] | [TopBlock::ParagraphWithImage] => ShapePredicate::SingleImageOnly,
        [TopBlock::BlockQuote] => ShapePredicate::SingleBlockquoteOnly,
        // Sentinel: nothing matched a shape rule. Use Empty here ONLY for
        // "no match" — the classify_one early-return ensures we never pick the
        // Empty rule for non-empty content.
        _ => ShapePredicate::Empty,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopBlock {
    H1,
    OtherHeading,
    Paragraph,
    ParagraphWithImage,
    BlockQuote,
    Image,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AutoLayout {
        AutoLayout::default()
    }

    fn raw(class: Option<&str>, body: &str) -> RawPage {
        RawPage { explicit_class: class.map(str::to_string), body: body.to_string() }
    }

    #[test]
    fn explicit_class_wins() {
        let pages = classify(vec![raw(Some("custom"), "# anything")], &cfg());
        assert_eq!(pages[0].class, "custom");
        assert_eq!(pages[0].origin, PageOrigin::Explicit);
    }

    #[test]
    fn positional_first_and_last() {
        let pages = classify(
            vec![raw(None, "intro\n"), raw(None, "middle\n"), raw(None, "bye\n")],
            &cfg(),
        );
        assert_eq!(pages[0].class, "hero");
        assert_eq!(pages[0].origin, PageOrigin::AutoPositional);
        assert_eq!(pages[1].class, "content");
        assert_eq!(pages[1].origin, PageOrigin::AutoDefault);
        assert_eq!(pages[2].class, "thanks");
        assert_eq!(pages[2].origin, PageOrigin::AutoPositional);
    }

    #[test]
    fn single_h1_only_becomes_section_divider() {
        // Middle page — positional doesn't apply, shape rule does.
        let pages = classify(
            vec![raw(None, "a"), raw(None, "# Section"), raw(None, "b")],
            &cfg(),
        );
        assert_eq!(pages[1].class, "section-divider");
        assert_eq!(pages[1].origin, PageOrigin::AutoShape);
    }

    #[test]
    fn single_image_becomes_image_full() {
        let pages = classify(
            vec![raw(None, "a"), raw(None, "![alt](foo.png)"), raw(None, "b")],
            &cfg(),
        );
        assert_eq!(pages[1].class, "image-full");
        assert_eq!(pages[1].origin, PageOrigin::AutoShape);
    }

    #[test]
    fn single_blockquote_becomes_callout() {
        let pages = classify(
            vec![raw(None, "a"), raw(None, "> a wise quote"), raw(None, "b")],
            &cfg(),
        );
        assert_eq!(pages[1].class, "callout");
    }

    #[test]
    fn shape_rule_beats_positional_on_first() {
        let pages = classify(vec![raw(None, "# Section"), raw(None, "x"), raw(None, "y")], &cfg());
        assert_eq!(pages[0].class, "section-divider");
    }

    #[test]
    fn single_page_doc_uses_first_not_last() {
        let pages = classify(vec![raw(None, "hello")], &cfg());
        assert_eq!(pages[0].class, "hero");
    }
}
