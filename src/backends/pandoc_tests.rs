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

struct PathAssets {
    key: &'static str,
    path: Option<std::path::PathBuf>,
    bytes: &'static [u8],
}

impl AssetProvider for PathAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>>> {
        let v = (key == self.key).then(|| Bytes::from_static(self.bytes));
        Box::pin(async move { Ok(v) })
    }

    fn list<'a>(&'a self, _prefix: &'a str) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn path<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<std::path::PathBuf>>> {
        let v = if key == self.key {
            self.path.clone()
        } else {
            None
        };
        Box::pin(async move { Ok(v) })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn materialise_hard_links_when_the_provider_offers_a_path() {
    use std::os::unix::fs::MetadataExt;

    let source_dir = tempfile::tempdir().unwrap();
    let source_path = source_dir.path().join("reference.docx");
    tokio::fs::write(&source_path, b"REFDOC").await.unwrap();

    // `bytes` deliberately differs from the source file's content: if
    // `materialise` fell back to `get()` instead of hard-linking, this
    // assertion below would catch it.
    let provider = PathAssets {
        key: "reference/reference.docx",
        path: Some(source_path.clone()),
        bytes: b"WRONG-IF-COPIED",
    };

    let dest_dir = tempfile::tempdir().unwrap();
    let dest_path = dest_dir.path().join("reference.docx");

    let materialised = materialise(&provider, "reference/reference.docx", &dest_path)
        .await
        .unwrap();

    assert!(materialised);
    assert_eq!(tokio::fs::read(&dest_path).await.unwrap(), b"REFDOC");
    let source_ino = tokio::fs::metadata(&source_path).await.unwrap().ino();
    let dest_ino = tokio::fs::metadata(&dest_path).await.unwrap().ino();
    assert_eq!(source_ino, dest_ino, "expected a hard link, not a copy");
}

#[tokio::test]
async fn materialise_falls_back_to_a_copy_when_the_link_target_is_missing() {
    let provider = PathAssets {
        key: "reference/reference.docx",
        path: Some(std::path::PathBuf::from("/does/not/exist/reference.docx")),
        bytes: b"COPIED",
    };
    let dest_dir = tempfile::tempdir().unwrap();
    let dest_path = dest_dir.path().join("reference.docx");

    let materialised = materialise(&provider, "reference/reference.docx", &dest_path)
        .await
        .unwrap();

    assert!(materialised);
    assert_eq!(tokio::fs::read(&dest_path).await.unwrap(), b"COPIED");
}

#[tokio::test]
async fn materialise_falls_back_to_a_copy_when_path_is_none() {
    let provider = PathAssets {
        key: "reference/reference.docx",
        path: None,
        bytes: b"COPIED",
    };
    let dest_dir = tempfile::tempdir().unwrap();
    let dest_path = dest_dir.path().join("reference.docx");

    let materialised = materialise(&provider, "reference/reference.docx", &dest_path)
        .await
        .unwrap();

    assert!(materialised);
    assert_eq!(tokio::fs::read(&dest_path).await.unwrap(), b"COPIED");
}

#[tokio::test]
async fn materialise_reports_false_when_the_provider_has_nothing() {
    let provider = PathAssets {
        key: "reference/reference.docx",
        path: None,
        bytes: b"unused",
    };
    let dest_dir = tempfile::tempdir().unwrap();
    let dest_path = dest_dir.path().join("missing.docx");

    let materialised = materialise(&provider, "reference/reference.pptx", &dest_path)
        .await
        .unwrap();

    assert!(!materialised);
    assert!(!dest_path.exists());
}
