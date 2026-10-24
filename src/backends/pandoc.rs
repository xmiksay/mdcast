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
//!     reference-doc-only v1 (see `PROJECT_PLAN.md` §10). pptx output *does*
//!     get one post-render patch already (issue #56): `pptx_autofit::add_autofit`
//!     inserts `<a:normAutofit/>` into each body placeholder's `<a:bodyPr>` so
//!     overflowing slide text shrinks instead of spilling off the slide —
//!     autofit lives on the slide's own shape, not the layout/master, so the
//!     reference doc can't express it.
//!
//! Reference docs (`reference.docx`, `reference.pptx`, `reference.odt`) live
//! in the provider; we materialise them to a tempfile per invocation.
//!
//! html-reveal additionally gets brand projection (issue #57): `ResolvedDoc.
//! brand`'s palette/fonts are mapped onto reveal.js CSS custom properties by
//! `reveal_brand::brand_css` and injected via `--include-in-header`; an
//! optional `brand.logo` is fetched through the provider and overlaid on
//! every slide as a data-URI `<img>` (`reveal_brand::logo_html`) via
//! `--include-after-body`. See `reveal_assets::brand_style_file`/
//! `::brand_logo_file` and the README's "Branding reveal.js decks" section.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use tempfile::TempDir;
use tokio::process::Command;

use crate::AssetProvider;
use crate::assets::BoxFuture;
use crate::images::resolve_images;
use crate::process::{Bounds, run_bounded};
use crate::{Backend, RenderedArtifact, ResolvedDoc, Target};

use super::pandoc_filter::{READER, write_filter};
pub use super::pandoc_input::build_input;
use super::reveal_assets::{brand_logo_file, brand_style_file, materialise_subtree};

/// Default for [`PandocBackend::with_timeout`].
pub const DEFAULT_PANDOC_TIMEOUT: Duration = Duration::from_secs(60);

pub struct PandocBackend {
    target: Target,
    timeout: Duration,
    max_memory_bytes: Option<u64>,
}

impl PandocBackend {
    pub fn new(target: Target) -> Self {
        assert!(matches!(
            target,
            Target::Docx | Target::Odt | Target::Pptx | Target::HtmlReveal
        ));
        Self {
            target,
            timeout: DEFAULT_PANDOC_TIMEOUT,
            max_memory_bytes: None,
        }
    }

