//! Pandoc backend: docx/odt/pptx/revealjs.
//!
//! Per-page class projection differs per writer:
//!   * **docx / odt** — wrap content in a `::: {custom-style="<class>"}` div so
//!     the matching paragraph style from the reference doc is applied. Spatial
//!     layout is not supported; this is typographic projection only.
//!   * **pptx / html-reveal** — annotate each slide with `{.<class>}` on its
//!     h1. html-reveal's theme CSS picks this up directly. Pandoc's pptx
//!     writer, however, has no notion of arbitrary named layouts — it always
//!     picks one of a fixed set of content-shape-driven layouts (Title Slide,
//!     Section Header, Two Content, Comparison, Content with Caption, Blank,
//!     Title and Content) by *structure*, ignoring the class. So `.<class>`
//!     is a no-op for pptx today; `reference.pptx` still earns its keep by
//!     giving those seven built-in layouts real branding instead of pandoc's
//!     stock look. True per-class layout selection would need post-render
//!     patching of each slide's layout relationship — out of scope for the
//!     reference-doc-only v1 (see `PROJECT_PLAN.md` §10).
//!
//! Reference docs (`reference.docx`, `reference.pptx`, `reference.odt`) live
//! in the provider; we materialise them to a tempfile per invocation.

use std::path::Path;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::future::try_join_all;
use tempfile::TempDir;
use tokio::process::Command;

use crate::AssetProvider;
use crate::assets::BoxFuture;
use crate::images::resolve_images;
use crate::pages::Page;
use crate::{Backend, RenderedArtifact, ResolvedDoc, Target};

/// Pull every key beginning with `prefix` from the provider and write the bytes
/// to `dest/<key-without-prefix>`. Returns the number of files materialised.
/// Used to lay out the reveal.js dist (and could later cover MathJax, etc.).
async fn materialise_subtree(
    provider: &dyn AssetProvider,
    prefix: &str,
    dest: &Path,
) -> Result<usize> {
    let keys = provider.list(prefix).await?;
    let written = try_join_all(keys.iter().map(|key| async move {
        let Some(bytes) = provider.get(key).await? else {
            return Ok(0usize);
        };
        let rel = key.strip_prefix(prefix).unwrap_or(key);
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &bytes).await?;
        Ok::<usize, anyhow::Error>(1)
    }))
    .await?;
    Ok(written.into_iter().sum())
}

pub struct PandocBackend {
    target: Target,
}

impl PandocBackend {
    pub fn new(target: Target) -> Self {
        assert!(matches!(
            target,
            Target::Docx | Target::Odt | Target::Pptx | Target::HtmlReveal
        ));
        Self { target }
    }
}

impl Backend for PandocBackend {
    fn target(&self) -> Target {
        self.target
    }

    fn render_to_bytes<'a>(
        &'a self,
        doc: &'a ResolvedDoc,
        assets: &'a dyn AssetProvider,
    ) -> BoxFuture<'a, Result<RenderedArtifact>> {
        Box::pin(async move {
            // Owns its whole temp lifecycle: input, reference doc, revealjs
            // dist, and the pandoc output itself all live under `root` and
            // are gone the moment this function returns — nothing escapes to
            // the caller but the bytes.
            let tmp = TempDir::new().context("create temp dir for pandoc render")?;
            let root = tmp.path();

            // Resolve image references via the provider before handing the
            // markdown to pandoc. Anything the provider doesn't know about is
            // left intact so pandoc's own resolution path can have a go.
            let mut pages = doc.pages.clone();
            let assets_dir = root.join("assets");
            tokio::fs::create_dir_all(&assets_dir).await?;
            resolve_images(&mut pages, assets, &assets_dir).await?;

            let input = build_input(&pages, self.target);
            let input_path = root.join("input.md");
            tokio::fs::write(&input_path, input).await?;

            // Materialise the reference doc if this writer uses one.
            let reference = match self.target {
                Target::Docx => Some(("reference/reference.docx", "reference.docx")),
                Target::Odt => Some(("reference/reference.odt", "reference.odt")),
                Target::Pptx => Some(("reference/reference.pptx", "reference.pptx")),
                Target::HtmlReveal => None,
                _ => unreachable!(),
            };
            let reference_path = if let Some((key, name)) = reference {
                if let Some(bytes) = assets.get(key).await? {
                    let p = root.join(name);
                    tokio::fs::write(&p, &bytes).await?;
                    Some(p)
                } else {
                    tracing::warn!(
                        key,
                        "reference doc not in provider; pandoc default styling will be used"
                    );
                    None
                }
            } else {
                None
            };

            let filename = format!("output.{}", self.target.extension());
            let out_path = root.join(&filename);

            // For reveal.js: materialise any provider-supplied reveal.js
            // distribution into the temp root so pandoc inlines it via
            // --embed-resources. The default EmbeddedAssets ships a minimal
            // dist (reveal.css + reset.css + reveal.js + a few themes);
            // consumers can override by serving the same keys themselves.
            let revealjs_root = if matches!(self.target, Target::HtmlReveal) {
                let dest = root.join("revealjs");
                let materialised = materialise_subtree(assets, "revealjs/", &dest).await?;
                if materialised > 0 { Some(dest) } else { None }
            } else {
                None
            };

            let mut cmd = Command::new("pandoc");
            cmd.arg(&input_path);
            cmd.arg("-o").arg(&out_path);
            cmd.arg("--from=markdown");
            cmd.arg(format!("--to={}", pandoc_writer(self.target)));
            if matches!(self.target, Target::HtmlReveal) {
                cmd.arg("--standalone").arg("--embed-resources");
                if let Some(dir) = &revealjs_root {
                    // Trailing slash matters — pandoc joins dist/… paths to this.
                    cmd.arg(format!("-Vrevealjs-url={}/", dir.display()));
                }
            }
            if matches!(self.target, Target::Pptx | Target::HtmlReveal) {
                cmd.arg("--slide-level=1");
            }
            if let Some(p) = &reference_path {
                cmd.arg(format!("--reference-doc={}", p.display()));
            }
            cmd.args(toc_args(self.target, doc.toc));
            // Plumb document metadata so revealjs has a real <title> and
            // docx/pptx get proper document properties. Only set fields the
            // caller actually provided — pandoc inserts a synthesised title
            // slide for pptx whenever a title is present, which would collide
            // with the author's own hero page.
            if let Some(t) = &doc.meta.title {
                cmd.arg(format!("--metadata=title={t}"));
            }
            if let Some(a) = &doc.meta.author {
                cmd.arg(format!("--metadata=author={a}"));
            }
            if let Some(d) = &doc.meta.date {
                cmd.arg(format!("--metadata=date={d}"));
            }

            let status = cmd
                .status()
                .await
                .context("spawn pandoc — is the `pandoc` binary on PATH?")?;
            if !status.success() {
                bail!("pandoc failed with status {status}");
            }

            let bytes = tokio::fs::read(&out_path)
                .await
                .context("read pandoc output")?;

            Ok(RenderedArtifact {
                primary: Bytes::from(bytes),
                filename,
                extras: vec![],
            })
        })
    }
}

