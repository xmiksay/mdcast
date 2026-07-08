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
├─ frontmatter.rs     extract() — strips leading YAML frontmatter into DocMeta
├─ images.rs          resolve_images() — async per-page image rewriter
├─ preprocessor.rs    MarkdownPreprocessor trait + Identity/Chain/HtmlImageTags
├─ pages/
│  ├─ splitter.rs     PageSplitter trait + DefaultSplitter (line-based)
│  └─ auto.rs         classify() — explicit > shape > positional > default
├─ backends/
│  ├─ pandoc.rs       #[cfg(feature = "pandoc")]  docx/odt/pptx/html-reveal
│  └─ typst/          #[cfg(feature = "typst")]   pdf/pdf-presentation
│     ├─ mod.rs       TypstBackend, driver assembly, in-process compile
│     ├─ markdown.rs  md_to_typst() — markdown → Typst-markup conversion
│     └─ context.rs   build_context_source() — DocMeta/BrandSpec → `/context.typ`
└─ bin/mdcast.rs      CLI (render / explain)

embedded/             rust-embed source — keys mirror these paths
├─ typst/layouts/{pdf,pdf-presentation}/{class}.typ
├─ revealjs/{dist,plugin}/…  vendored reveal.js 4.6.1 (fonts stripped)
└─ reference/         reference.odt (real, minimal); .docx/.pptx TBD
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
cargo test                               # 40 tests; unit suite runs in <1s
```

`tests/render_smoke.rs` drives the real engines (in-process typst, subprocess
pandoc) to a genuine artifact per `Target` — the rest of the suite is unit
tests and never invokes an engine. The pandoc-backed smoke tests skip
gracefully (not `#[ignore]`) when `pandoc` isn't on `PATH`, so `cargo test`
stays green with or without it installed locally.

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy --all-targets
-- -D warnings`, all four `cargo check` combinations, and `cargo test` on
every push/PR to `master`, with `pandoc` installed in the job so the
OOXML/revealjs smoke tests actually exercise pandoc in CI.

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
   `#let layout(body) = …`). Optionally `#import "/context.typ": doc-meta,
   brand, ...` to read document title/author/date/extra and brand
   palette/fonts — see README's "Typst layout context" section for the
   contract. Layouts that skip the import are unaffected.
3. For pandoc: nothing for docx/odt/html-reveal — pandoc just emits the class
   onto the slide/div; the style is whatever the reference doc / theme CSS
   provides. For PPTX: pandoc's writer has no notion of arbitrary named
   layouts — see the PPTX bullet under Known limitations — so a new class
   gets no dedicated pptx treatment.
4. Author uses `<page class="two-column">…</page>` or `::: {.two-column}`.
5. Missing template → warn + fall back to `content`.

## Known limitations

- The md→Typst converter (`typst/markdown.rs::md_to_typst`) covers a v1
  subset: headings, paragraphs, emphasis/strong, lists, blockquotes, images,
  inline code, code blocks, tables, links (inline and reference-style),
  autolinks (`<https://…>`, `<user@host>`), and footnotes. Raw HTML blocks are
  dropped (their text content still comes through). Layouts receive the
  converted body as a string and typeset it via `#eval(body, mode:
  "markup")`. Code block text is passed through verbatim (not escaped) inside
  the fence, with the fence language forwarded from the markdown info string;
  inline code renders via `#raw(...)` (a function call, not the backtick
  shorthand) so embedded backticks can't break out of it. Tables project to a
  structural `#table(columns:, align:, table.header(...), ...)` call — column
  count and per-column alignment come from the GFM alignment row, header
  cells are wrapped in `table.header(...)`, ragged rows are padded/truncated
  to the column count, and cell text runs through the same inline conversion
  and escaping as paragraph text (`[`/`]` are additionally escaped there,
  since a cell's content sits inside a `[...]` literal). Styling is
  deliberately left to the layout: a `#show table: ...` rule set before
  `#eval(...)` in the same layout scope applies to the table content that
  call produces, since show rules attach at realization time, not at
  content-construction time. Links (`Tag::Link`, any `LinkType` — inline,
  reference, autolink, or email) render as `#link("<url>")[<inline text>]`;
  email autolinks get a `mailto:` prefix added (pulldown-cmark's own HTML
  writer does the same — the parser itself leaves the bare address in
  `dest_url`). Footnotes need two passes over the same parsed event list: the
  first only harvests `label -> rendered body` from each
  `Tag::FootnoteDefinition` (definitions commonly follow their reference site
  in document order, so they can't be resolved in one forward pass); the
  second does the real render, expanding each `FootnoteReference` inline as
  `#footnote[...]` and dropping the (now-redundant) definition sites.
  Requires `Options::ENABLE_FOOTNOTES` on the pulldown parser. Prose text runs
  escape `_` and `*` (along with `#`, `@`, `<`, `>`, `$`, `\`, `[`, `]`) so
  literal identifiers like `snake_case_name` don't pick up phantom emphasis —
  this is safe because the emphasis/strong markers the converter itself
  emits go straight through `push_char`, never through the text-escaping
  path.
- DOCX/ODT honour a class as a paragraph-style name only (typographic
  projection). Spatial layout — multi-column, image positioning — would need a
  template-injection backend; documented in `PROJECT_PLAN.md` §10.
  `reference.docx`/`reference.odt` each define real named paragraph styles for
  the six built-in classes (`hero`, `content`, `thanks`, `image-full`,
  `section-divider`, `callout`), so `custom-style` projection actually renders
  distinct styling instead of falling back to pandoc's defaults.
  `reference.odt` also carries the `PageBreak` style
  (`fo:break-before="page"`) that page separators reference.
- PPTX has no reference-doc mechanism for arbitrary named layouts: pandoc's
  writer always picks one of seven fixed, content-shape-driven layouts
  (`Title Slide`, `Section Header`, `Two Content`, `Comparison`,
  `Content with Caption`, `Blank`, `Title and Content`) — see [PowerPoint
  layout choice](https://pandoc.org/MANUAL.html#powerpoint-layout-choice).
  Our per-page `{.<class>}` heading attribute is a no-op for pptx (it's only
  meaningful for html-reveal's theme CSS). `reference.pptx` still gives real
  branding (accent color + title styling) to those seven layouts instead of
  pandoc's stock look, but true per-class layout selection would require
  post-render patching of each slide's layout relationship — out of scope for
  v1 (`PROJECT_PLAN.md` §10).
- Image-ref rewriter uses a small regex; reference-style links, titles
  (`![alt](url "title")`), and angle-bracket URLs are not recognised in v1.
- Typst runs **in-process** (`typst-as-lib`) — no `typst` binary is needed to
  render PDF targets. Only pandoc is an external binary dependency.
- `DocMeta` / `BrandSpec` reach typst layouts via a synthetic `/context.typ`
  source (`typst/context.rs::build_context_source`), registered alongside the
  per-class layouts and imported opt-in (`#import "/context.typ": doc-meta,
  brand, ...`) — see README's "Typst layout context" section for the field
  contract and the accessor helpers (`doc-meta-get`, `brand-color`,
  `brand-font`) that degrade missing keys to a default instead of a compile
  error. Pandoc's equivalent is `--metadata` (`backends/pandoc.rs`); the two
  mechanisms are unrelated because pandoc metadata and typst's project-root
  file namespace are different plumbing.
- `ResolvedDoc.toc: Option<u8>` requests a table of contents at the given
  heading depth — see README's "Table of contents" section for the
  per-target contract. Pandoc gets `--toc --toc-depth=<n>` (docx/odt only —
  `pandoc.rs::toc_args`); typst's `pdf` target gets a leading
  `#outline(depth: <n>)` page ahead of the first real page
  (`typst/mod.rs::build_driver`). `pdf-presentation`/`pptx`/`html-reveal`
  ignore the request outright — slide decks don't get a TOC.

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
