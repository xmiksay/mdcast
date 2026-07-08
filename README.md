# mdcast

Markdown → **DOCX · ODT · PDF · PDF-presentation · PPTX · reveal.js HTML**, in
one async Rust library and a thin CLI on top of it.

The pitch:

- **One markdown source, six outputs.** Write once, render to whatever the
  audience reads.
- **Per-page layout system.** Tag a page `hero`, `image-full`, `callout`,
  `thanks` — and have it honoured across every output format.
- **Pluggable everything.** Templates, images, reveal.js distribution — all
  fetched through one async `AssetProvider` trait. Your app feeds bytes from
  a DB, S3, an in-memory map, whatever.
- **Single self-contained HTML.** Reveal.js dist is bundled; with
  `--embed-resources` (default) the deck is one file with zero external URLs.

mdcast does **not** try to replace pandoc. Pandoc handles DOCX/PPTX/revealjs
because no Rust crate matches its OOXML fidelity. Typst handles PDF because
the LaTeX toolchain is slow and heavy. The value mdcast adds is the
**branding-and-layout layer** that sits on top of both.

## Quick start

PDF targets need nothing extra — the Typst compiler is embedded in the
library. Only the pandoc-backed targets (docx/odt/pptx/html-reveal) need the
`pandoc` binary:

```sh
yay -S pandoc        # arch
brew install pandoc  # macos
apt install pandoc   # debian/ubuntu
```

Build and render:

```sh
cargo build --release
./target/release/mdcast render slides.md \
    --target html-reveal \
    --out slides.html \
    --assets ./my-images/
```

You'll get a single self-contained `slides.html` you can open in any browser.

## A minimal markdown example

```markdown
<page class="hero">
# Q3 Operations Review

*F13 — for board discussion*
</page>

---

# Agenda

- Headlines
- Margins
- Open questions

---

# {.image-full}
![](charts/revenue.svg)

---

> A simple plan, decisively executed, beats a perfect plan that ships late.

---

Closing remarks and next steps.
```

What you get with no extra config:

| Page | Class           | Why                                                     |
|------|-----------------|---------------------------------------------------------|
| 1    | `hero`          | Explicit `<page class="hero">` wrapper                  |
| 2    | `content`       | Default (no rule matched)                               |
| 3    | `image-full`    | Page body is just one image → shape rule                |
| 4    | `callout`       | Body is just a blockquote → shape rule                  |
| 5    | `thanks`        | Last page, no explicit class → positional rule          |

Run `mdcast explain slides.md` to print this table for any file.

## Frontmatter

A leading YAML block is stripped before the page splitter runs, so it never
becomes a phantom `hero` page:

```markdown
---
title: Q3 Operations Review
author: F13
date: 2026-07-03
---

# Real first page
```

Only a flat `key: value` subset is parsed — `title`/`author`/`date` populate
`DocMeta`, any other keys land in `DocMeta::extra`. Pandoc targets pass
`title`/`author`/`date` through as `--metadata` (revealjs `<title>`,
docx/pptx document properties). No frontmatter block → `DocMeta` stays
default, same as before.

## Page boundaries and classes

Two surface syntaxes, both accepted:

- HTML-style: `<page class="hero">…</page>`
- Pandoc fenced div: `::: {.hero}` … `:::`

Outside an explicit wrapper, **`---` thematic breaks split pages.** The
auto-classifier then fills in a class:

1. **Explicit class** (from a wrapper) — always wins.
2. **Content shape** — `single_h1_only` → `section-divider`,
   `single_image_only` → `image-full`, `single_blockquote_only` → `callout`.
3. **Positional** — first page → `hero`, last page → `thanks`.
4. **Default** — `content`.

All rules live in `brand.toml`:

```toml
[auto_layout]
first   = "hero"
last    = "thanks"
default = "content"

[[auto_layout.rules]]
when  = "single_h1_only"
class = "section-divider"

[[auto_layout.rules]]
when  = "single_image_only"
class = "image-full"
```

## Built-in classes

| Class             | Where it shows up                                    |
|-------------------|------------------------------------------------------|
| `hero`            | Title / cover                                        |
| `content`         | Body pages — paragraphs, lists, the usual            |
| `thanks`          | Closing                                              |
| `image-full`      | Full-bleed image                                     |
| `section-divider` | Single-heading section break                         |
| `callout`         | Pull-quote / emphasised single block                 |