/// `--toc`/`--toc-depth` args, only for docx/odt — pandoc's page-based
/// writers. Slide writers (pptx/html-reveal) never get a TOC (slide decks
/// don't have one); `None` (no request) yields no args either way.
fn toc_args(target: Target, toc: Option<u8>) -> Vec<String> {
    match (target, toc) {
        (Target::Docx | Target::Odt, Some(depth)) => {
            vec!["--toc".to_string(), format!("--toc-depth={depth}")]
        }
        _ => vec![],
    }
}

fn pandoc_writer(target: Target) -> &'static str {
    match target {
        Target::Docx => "docx",
        Target::Odt => "odt",
        Target::Pptx => "pptx",
        Target::HtmlReveal => "revealjs",
        _ => unreachable!(),
    }
}

/// Build the markdown input fed to pandoc, projecting per-page class according
/// to the target's idiom.
///
/// * **docx / odt** — page-based outputs; wrap each page's body in
///   `::: {custom-style="<class>"}` so the matching paragraph style from the
///   reference doc is applied. Pages are separated by a raw-format block pandoc
///   passes straight through to the writer — `\pagebreak{}` is raw LaTeX and
///   both writers silently drop it. Docx gets an `openxml` block inserting
///   `<w:br w:type="page"/>` directly (no reference doc needed). Odt gets an
///   `opendocument` block referencing the `PageBreak` paragraph style (defined
///   in `embedded/reference/reference.odt` with `fo:break-before="page"`) —
///   ODF has no reference-doc-free way to force a break, since page-break
///   formatting can only live on a named style, not inline on the paragraph.
/// * **pptx / html-reveal** — slide-based outputs; with `--slide-level=1` only
///   an h1 starts a new slide, so we project the class *onto the h1* of each
///   page, synthesising an empty h1 when the page has none. This is the
///   pandoc-native way to put a class on a slide section.
pub fn build_input(pages: &[Page], target: Target) -> String {
    let mut s = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 {
            s.push('\n');
            match target {
                Target::Docx => {
                    s.push_str(
                        "```{=openxml}\n<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>\n```\n\n",
                    );
                }
                Target::Odt => {
                    s.push_str(
                        "```{=opendocument}\n<text:p text:style-name=\"PageBreak\"/>\n```\n\n",
                    );
                }
                _ => {}
            }
        }
        match target {
            Target::Docx | Target::Odt => {
                s.push_str(&format!("::: {{custom-style=\"{}\"}}\n", page.class));
                s.push_str(&page.body);
                if !page.body.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(":::\n");
            }
            Target::Pptx | Target::HtmlReveal => {
                s.push_str(&project_slide(&page.body, &page.class));
                if !s.ends_with('\n') {
                    s.push('\n');
                }
            }
            _ => unreachable!(),
        }
    }
    s
}

/// Attach `{.<class>}` to the first h1 of `body`, or prepend an empty
/// `# {.<class>}` if none exists. Only handles ATX-style (`#`) headings; setext
/// underlining is rare enough to defer.
fn project_slide(body: &str, class: &str) -> String {
    let trimmed = body.trim_start_matches('\n');
    let mut lines = trimmed.lines();
    let Some(first) = lines.next() else {
        return format!("# {{.{class}}}\n");
    };
    if let Some(title) = first.strip_prefix("# ") {
        // Page already has an h1: append the class. We don't try to merge with
        // an existing attribute spec — if authors hand-write `{...}` on an h1,
        // the auto-class will collide; in that case they should set the class
        // explicitly on the wrapper instead.
        let rest: String = lines.collect::<Vec<_>>().join("\n");
        let trailing_nl = if body.ends_with('\n') { "\n" } else { "" };
        if rest.is_empty() {
            format!("# {title} {{.{class}}}\n")
        } else {
            format!("# {title} {{.{class}}}\n{rest}{trailing_nl}")
        }
    } else {
        // No h1: synthesise an empty one so pandoc starts a new slide.
        let trailing_nl = if body.ends_with('\n') { "" } else { "\n" };
        format!("# {{.{class}}}\n\n{body}{trailing_nl}")
    }
}

#[cfg(test)]
mod tests {
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
}
