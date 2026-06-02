//! Pandoc backend: docx/odt/pptx/revealjs.
//!
//! Per-page class projection differs per writer:
//!   * **docx / odt** — wrap content in a `::: {custom-style="<class>"}` div so
//!     the matching paragraph style from the reference doc is applied. Spatial
//!     layout is not supported; this is typographic projection only.
//!   * **pptx** — set a `slide-attributes` map per slide selecting the slide
//!     layout *name* from the reference deck (`layout="<class>"`).
//!   * **html-reveal** — annotate each slide with `{.<class>}` so the theme
//!     CSS picks up the layout.
//!
//! Reference docs (`reference.docx`, `reference.pptx`, `reference.odt`) live
//! in the provider; we materialise them to a tempfile per invocation.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tempfile::TempDir;
use tokio::process::Command;

use std::path::Path;

use crate::AssetProvider;
use crate::assets::BoxFuture;
use crate::images::resolve_images;
use crate::pages::Page;
use crate::{Artifact, Backend, RenderRequest, Target};

/// Pull every key beginning with `prefix` from the provider and write the bytes
/// to `dest/<key-without-prefix>`. Returns the number of files materialised.
/// Used to lay out the reveal.js dist (and could later cover MathJax, etc.).
async fn materialise_subtree(
    provider: &dyn AssetProvider,
    prefix: &str,
    dest: &Path,
) -> Result<usize> {
    let keys = provider.list(prefix).await?;
    let mut written = 0usize;
    for key in keys {
        let Some(bytes) = provider.get(&key).await? else { continue };
        let rel = key.strip_prefix(prefix).unwrap_or(&key);
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &bytes).await?;
        written += 1;
    }
    Ok(written)
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

    fn render<'a>(&'a self, req: &'a RenderRequest<'a>) -> BoxFuture<'a, Result<Artifact>> {
        Box::pin(async move {
            let tmp = TempDir::new().context("create temp dir for pandoc render")?;
            let root = tmp.path();

            // Resolve image references via the provider before handing the
            // markdown to pandoc. Anything the provider doesn't know about is
            // left intact so pandoc's own resolution path can have a go.
            let mut pages = req.doc.pages.clone();
            let assets_dir = root.join("assets");
            tokio::fs::create_dir_all(&assets_dir).await?;
            resolve_images(&mut pages, req.assets, &assets_dir).await?;

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
                if let Some(bytes) = req.assets.get(key).await? {
                    let p = root.join(name);
                    tokio::fs::write(&p, &bytes).await?;
                    Some(p)
                } else {
                    tracing::warn!(key, "reference doc not in provider; pandoc default styling will be used");
                    None
                }
            } else {
                None
            };

            let out_path: PathBuf = req.out.to_path_buf();
            if let Some(parent) = out_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            // For reveal.js: materialise any provider-supplied reveal.js
            // distribution into the temp root so pandoc inlines it via
            // --embed-resources. The default EmbeddedAssets ships a minimal
            // dist (reveal.css + reset.css + reveal.js + a few themes);
            // consumers can override by serving the same keys themselves.
            let revealjs_root = if matches!(self.target, Target::HtmlReveal) {
                let dest = root.join("revealjs");
                let materialised =
                    materialise_subtree(req.assets, "revealjs/", &dest).await?;
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
            // Plumb document metadata so revealjs has a real <title> and
            // docx/pptx get proper document properties. Only set fields the
            // caller actually provided — pandoc inserts a synthesised title
            // slide for pptx whenever a title is present, which would collide
            // with the author's own hero page.
            if let Some(t) = &req.doc.meta.title {
                cmd.arg(format!("--metadata=title={t}"));
            }
            if let Some(a) = &req.doc.meta.author {
                cmd.arg(format!("--metadata=author={a}"));
            }
            if let Some(d) = &req.doc.meta.date {
                cmd.arg(format!("--metadata=date={d}"));
            }

            let status = cmd
                .status()
                .await
                .context("spawn pandoc — is the `pandoc` binary on PATH?")?;
            if !status.success() {
                bail!("pandoc failed with status {status}");
            }

            Ok(Artifact { primary: out_path, extras: vec![] })
        })
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
///   reference doc is applied. Pages are separated by `\pagebreak{}` (raw
///   docx page break) so each Page becomes a real page.
/// * **pptx / html-reveal** — slide-based outputs; with `--slide-level=1` only
///   an h1 starts a new slide, so we project the class *onto the h1* of each
///   page, synthesising an empty h1 when the page has none. This is the
///   pandoc-native way to put a class on a slide section.
pub fn build_input(pages: &[Page], target: Target) -> String {
    let mut s = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 {
            s.push('\n');
            if matches!(target, Target::Docx | Target::Odt) {
                s.push_str("\\pagebreak{}\n\n");
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
        Page { class: class.into(), body: body.into(), origin: PageOrigin::Explicit }
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
    fn page_input_uses_pagebreak_for_docx() {
        let out = build_input(&[p("hero", "intro"), p("content", "body")], Target::Docx);
        assert!(out.contains(r"\pagebreak{}"));
        assert!(out.contains(r#"custom-style="hero""#));
        assert!(out.contains(r#"custom-style="content""#));
    }

    #[test]
    fn single_page_has_no_separator() {
        let out = build_input(&[p("hero", "just one")], Target::HtmlReveal);
        assert!(!out.contains(r"\pagebreak"));
        assert!(out.starts_with("# {.hero}"));
    }
}