A class name resolves to a *different template per target*. The same
`<page class="hero">` produces:

- a centred large-type cover **in PDF** (via `typst/layouts/pdf/hero.typ`)
- a dark-background title slide **in PDF-presentation** (via
  `typst/layouts/pdf-presentation/hero.typ`)
- a `<section class="hero">` **in reveal.js** (styled by the theme CSS)
- a `Hero` paragraph-style **in DOCX/ODT** (from the reference doc)

Missing template for some class? The renderer logs a warning and falls back
to `content`. Authors are never blocked.

## Typst layout context: `doc-meta` / `brand` / `asset-path`

Every typst render (`pdf`, `pdf-presentation`) registers a synthetic
`/context.typ` source alongside the per-class layouts, built from
`ResolvedDoc.meta` (`DocMeta`), `.brand` (`BrandSpec`), and `.assets`
(`Vec<AssetRef>`). A layout opts in with an `#import` — layouts that don't
import it are completely unaffected, so third-party `.typ` files with the
plain `layout(body)` signature keep working with no changes:

```typst
#import "/context.typ": doc-meta, brand, doc-meta-get, brand-color, brand-font, asset-path

#let layout(body) = [
  #doc-meta.title           // "" if frontmatter set no title
  #doc-meta.author
  #doc-meta.date
  #doc-meta.classification  // any DocMeta.extra key, flattened onto doc-meta
  #brand.name
  #brand.palette.navy       // "" — a raw hex string like "#243752", not a color
  #brand.fonts.sans

  // Safe accessors — missing keys degrade to the given default instead of
  // erroring, which matters for `extra`/`palette`/`fonts` since those come
  // from the frontmatter/brand.toml a given document happens to set:
  #doc-meta-get("classification", default: "internal")
  #text(fill: brand-color("navy", default: black))[...]
  #set text(font: brand-font("sans", default: "New Computer Modern"))

  // A provider-supplied asset (logo, background) the layout owns directly —
  // not something referenced from the markdown body. `asset-path` degrades
  // to `none` for a key that was never declared or that the provider
  // couldn't resolve, so wrap it in an `if` rather than calling `#image`
  // unconditionally.
  #let logo = asset-path("branding/logo.svg")
  #if logo != none [#image(logo, width: 3cm)]
]
```

- `doc-meta.title` / `.author` / `.date` are always present (empty string if
  unset). Every other `DocMeta.extra` key (`subtitle`, `classification`, …)
  is flattened onto the same dict, so `"classification" in doc-meta` /
  `doc-meta.at("classification", default: "")` both work.
- `brand.name`, `brand.palette` (name → hex string), and `brand.fonts` (name
  → family string) mirror `BrandSpec` from `brand.toml` as-is — `rgb(...)`
  the palette values yourself, or use `brand-color(key, default: ...)`, which
  does that for you and returns `default` for a missing key.
- The built-in `hero` and `content` layouts (both targets) already use this:
  `hero` centres `doc-meta.author`/`.date` under the title and themes it via
  `brand-color("accent", ...)` / `brand-font("sans", ...)`; `content` renders
  a running header with `doc-meta.title` and a `classification` extra key
  when either is set. A document with no frontmatter and no `brand.toml`
  renders identically to before this existed — every accessor's default
  reproduces the prior hardcoded value.
- `asset-path(key, default: none)` resolves a `ResolvedDoc.assets` entry —
  each `AssetRef { key }` is fetched through the same `AssetProvider` as
  everything else and registered as a virtual file, so a layout reaches it
  by the exact key the document declared (`"branding/logo.svg"`, not a
  sanitized internal path). Both SVG and raster images work — `#image(...)`
  doesn't care which. A key the provider has no bytes for is dropped with a
  `tracing::warn!` instead of failing the render, so a missing logo degrades
  to whatever `default:` the layout passes rather than breaking the PDF.
  This is layout chrome, distinct from `images.rs`'s page-body image
  resolution: an asset here isn't referenced from markdown, and pandoc
  targets ignore `ResolvedDoc.assets` entirely.

## Table of contents

`ResolvedDoc.toc: Option<u8>` requests a table of contents at the given
heading depth (1-6). `None` (the default) means no TOC — output is
byte-identical to before this field existed. Each backend honours it in its
own idiom:

