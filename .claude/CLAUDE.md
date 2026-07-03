# mdcast — Claude context

Markdown → DOCX / ODT / PDF / PDF-presentation / PPTX / reveal.js HTML, with a
per-page layout system.

The strategic intent lives in [`../PROJECT_PLAN.md`](../PROJECT_PLAN.md) — read
that first if you need the *why*. This file is the operational summary.

## Layout

Single crate. Engine dependencies are gated behind Cargo features (no
sub-crates).

```
src/
├─ lib.rs             ResolvedDoc, Page, Backend, RenderRequest/RenderedArtifact, re-exports
├─ assets.rs          AssetProvider trait + EmbeddedAssets/LayeredAssets/sync_/async_
├─ brand.rs           BrandSpec + AutoLayout config
├─ images.rs          resolve_images() — async per-page image rewriter
├─ pages/
│  ├─ splitter.rs     PageSplitter trait + DefaultSplitter (line-based)
│  └─ auto.rs         classify() — explicit > shape > positional > default
├─ backends/
│  ├─ pandoc.rs       #[cfg(feature = "pandoc")]  docx/odt/pptx/html-reveal
│  └─ typst.rs        #[cfg(feature = "typst")]   pdf/pdf-presentation
└─ bin/mdcast.rs      CLI (render / explain)

embedded/             rust-embed source — keys mirror these paths
├─ typst/layouts/{pdf,pdf-presentation}/{class}.typ
├─ revealjs/{dist,plugin}/…  vendored reveal.js 4.6.1 (fonts stripped)
└─ reference/         (placeholder — real .docx/.pptx/.odt TBD)
```

## Architectural seams (don't violate)

- **`ResolvedDoc` is the IR.** Never pandoc's AST. Single load-bearing schema.
- **`Backend` trait per target.** Async (`BoxFuture`), bytes-first —
  `render_to_bytes(doc, assets) -> RenderedArtifact` is the one method every
  backend implements. Pandoc and Typst are *guests*; the core never imports
  either.
- **One render path, two ways to collect it.** `Registry::render_to_bytes`
  (in-memory, for server embedders) and `Registry::render` (path-based, used
  by the CLI) both funnel through `Backend::render_to_bytes`; the latter just
  adds `RenderedArtifact::write_to`. Typst already compiles to bytes
  in-process; pandoc's temp dir (input file, reference doc, subprocess
  output) is owned and cleaned up inside `PandocBackend::render_to_bytes` —
  nothing escapes to the caller but the bytes.
- **`AssetProvider` is the only way backends reach files.** No `std::fs` in
  backend code. Trait is `async` (boxed-future, dyn-safe). Image refs are
  resolved through it via `images::resolve_images` *before* pandoc sees the
  markdown.
- **Per-target template namespacing.** A class (`hero`) resolves to a
  *different* file per target: `typst/layouts/pdf/hero.typ` vs
  `typst/layouts/pdf-presentation/hero.typ`. Author writes the class once.
- **`PageSplitter` is a trait.** Consumers can plug in their own (e.g. for a
  different slide-marker convention or transclusion). `DefaultSplitter`
  handles `<page class="X">`, `::: {.X}`, and `---` thematic breaks.
- **Classifier rule order:** explicit class > content-shape > positional
  (first/last) > default. Content-shape predicates are a closed enum in v1
  (`SingleH1Only`, `SingleImageOnly`, `SingleBlockquoteOnly`, `Empty`).

## Asset key namespace

Flat paths, slash-separated. Not a filesystem contract — `EmbeddedAssets`
happens to mirror them.

| Prefix                                | Used by         |
|---------------------------------------|-----------------|
| `typst/layouts/{target}/{class}.typ`  | typst backend   |
| `revealjs/dist/...`, `revealjs/plugin/...` | pandoc html-reveal |
| `reference/reference.{docx,odt,pptx}` | pandoc (matching target) |
| anything else (e.g. `img/...`)        | image refs in markdown |

## Build / test

```
cargo build                              # default = pandoc + typst
cargo check --no-default-features        # core only — neither engine
cargo check --no-default-features --features pandoc
cargo check --no-default-features --features typst
cargo test                               # 33 tests; a few shell out to pandoc/typst
```

CI must verify all four `cargo check` combinations; engine binaries are not
required to build, only to render.

## Run

```
./target/debug/mdcast render INPUT.md --target html-reveal --out out.html [--assets DIR] [--brand brand.toml]
./target/debug/mdcast explain INPUT.md  # prints per-page (class, origin)
```

`--assets DIR` layers a filesystem-backed provider over `EmbeddedAssets`.
That's the easy way to supply images referenced from markdown without writing
Rust.

## Adding a new layout class

1. Pick a name (`two-column`).
2. Add `embedded/typst/layouts/pdf/two-column.typ` and
   `embedded/typst/layouts/pdf-presentation/two-column.typ` (export
   `#let layout(body) = …`).
3. For pandoc: nothing for docx/odt/html-reveal — pandoc just emits the class
   onto the slide/div; the style is whatever the reference doc / theme CSS
   provides. For PPTX: needs a layout of that name in `reference.pptx`.
4. Author uses `<page class="two-column">…</page>` or `::: {.two-column}`.
5. Missing template → warn + fall back to `content`.

## Known limitations

- Typst layouts use `#raw(body, lang: "markdown")` — body shows as markdown
  source, not typeset content. The md→Typst body renderer is the Phase-4
  trigger and is **not** in scope yet.
- DOCX/ODT honour a class as a paragraph-style name only (typographic
  projection). Spatial layout — multi-column, image positioning — would need a
  template-injection backend; documented in `PROJECT_PLAN.md` §10.
- PPTX layout-name → slide-master mapping requires a `reference.pptx` that
  defines slide masters with the expected names. The vendored placeholder
  doesn't; pandoc default styling applies until a real reference doc lands.
- `reference.docx/odt/pptx` are placeholder `.keep` files in `embedded/`. Real
  reference docs (with named paragraph styles + slide masters) are the next
  asset task.
- Image-ref rewriter uses a small regex; reference-style links, titles
  (`![alt](url "title")`), and angle-bracket URLs are not recognised in v1.
- `typst` binary not on `PATH` in this dev environment — Typst end-to-end is
  not exercised here. `cargo check --features typst` passes; render needs
  `typst` installed.

## Conventions

- **Async everywhere on the boundary.** `Backend::render_to_bytes` and
  `AssetProvider` are both async. Default impls (`EmbeddedAssets`) return
  ready futures — zero cost. Slow consumers (S3, DB, image renderer) `.await`
  freely.
- **`try_join_all` for fan-out fetches.** Multiple assets per render
  (templates + reference doc + images + reveal.js dist) resolve in parallel.
- **`anyhow` for app-level errors**, `thiserror` for typed library errors
  *only when a caller needs to match on them* (currently: none — `anyhow`
  throughout).
- **`tracing` for logs**, not `eprintln!`. The CLI initialises a `fmt`
  subscriber controlled by `RUST_LOG`.
- **Don't add error handling, fallbacks, or validation for scenarios that
  can't happen.** Per the global CLAUDE.md.

## Don't do these

- Don't write `std::fs` calls in `src/backends/*`. Go through `AssetProvider`.
- Don't reach for pandoc's AST. `ResolvedDoc` is the only IR.
- Don't split into a workspace. Use Cargo features. (User preference: see
  `feedback_crate_structure` in auto-memory.)
- Don't add a `--remove-pandoc` flag or similar. Pandoc stays for DOCX/PPTX
  indefinitely — no mature Rust OOXML alternative exists; documented in
  `PROJECT_PLAN.md` §10.