    /// Wall-clock budget for one pandoc run; exceeded → the child is killed
    /// and reaped, and the render fails.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// `RLIMIT_AS` for the pandoc child (Unix only). `None`, the default,
    /// leaves it unbounded.
    pub fn with_max_memory_bytes(mut self, bytes: Option<u64>) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    fn bounds(&self) -> Bounds {
        Bounds {
            timeout: self.timeout,
            max_memory_bytes: self.max_memory_bytes,
            max_file_bytes: None,
            // Backs up the wall-clock timeout should this process die and
            // leave the child unreaped; pandoc is effectively single-core.
            cpu_secs: Some(self.timeout.as_secs().saturating_mul(2).saturating_add(5)),
        }
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
            // markdown to pandoc; anything unresolved is dropped, and the
            // filter enforces the same on pandoc's own AST (see
            // `pandoc_filter`'s module docs for why both).
            let mut pages = doc.pages.clone();
            let assets_dir = root.join("assets");
            tokio::fs::create_dir_all(&assets_dir).await?;
            resolve_images(&mut pages, assets, &assets_dir, self.target).await?;
            let filter = write_filter(root, &assets_dir).await?;

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
                let p = root.join(name);
                if materialise(assets, key, &p).await? {
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

            // Brand-driven reveal.js styling (issue #57): a generated CSS
            // custom-property layer (palette/fonts) plus an optional
            // provider-supplied `revealjs/brand.css` escape hatch, injected
            // via `--include-in-header`; and an optional logo overlay,
            // fetched through the provider and embedded as a data URI via
            // `--include-after-body`. Both are no-ops (no files, no extra
            // pandoc args) for an unbranded doc, so unbranded output stays
            // byte-identical to before this existed.
            let style_path = if matches!(self.target, Target::HtmlReveal) {
                brand_style_file(root, &doc.brand.0, assets).await?
            } else {
                None
            };
            let logo_path = if matches!(self.target, Target::HtmlReveal) {
                brand_logo_file(root, &doc.brand.0, assets).await?
            } else {
                None
            };

            let mut cmd = Command::new("pandoc");
            // Relative paths resolve inside the temp root, never in the
            // server's own working directory.
            cmd.current_dir(root);
            cmd.arg(&input_path);
            cmd.arg("-o").arg(&out_path);
            cmd.arg(format!("--from={READER}"));
            cmd.arg(format!("--lua-filter={}", filter.display()));
            cmd.arg(format!("--to={}", pandoc_writer(self.target)));
            if matches!(self.target, Target::HtmlReveal) {
                cmd.arg("--standalone").arg("--embed-resources");
                if let Some(dir) = &revealjs_root {
                    // Trailing slash matters — pandoc joins dist/… paths to this.
                    cmd.arg(format!("-Vrevealjs-url={}/", dir.display()));
                }
                if let Some(p) = &style_path {
                    cmd.arg(format!("--include-in-header={}", p.display()));
                }
                if let Some(p) = &logo_path {
                    cmd.arg(format!("--include-after-body={}", p.display()));
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

            // The environment is cleared except PATH/TZ (`run_bounded`);
            // HOME and the XDG dirs point into the temp root, so pandoc's
            // user data directory is empty and nothing lands in the server's
            // own home.
            let home = root.join("home");
            tokio::fs::create_dir_all(&home).await?;
            let env = [
                ("HOME", home.as_os_str()),
                ("XDG_DATA_HOME", home.as_os_str()),
                ("XDG_CONFIG_HOME", home.as_os_str()),
                ("XDG_CACHE_HOME", home.as_os_str()),
                ("LANG", OsStr::new("C.UTF-8")),
            ];
            let done = run_bounded(cmd, &env, None, self.bounds(), 0, "pandoc").await?;
            if !done.status.success() {
                bail!(
                    "pandoc failed with status {}:\n{}",
                    done.status,
                    String::from_utf8_lossy(&done.stderr)
                );
            }

            let mut bytes = tokio::fs::read(&out_path)
                .await
                .context("read pandoc output")?;

            if matches!(self.target, Target::Pptx) {
                bytes = super::pptx_autofit::add_autofit(&bytes)
                    .context("patch pptx with normAutofit")?;
            }

            Ok(RenderedArtifact {
                primary: Bytes::from(bytes),
                filename,
                extras: vec![],
            })
        })
    }
}

/// Materialise `key`'s bytes at `dest`. If the provider can offer a real
/// path for `key` (a `BlobStore`-backed mirror, see `mdcast-server`),
/// hard-link it into `dest` — inode-based, so it's safe even if the
/// provider's own cache entry is evicted afterward, and it avoids copying
/// potentially large bytes pandoc will read once and discard. Falls back to
/// fetching bytes and writing them on any link error (e.g. `EXDEV` when the
/// provider's path and `dest` don't share a filesystem) or when the
/// provider has no path to offer at all. Returns `false` only when the
/// provider has nothing for `key` under either path.
async fn materialise(assets: &dyn AssetProvider, key: &str, dest: &Path) -> Result<bool> {
    if let Some(source) = assets.path(key).await? {
        match hard_link(&source, dest).await {
            Ok(()) => return Ok(true),
            Err(e) => {
                tracing::warn!(key, error = %e, "hard-linking asset failed; falling back to a copy")
            }
        }
    }
    match assets.get(key).await? {
        Some(bytes) => {
            tokio::fs::write(dest, &bytes).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

async fn hard_link(source: &Path, dest: &Path) -> std::io::Result<()> {
    let source = source.to_path_buf();
    let dest = dest.to_path_buf();
    match tokio::task::spawn_blocking(move || std::fs::hard_link(&source, &dest)).await {
        Ok(result) => result,
        Err(join_err) => Err(std::io::Error::other(join_err)),
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

#[cfg(test)]
#[path = "pandoc_tests.rs"]
mod tests;
