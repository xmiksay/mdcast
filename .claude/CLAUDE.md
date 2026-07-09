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
├─ image_format.rs    sniff() + warn_if_unsupported() — magic-byte format matrix warning (issue #55)
├─ images/
│  ├─ mod.rs           resolve_images() — async per-page image rewriter
│  └─ tests.rs         unit tests for image_refs()/collect_images()/resolve_images()
├─ preprocessor.rs    MarkdownPreprocessor trait + Identity/Chain/HtmlImageTags
├─ pages/
│  ├─ splitter.rs     PageSplitter trait + DefaultSplitter (line-based)
│  └─ auto.rs         classify() — explicit > shape > positional > default
├─ backends/
│  ├─ pandoc.rs       #[cfg(feature = "pandoc")]  docx/odt/pptx/html-reveal
│  └─ typst/          #[cfg(feature = "typst")]   pdf/pdf-presentation
│     ├─ mod.rs         TypstBackend, driver assembly, in-process compile
│     ├─ virtual_files.rs  fetch_deduped() + collect_images_for_typst()/collect_layout_assets()
│     ├─ fonts.rs       collect_fonts() — ResolvedDoc.fonts → font book bytes
│     ├─ markdown/      md_to_typst() — markdown → Typst-markup conversion
│     │  ├─ mod.rs      render_events() state machine + inline helpers
│     │  └─ table.rs    TableBuilder — `#table(...)` projection for GFM tables
│     ├─ context.rs     build_context_source() — DocMeta/BrandSpec/assets → `/context.typ`
│     └─ template.rs    TemplateDoc + render_template()/render_template_html() — user template + data → PDF/HTML, no markdown
└─ bin/mdcast/
   ├─ main.rs           CLI (render / explain / render-template), Cli/Cmd, load_doc/load_brand
   ├─ fs_assets.rs       FsAssets — filesystem-backed AssetProvider for --assets DIR
   └─ render_template.rs #[cfg(feature = "typst")] render-template subcommand plumbing; Format enum (--format pdf|html)

embedded/             rust-embed source — keys mirror these paths
├─ typst/layouts/{pdf,pdf-presentation}/{class}.typ
├─ revealjs/{dist,plugin}/…  vendored reveal.js 4.6.1 (fonts stripped)
└─ reference/         reference.{docx,odt,pptx} — real, named styles per class
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
- **`render_template`/`render_template_html` are a parallel entry point, not
  a second IR.** `backends/typst/template.rs`'s `TemplateDoc`/
  `render_template`/`render_template_html` bypass `ResolvedDoc` entirely (no
  pages, no splitter, no classifier, no `md_to_typst`) for the
  structured-data case (user typst template + JSON → PDF, or HTML behind the
  `typst-html` feature). `Registry`/`Backend` stay untouched — they're
  `Target`-keyed and markdown-shaped, and there is no `Target` variant this
  belongs under. Reuses `/context.typ` and the in-process compile machinery;
  does not reuse layouts, fonts, or `ResolvedDoc.assets` (no per-class layout
  to declare chrome for — a template reaches its own images via sibling
  files instead).

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

Day-to-day commands are wrapped in the `Makefile` — run a bare `make` to list
all targets:

```
make build              # cargo build — default = pandoc + typst
make check-all          # all cargo check feature combos (core / pandoc / typst / both / +typst-html / +remote-images)
make test                # cargo test — ~100 tests; unit suite runs in <1s
make test-typst-html     # cargo test --features typst-html (off-by-default HTML export, issue #53)
make test-remote-images  # cargo test --features remote-images (off-by-default http(s) image fetch, issue #54)
make lint                # cargo fmt --check + cargo clippy --all-targets -- -D warnings (+ typst-html, + remote-images)
make coverage            # cargo llvm-cov → lcov.info + summary (needs cargo-llvm-cov)
make verify              # lint + check-all + test + test-typst-html + test-remote-images — the pre-"done" gate
```

`tests/render_smoke.rs` drives the real engines (in-process typst, subprocess
pandoc) to a genuine artifact per `Target` — the rest of the suite is unit
tests and never invokes an engine. The pandoc-backed smoke tests skip
gracefully (not `#[ignore]`) when `pandoc` isn't on `PATH`, so `cargo test`
stays green with or without it installed locally.

`.github/workflows/ci.yml` has two jobs: `test` runs `make lint`,
`make check-all`, `make test`, `make test-typst-html`, and
`make test-remote-images` on every pull request, with `pandoc` installed so
the OOXML/revealjs smoke tests actually exercise pandoc in CI; `coverage`
runs `make coverage` (cargo-llvm-cov) on every push/merge to `master`, writes
the summary table to the Actions job summary, and uploads `lcov.info` + the
HTML report as a `coverage-report` artifact.

`.github/workflows/release.yml` publishes to crates.io on every `v*` tag
push: it checks the tag against the `Cargo.toml` version, re-runs
`make verify` (tag pushes skip the PR gate), then `cargo publish --locked`
authenticated via crates.io Trusted Publishing (OIDC,
`rust-lang/crates-io-auth-action`) — no registry token secret. Release flow:
bump the version, merge, tag `vX.Y.Z`, push the tag.

## Run

```
./target/debug/mdcast render INPUT.md --target html-reveal --out out.html [--assets DIR] [--brand brand.toml] [--toc-depth N] [--html-image-tags]
./target/debug/mdcast explain INPUT.md [--brand brand.toml] [--html-image-tags]  # prints per-page (class, origin)
./target/debug/mdcast render-template TEMPLATE --data data.json --out out.pdf [--assets DIR] [--brand brand.toml]  # typst-only, no markdown
```

`--assets DIR` layers a filesystem-backed provider over `EmbeddedAssets`.
That's the easy way to supply images referenced from markdown without writing
Rust. `--html-image-tags` enables the built-in `HtmlImageTags` preprocessor:
`<img src="X">` / `<image path="X">` become `![alt](X)` before splitting, so
the auto-classifier and both engines see real image nodes.

`render-template` is the data-driven entry point (issue #52): `TEMPLATE` is an
`AssetProvider` key for the main `.typ` file, `--data` a JSON file deserialized
into `TemplateDoc.data`. See README's "Data-driven template rendering"
section. Only present when built with the `typst` feature.

## Adding a new layout class

1. Pick a name (`two-column`).
2. Add `embedded/typst/layouts/pdf/two-column.typ` and
   `embedded/typst/layouts/pdf-presentation/two-column.typ` (export
   `#let layout(body) = …`). Optionally `#import "/context.typ": doc-meta,
   brand, asset-path, ...` to read document title/author/date/extra, brand
   palette/fonts, and declared `ResolvedDoc.assets` (logos, backgrounds) — see
   README's "Typst layout context" section for the contract. Layouts that
   skip the import are unaffected.
3. For pandoc: nothing for docx/odt/html-reveal — pandoc just emits the class
   onto the slide/div; the style is whatever the reference doc / theme CSS
   provides. For PPTX: pandoc's writer has no notion of arbitrary named
   layouts — see the PPTX bullet under Known limitations — so a new class
   gets no dedicated pptx treatment.
4. Author uses `<page class="two-column">…</page>` or `::: {.two-column}`.
5. Missing template → warn + fall back to `content`.

## Known limitations

- The md→Typst converter (`typst/markdown/mod.rs::md_to_typst`) covers a v1
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
- Image-ref rewriter (`images::image_refs`) parses `Tag::Image` via
  pulldown-cmark instead of a regex, so titled images (`![alt](url "title")`),
  angle-bracket URLs (`![alt](<url>)`), and reference-style images
  (`![alt][ref]`) all resolve through the `AssetProvider` — pulldown-cmark
  already resolves reference definitions and strips titles/angle-brackets into
  `dest_url`/`title`. `images::collect_images` is the one shared
  walk/dedup/fetch pipeline: it finds every non-remote image reference across
  a page set and fetches each unique key once via `try_join_all`. The pandoc
  path (`resolve_images`) writes the fetched bytes to a per-render temp
  directory and rewrites page bodies to point at the materialised file; the
  typst path (`backends/typst/virtual_files.rs::collect_images_for_typst`)
  keeps the bytes in memory and registers them as virtual files with the
  in-process compiler — engine-specific code only handles that last step. Both
  paths share one sanitizer, `images::sanitize_key` (`/`, `\` → `__`), for turning a
  provider key into a safe path/virtual-path segment; typst's unrelated
  `sanitize_class` (page/layout class name → import path segment, `/`, `\` →
  `_`) is kept separate since it sanitizes a different input domain. Whatever
  the original image syntax, a resolved reference is rewritten to a plain
  `![alt](local-path)` (or `(local-path "title")` if a title was present) —
  reference-style images collapse to inline once resolved, leaving their
  now-unused `[ref]: ...` definition line in place.
- Unresolved images never leak into the artifact (issue #54). An `http(s)`
  URL is never a provider key, and a missing key takes the same path: the
  typst converter (`backends/typst/markdown/mod.rs`, `Tag::Image` arm) used
  to emit literal `[image unresolved: ...]` text into the rendered document;
  it now emits a `tracing::warn!` with the URL and drops the image instead.
  With the off-by-default `remote-images` feature, `images::collect_images`
  additionally fetches every unique `http(s)` URL directly (not through the
  `AssetProvider` — a remote URL isn't a provider key) via `reqwest`, deduped
  the same way as provider keys, and folds the bytes into the same result map
  — so a remote image resolves identically for both engines: the typst path
  registers it as a virtual file same as any other image, and the pandoc path
  (`resolve_images`) materialises it to a temp file and rewrites the markdown,
  so pandoc's own subprocess never touches the network. A fetch failure (DNS,
  404, timeout, …) warns and is dropped rather than failing the render. The
  feature gates a new `reqwest` dependency (`rustls-tls`, no native-tls) —
  off by default, no new dependency and byte-identical output for documents
  without remote images.
- mdcast never validates or transcodes image bytes — the per-target format
  support matrix (issue #55, documented in README's "Image formats" section)
  is whatever the target engine/viewer accepts. `image_format.rs::sniff`
  detects a format from magic bytes only (no `image`-crate dependency — a
  ~15-arm match on binary prefixes, plus a text sniff for SVG since it's XML
  with no fixed prefix); `image_format.rs::warn_if_unsupported` is called
  from `images::collect_images` for every fetched `(key, bytes)` pair (after
  the `remote-images` fold, so a fetched remote image is checked too) and
  emits one `tracing::warn!` naming the key, detected format, and target when
  the pair is a known-bad combination (WebP→docx/pptx, BMP/TIFF/AVIF/HEIC→
  typst, AVIF/HEIC→pandoc targets, PDF-as-image→anything but typst) — never
  fails the render, since the embed may be intentional (e.g. WebP in a
  document destined only for LibreOffice). Combinations the matrix leaves
  ambiguous (TIFF in html-reveal is browser-dependent, WebP in odt needs
  LibreOffice ≥ 7.4) are deliberately left un-warned rather than guessed at.
- Typst runs **in-process** (`typst-as-lib`) — no `typst` binary is needed to
  render PDF targets. Only pandoc is an external binary dependency.
- `DocMeta` / `BrandSpec` / `ResolvedDoc.assets` reach typst layouts via a
  synthetic `/context.typ` source (`typst/context.rs::build_context_source`),
  registered alongside the per-class layouts and imported opt-in (`#import
  "/context.typ": doc-meta, brand, asset-path, ...`) — see README's "Typst
  layout context" section for the field contract and the accessor helpers
  (`doc-meta-get`, `brand-color`, `brand-font`, `asset-path`) that degrade
  missing keys to a default instead of a compile error. Pandoc's equivalent
  is `--metadata` (`backends/pandoc.rs`); the two mechanisms are unrelated
  because pandoc metadata and typst's project-root file namespace are
  different plumbing.
- `typst/virtual_files.rs::fetch_deduped` is the shared dedup-then-fetch
  primitive both `collect_layout_assets` (below) and `typst/fonts.rs`'s
  `collect_fonts` build on: given a `&[AssetRef]`, it dedups by key
  *preserving first-declaration order* (not a `BTreeMap`'s key order — order
  matters to at least one caller, see `collect_fonts` below) and fetches each
  through the `AssetProvider` concurrently via `try_join_all`, returning
  `(key, Option<Bytes>)` pairs. Callers decide what a miss means.
- `ResolvedDoc.assets: Vec<AssetRef>` declares provider-resolved chrome a
  typst layout owns directly (a logo in a running header, a cover
  background) — distinct from page-body images, which `images.rs` resolves
  by scanning markdown. `typst/virtual_files.rs::collect_layout_assets` calls
  `fetch_deduped` concurrently with `collect_images_for_typst`, registers each
  found key under `assets/<sanitized-key>` (reusing `images::sanitize_key`
  and the shared `register_virtual_files` folding helper), and folds the
  key → virtual-path mapping into `/context.typ`'s `asset-path` accessor. A
  key the provider can't resolve is dropped with a `tracing::warn!` rather
  than failing the render. Typst-only: pandoc backends ignore this field
  since they already handle body images.
- `ResolvedDoc.fonts: Vec<AssetRef>` declares brand font faces
  (`.ttf`/`.otf`) to register with the typst font book before compiling — see
  README's "Brand fonts" section. `typst/fonts.rs::collect_fonts` calls
  `fetch_deduped`, in parallel with `collect_images_for_typst`/
  `collect_layout_assets` (`typst/mod.rs`'s `try_join!`), and hands the raw
  bytes to `TypstEngine::builder().fonts(...)` — called *before*
  `.search_fonts_with(...)`, so an exact family match resolves to the
  provider-supplied font even when the same family is also
  host/embedded-discoverable (`typst-as-lib`'s builder pushes explicit
  `.fonts(...)` faces into the `FontBook` first, in `Vec` order, and
  `FontBook::select`'s variant-distance tie-break keeps the first-inserted
  candidate on an exact tie) — which is exactly why `collect_fonts` needs
  `fetch_deduped`'s order-preserving dedup rather than a key-sorted one, a
  bug the first version of this code had. A key the provider can't resolve,
  or that isn't parseable font data, is dropped (the former warns via
  `tracing::warn!`, matching `collect_layout_assets`; the latter is silently
  skipped by typst's own `Font::iter`) — either way compilation still
  succeeds and falls back to host/embedded search for that family.
  `Vec::new()` (the default) is a no-op. Typst-only: pandoc backends render
  text with whatever font the
  target document format resolves and ignore this field. Typst's own
  compile-time warnings (e.g. `unknown font family: ...`) are otherwise
  discarded by `typst-as-lib`'s `Warned<T>` wrapper, so `typst/mod.rs`
  forwards each one through `tracing::warn!` — since that happens inside the
  `spawn_blocking` compile closure (a different OS thread than the calling
  task), the closure explicitly re-installs the calling task's `tracing`
  dispatcher rather than relying on thread-local inheritance, which doesn't
  cross `spawn_blocking`.
- `ResolvedDoc.toc: Option<u8>` requests a table of contents at the given
  heading depth — see README's "Table of contents" section for the
  per-target contract. Pandoc gets `--toc --toc-depth=<n>` (docx/odt only —
  `pandoc.rs::toc_args`); typst's `pdf` target gets a leading
  `#outline(depth: <n>)` page ahead of the first real page
  (`typst/mod.rs::build_driver`). `pdf-presentation`/`pptx`/`html-reveal`
  ignore the request outright — slide decks don't get a TOC.
- `backends/typst/template.rs::{TemplateDoc, render_template}` (issue #52) is
  the data-driven entry point — see README's "Data-driven template
  rendering" section for the full contract. `data` (any
  `serde_json::Value`) is registered as a virtual `/data.json` a template
  reads with typst's native `json()`, not through `/context.typ`'s
  string-only dict (which doesn't scale to arbitrary nested JSON).
  `render_template` discovers sibling files (partials the template
  `#import`s, images it `#image`s) by calling `AssetProvider::list` scoped to
  everything up to the last `/` in `template`, and registers each at the
  same virtual path as its provider key — `.typ` siblings as typst sources,
  everything else as a binary file — so a relative reference resolves
  exactly as it would on a real filesystem. A template key with no `/` scopes
  discovery to the whole provider, which is fine against a small
  filesystem-backed one but wasteful against a large embedded catalog.
  Typst-only, deliberately does not reuse `ResolvedDoc`, `Registry`, or
  `Backend` — see the architectural-seams bullet above.
- `backends/typst/template.rs::render_template_html` (issue #53), behind the
  off-by-default `typst-html` cargo feature, is `render_template`'s HTML
  sibling — same `assemble()` (fetch template + siblings + build
  `/data.json`/`/context.typ` sources), only the export step differs
  (`typst_html::html(...)` instead of `typst_pdf::pdf(...)`). The
  `typst-html` cargo feature enables `typst-as-lib`'s own `typst-html`
  feature, which is a compile-time toggle on the whole library
  (`typst::Feature::Html`) rather than a per-`TypstEngine` option — so
  turning it on affects every `TypstEngine` built in the process, not just
  template renders, though in practice `Feature::Html` only unlocks the
  `target()` function and `html.elem`/`html.frame`, it doesn't change PDF
  output. `target()` (itself part of the HTML feature) is how a single
  template branches paged-only chrome (`#set page`, `#place`) away from
  semantic web chrome — see README's "writing a dual-target template" note.
  Warnings from HTML-unsupported constructs (e.g. `#place` — no HTML export
  rule exists for it, so typst warns "... was ignored during HTML export"
  and drops the content) surface through the same `tracing::warn!` path as
  every other typst diagnostic in this crate, not swallowed by
  `typst_html::html`'s own `SourceResult`. CLI: `mdcast render-template ...
  --format html` (`bin/mdcast/render_template.rs::Format`; the `Html`
  variant itself is `#[cfg(feature = "typst-html")]`, so clap simply
  rejects `--format html` as an unknown value when the binary wasn't built
  with the feature).

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