| Target                          | Behaviour with `toc: Some(n)`                          |
|----------------------------------|--------------------------------------------------------|
| `docx`, `odt`                    | `pandoc --toc --toc-depth=<n>` — a real TOC field/element |
| `pdf`                            | A leading `#outline(depth: <n>)` page, before page 1    |
| `pdf-presentation`, `pptx`, `html-reveal` | Ignored — slide decks don't get a TOC          |

```rust
let doc = ResolvedDoc { toc: Some(3), /* .. */ };
```

The typst `pdf` outline only lists headings that survive md→typst conversion
(see `.claude/CLAUDE.md`'s Known limitations for the converter's coverage) —
a document with no headings renders an empty outline page.

## Library usage

```rust
use std::path::Path;
use std::sync::Arc;

use mdcast::backends::Registry;
use mdcast::pages::{auto::classify, splitter::DefaultSplitter};
use mdcast::{
    AssetRef, BrandHandle, BrandSpec, DocMeta, EmbeddedAssets, LayeredAssets,
    PageSplitter, RenderRequest, ResolvedDoc, Target, async_provider,
};
use bytes::Bytes;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Parse → pages → classify.
    let md = tokio::fs::read_to_string("slides.md").await?;
    let raw = DefaultSplitter.split(&md);
    let brand = BrandSpec::default();
    let pages = classify(raw, &brand.auto_layout);

    let doc = ResolvedDoc {
        pages,
        meta: DocMeta { title: Some("Q3 Review".into()), ..Default::default() },
        brand: BrandHandle(Arc::new(brand)),
        // Declare a brand logo typst layouts can reach via `asset-path("branding/logo.svg")`
        // (see "Typst layout context" above). `Vec::new()` if no layout needs one.
        assets: vec![AssetRef { key: "branding/logo.svg".into() }],
        toc: None, // Some(3) to request a 3-level-deep table of contents
    };

    // 2. Compose an asset provider — fetch images from your app, fall back to
    //    the built-in templates and reveal.js dist.
    let app_provider = async_provider(|key: String| async move {
        // your code: hit a DB, S3, an in-memory cache, an image renderer …
        if let Some(bytes) = your_app_lookup(&key).await? {
            Ok::<_, anyhow::Error>(Some(Bytes::from(bytes)))
        } else {
            Ok(None)
        }
    });
    let provider = LayeredAssets { over: app_provider, base: EmbeddedAssets };

    // 3. Render.
    let registry = Registry::with_defaults();
    let req = RenderRequest {
        doc: &doc,
        assets: &provider,
        out: Path::new("slides.html"),
    };
    let artifact = registry.render(Target::HtmlReveal, &req).await?;
    println!("wrote {}", artifact.primary.display());
    Ok(())
}

# async fn your_app_lookup(_: &str) -> Result<Option<Vec<u8>>> { Ok(None) }
```

### Server embedding: render straight to bytes

A server handling a render request doesn't want a file on disk — it wants
bytes to put in a response body. `Registry::render_to_bytes` skips the
temp-dir dance entirely: Typst PDFs are already produced in memory, and the
pandoc temp lifecycle (input file, reference doc, subprocess output) is owned
internally and cleaned up before the call returns.

```rust
let registry = Registry::with_defaults();
let artifact = registry.render_to_bytes(Target::HtmlReveal, &doc, &provider).await?;
// artifact.primary: Bytes, artifact.filename: "output.html"
respond_with(artifact.filename, artifact.primary);
```

`RenderRequest`/`registry.render(...)` (the path-based API used above) is
implemented on top of this — one render path, two ways to collect the
result.

Anything the provider returns `None` for falls through to the next layer.
`EmbeddedAssets` is always at the bottom and ships:

- Built-in Typst layouts (`hero`, `content`, `thanks`, `image-full`,
  `section-divider`, `callout`) for `pdf` and `pdf-presentation`.
- Minimal reveal.js 4.6.1 distribution (with stripped font imports — falls
  back to system sans-serif).
