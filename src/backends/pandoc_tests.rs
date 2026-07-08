use super::*;
use crate::pages::{Page, PageOrigin};

fn p(class: &str, body: &str) -> Page {
    Page {
        class: class.into(),
        body: body.into(),
        origin: PageOrigin::Explicit,
    }
}

#[test]
fn slide_input_attaches_class_to_existing_h1() {
    let out = build_input(
        &[p("hero", "# Title\n\nsub"), p("content", "no heading body")],
        Target::HtmlReveal,
    );
    assert!(out.contains("# Title {.hero}"), "{out}");
    // Page 2 has no h1 — synthesises one carrying the class.
    assert!(out.contains("# {.content}"), "{out}");
}

#[test]
fn page_input_uses_raw_openxml_pagebreak_for_docx() {
    let out = build_input(&[p("hero", "intro"), p("content", "body")], Target::Docx);
    assert!(out.contains("```{=openxml}"), "{out}");
    assert!(out.contains(r#"<w:br w:type="page"/>"#), "{out}");
    assert!(!out.contains(r"\pagebreak"), "{out}");
    assert!(out.contains(r#"custom-style="hero""#));
    assert!(out.contains(r#"custom-style="content""#));
}

#[test]
fn page_input_uses_raw_opendocument_pagebreak_for_odt() {
    let out = build_input(&[p("hero", "intro"), p("content", "body")], Target::Odt);
    assert!(out.contains("```{=opendocument}"), "{out}");
    assert!(
        out.contains(r#"<text:p text:style-name="PageBreak"/>"#),
        "{out}"
    );
    assert!(!out.contains(r"\pagebreak"), "{out}");
    assert!(out.contains(r#"custom-style="hero""#));
    assert!(out.contains(r#"custom-style="content""#));
}

#[test]
fn toc_args_adds_toc_and_depth_for_docx_and_odt() {
    assert_eq!(
        toc_args(Target::Docx, Some(3)),
        vec!["--toc".to_string(), "--toc-depth=3".to_string()]
    );
    assert_eq!(
        toc_args(Target::Odt, Some(2)),
        vec!["--toc".to_string(), "--toc-depth=2".to_string()]
    );
}

#[test]
fn toc_args_empty_when_not_requested() {
    assert!(toc_args(Target::Docx, None).is_empty());
    assert!(toc_args(Target::Odt, None).is_empty());
}

#[test]
fn toc_args_ignored_for_slide_targets() {
    assert!(toc_args(Target::Pptx, Some(3)).is_empty());
    assert!(toc_args(Target::HtmlReveal, Some(3)).is_empty());
}

#[test]
fn single_page_has_no_separator() {
    let out = build_input(&[p("hero", "just one")], Target::HtmlReveal);
    assert!(!out.contains(r"\pagebreak"));
    assert!(!out.contains("openxml"));
    assert!(!out.contains("opendocument"));
    assert!(out.starts_with("# {.hero}"));
}

struct MockAssets(std::collections::BTreeMap<&'static str, &'static [u8]>);

impl AssetProvider for MockAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        let v = self.0.get(key).map(|b| Bytes::from_static(b));
        Box::pin(async move { Ok(v) })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        let out: Vec<String> = self
            .0
            .keys()
            .filter(|k| k.starts_with(prefix))
            .map(|k| k.to_string())
            .collect();
        Box::pin(async move { Ok(out) })
    }
}

#[tokio::test]
async fn materialise_subtree_fetches_only_prefixed_keys_in_parallel() {
    let mut m = std::collections::BTreeMap::new();
    m.insert("revealjs/dist/reveal.js", b"AAA".as_slice());
    m.insert("revealjs/plugin/notes.js", b"BBB".as_slice());
    m.insert("reference/reference.odt", b"CCC".as_slice());
    let provider = MockAssets(m);

    let dir = tempfile::tempdir().unwrap();
    let written = materialise_subtree(&provider, "revealjs/", dir.path())
        .await
        .unwrap();

    assert_eq!(written, 2);
    assert_eq!(
        tokio::fs::read(dir.path().join("dist/reveal.js"))
            .await
            .unwrap(),
        b"AAA"
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("plugin/notes.js"))
            .await
            .unwrap(),
        b"BBB"
    );
    assert!(!dir.path().join("reference.odt").exists());
}
