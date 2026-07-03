//! Strip a leading YAML frontmatter block (`---` … `---`) off a markdown
//! document and parse it into a `DocMeta`, before `PageSplitter` ever sees
//! the document — otherwise the splitter reads the closing fence as a
//! thematic break and the frontmatter becomes a phantom `hero` page.
//!
//! Only a flat `key: value` subset of YAML is supported: no nesting, lists,
//! or multiline scalars. That covers `title`/`author`/`date` plus arbitrary
//! extra scalar keys, which is all `DocMeta` carries.

use crate::DocMeta;

/// Split `markdown` into `(meta, body)`. If the document doesn't open with a
/// `---` fence on its own line, or the fence is never closed, `meta` is
/// empty and `body` is the input unchanged.
pub fn extract(markdown: &str) -> (DocMeta, String) {
    let mut lines = markdown.lines();
    if lines.next() != Some("---") {
        return (DocMeta::default(), markdown.to_string());
    }

    let mut block = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        block.push(line);
    }
    if !closed {
        return (DocMeta::default(), markdown.to_string());
    }

    let meta = parse_block(&block);
    let body = lines.collect::<Vec<_>>().join("\n");
    (meta, body)
}

fn parse_block(lines: &[&str]) -> DocMeta {
    let mut meta = DocMeta::default();
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        if key.is_empty() || value.is_empty() {
            continue;
        }
        match key {
            "title" => meta.title = Some(value),
            "author" => meta.author = Some(value),
            "date" => meta.date = Some(value),
            _ => {
                meta.extra.insert(key.to_string(), value);
            }
        }
    }
    meta
}

fn unquote(s: &str) -> String {
    let mut chars = s.chars();
    match (chars.next(), chars.next_back()) {
        (Some('"'), Some('"')) | (Some('\''), Some('\'')) if s.len() >= 2 => {
            chars.as_str().to_string()
        }
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_passes_through_unchanged() {
        let md = "# Real first page\n";
        let (meta, body) = extract(md);
        assert!(meta.title.is_none());
        assert_eq!(body, md);
    }

    #[test]
    fn strips_frontmatter_and_populates_meta() {
        let md = "---\ntitle: My Doc\nauthor: Martin\n---\n\n# Real first page\n";
        let (meta, body) = extract(md);
        assert_eq!(meta.title.as_deref(), Some("My Doc"));
        assert_eq!(meta.author.as_deref(), Some("Martin"));
        assert_eq!(meta.date, None);
        assert_eq!(body.trim(), "# Real first page");
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let md = "---\ntitle: \"Quoted Title\"\ndate: '2026-07-03'\n---\nbody\n";
        let (meta, _) = extract(md);
        assert_eq!(meta.title.as_deref(), Some("Quoted Title"));
        assert_eq!(meta.date.as_deref(), Some("2026-07-03"));
    }

    #[test]
    fn unknown_keys_land_in_extra() {
        let md = "---\ntitle: T\nsubtitle: Sub\n---\nbody\n";
        let (meta, _) = extract(md);
        assert_eq!(meta.extra.get("subtitle").map(String::as_str), Some("Sub"));
    }

    #[test]
    fn unclosed_fence_is_left_alone() {
        let md = "---\ntitle: T\n\n# Not actually closed\n";
        let (meta, body) = extract(md);
        assert!(meta.title.is_none());
        assert_eq!(body, md);
    }

    #[test]
    fn dashes_not_at_top_are_not_frontmatter() {
        let md = "intro\n\n---\n\ntitle: T\n\n---\n";
        let (meta, body) = extract(md);
        assert!(meta.title.is_none());
        assert_eq!(body, md);
    }
}