- Real, branded pandoc reference docs: `reference.docx`/`reference.odt`
  define named paragraph styles for the six built-in classes (`hero`,
  `content`, `thanks`, `image-full`, `section-divider`, `callout`), plus
  `PageBreak` (odt) for page separators; `reference.pptx` brands pandoc's
  seven built-in content-shape layouts (pptx has no per-class layout
  selection — see `.claude/CLAUDE.md`'s Known limitations).

## Cargo features

```toml
[features]
default = ["pandoc", "typst"]
pandoc  = []   # DOCX, ODT, PPTX, html-reveal
typst   = []   # PDF, PDF-presentation
```

Build with only what you need:

```sh
cargo build --no-default-features --features pandoc   # no typst dep tree
cargo build --no-default-features --features typst    # no pandoc backend
```

## Targets

| Target              | Engine     | Notes                                                          |
|---------------------|------------|----------------------------------------------------------------|
| `docx`              | pandoc     | Class = paragraph-style name in `reference.docx`               |
| `odt`               | pandoc     | Class = paragraph-style name in `reference.odt`                |
| `pptx`              | pandoc     | Class = slide-layout name in `reference.pptx`                  |
| `html-reveal`       | pandoc     | Single self-contained file; reveal.js dist bundled & inlined   |
| `pdf`               | typst      | Per-class typst template under `typst/layouts/pdf/`            |
| `pdf-presentation`  | typst      | Per-class typst template under `typst/layouts/pdf-presentation/` |

## CLI

```
mdcast render INPUT.md --target <T> --out OUTPUT [--assets DIR] [--brand brand.toml] [--toc-depth N] [--html-image-tags] [--layout-asset KEY]...
mdcast explain INPUT.md [--brand brand.toml] [--html-image-tags]
```

Targets: `docx`, `odt`, `pdf`, `pdf-presentation`, `pptx`, `html-reveal`.

`--html-image-tags` enables the built-in `HtmlImageTags` preprocessor:
`<img src="X" alt="A">` / `<image path="X">` HTML tags are rewritten to
standard `![A](X)` markdown before page splitting, so the auto-classifier and
both engines see real image nodes.

`--layout-asset KEY` (repeatable) declares a `ResolvedDoc.assets` entry —
resolved through `--assets`/the embedded provider and reachable from a typst
layout via `asset-path(KEY)` (see "Typst layout context" above). Ignored by
pandoc targets.

## Development

All day-to-day commands are wrapped in the `Makefile` — run a bare `make` to
list them:

| Target                  | What it does                                                    |
|-------------------------|-----------------------------------------------------------------|
| `make build` / `release`| Debug / release build (default features = pandoc + typst)      |
| `make check`            | Fast typecheck (default features)                               |
| `make check-all`        | All four feature combinations (core, pandoc, typst, both)      |
| `make fmt` / `lint`     | Apply formatting / fmt-check + clippy with `-D warnings`       |
| `make test`             | Full suite (unit + integration)                                 |
| `make test-unit`        | In-module `#[cfg(test)]` tests only                             |
| `make test-integration` | `tests/` suite, incl. engine smoke tests (pandoc-backed ones skip when `pandoc` is absent) |
| `make coverage`         | Coverage report: `lcov.info` + terminal summary (needs [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov)); CI runs it on every merge to `master` |
| `make verify`           | Pre-merge gate: `lint` + `check-all` + `test` — what CI runs on every PR |
| `make demo`             | Render the golden fixture to `target/demo/` (html-reveal + pdf) |

`CARGO_BUILD_JOBS` defaults to 4; override with `make build CARGO_BUILD_JOBS=8`.

## What's deferred

These are not bugs — they're chosen scope cuts. Each lands as an additive
change at a seam that already exists (see [`PROJECT_PLAN.md` §10](https://github.com/xmiksay/mdcast/blob/master/PROJECT_PLAN.md#10-future-evolution--with-explicit-triggers) on GitHub).

- PPTX per-class layout selection: pandoc's writer only ever picks from
  seven fixed, content-shape-driven layouts, never by our page class, so
  `reference.pptx` brands those seven instead of adding one layout per class.
  True per-class selection needs post-render patching of each slide's layout
  relationship.
- Full markdown coverage in the md→Typst converter (v1 handles headings,
  emphasis, lists, blockquotes, images, code, tables, links, autolinks, and
  footnotes; raw HTML blocks are not yet projected — their text comes through
  unstyled).
- Mermaid → SVG pre-processing (a Rust renderer the team already owns will
  plug in as a pre-step).
- Brand projection (one `brand.toml` colour change → propagated to all
  outputs).
- Caching (content-hashed diagram + output cache).

## License

MIT OR Apache-2.0
