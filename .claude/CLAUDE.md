# mdcast — Claude context

Markdown → DOCX / ODT / PDF / PDF-presentation / PPTX / reveal.js HTML, with a
per-page layout system.

The strategic intent lives in [`../PROJECT_PLAN.md`](../PROJECT_PLAN.md) — read
that first if you need the *why*. This file is the operational summary.

## Layout

Cargo workspace with three members. The root `Cargo.toml` is **both** the
`[workspace]` root and the `mdcast` `[package]` — `src/`, `tests/`,
`embedded/` and `examples/` never moved. Shared metadata (version, edition,
license, authors, repository, homepage) lives in `[workspace.package]` and
all three crates inherit it, so they release in lockstep.

Engine dependencies stay gated behind Cargo features inside `mdcast` — the
split is drawn at *data*, not at engines. `mdcast-api` exists so
`mdcast-client` can speak the HTTP protocol without compiling `rust-embed`
(the whole vendored `embedded/` tree via proc macro), `clap`, `toml`,
`pulldown-cmark`, `tempfile` and a fat `tokio` for a few structs. See
`PROJECT_PLAN.md` §7.1.

```
mdcast-api/           serde-only shared types + wire protocol; no anyhow/tokio/IO
├─ src/lib.rs         module wiring + flat re-exports
├─ src/doc.rs         Target, DocMeta
├─ src/brand.rs       BrandSpec, LogoSpec, LogoPosition, AutoLayout, AutoRule, ShapePredicate
├─ src/digest.rs      Digest (validated lowercase-hex sha256) + DigestError
├─ src/manifest.rs    AssetManifest — asset key → Digest
└─ src/wire/
   ├─ mod.rs          protocol overview (manifest → 409 → upload → retry)
   ├─ render.rs       RenderMarkdownRequest/RenderOptions, RenderTemplateRequest/TemplateFormat
   ├─ blob.rs         BlobCheckRequest/Response, MissingBlob, MissingBlobsError (the 409 body)
   ├─ generated.rs    GeneratedAsset/GeneratedSpec (non-exhaustive, `kind`-tagged), QrEcLevel, Generator (capabilities)
   └─ error.rs        ErrorResponse + ErrorCode (every other 4xx/5xx: bad_request,
                      unauthorized, payload_too_large, unsupported_target,
                      render_failed, internal)

mdcast-client/        async client for mdcast-server; mdcast-api + reqwest, NEVER mdcast
├─ src/lib.rs         crate docs + module wiring + flat re-exports
├─ src/client.rs      Client/ClientBuilder, render()/render_template(), the 409→upload→retry-once flow
├─ src/blobs.rs       check()/upload()/preflight() + upload-set selection (impl Client, split for file size)
├─ src/bundle.rs      AssetBundle (key → bytes, or key → digest with a lazy fetch) + sha256() + manifest()/bind()/resolve()
├─ src/artifact.rs    Artifact (bytes/filename/content_type) + Content-Disposition parsing
├─ src/request.rs     request::markdown()/template() — constructors for the wire bodies
├─ src/error.rs       Error (thiserror) + from_status() status/body → typed error
└─ tests/negotiation.rs  wiremock-backed HTTP flow tests (dev-dependency only)

src/
├─ lib.rs             ResolvedDoc, Page, Backend, RenderRequest/RenderedArtifact, re-exports (incl. mdcast_api's Target/DocMeta)
├─ assets.rs          AssetProvider trait + EmbeddedAssets/LayeredAssets/MemoryAssets/sync_/async_
├─ brand.rs           re-export of mdcast_api::brand + from_toml() (the anyhow/toml loader)
├─ frontmatter.rs     extract() — strips leading YAML frontmatter into DocMeta
├─ image_format.rs    sniff() + mime_type() + warn_if_unsupported() — magic-byte format matrix warning (issue #55)
├─ images/
│  ├─ mod.rs           resolve_images() — async per-page image rewriter
│  └─ tests.rs         unit tests for image_refs()/collect_images()/resolve_images()
├─ mermaid.rs         #[cfg(feature = "mermaid")] render_diagrams() — ```mermaid fences → SVG via mermaid-svg (pure Rust), pre-splitter
├─ generators/        #[cfg(feature = "qr")] images mdcast draws itself
│  ├─ mod.rs          expand() — shared fence/tag scanner (```name / <name>…</name> / <name path="KEY">), async path= resolution, splice; render_spec() for template `generated` entries; available()
│  ├─ qr.rs           render_qr_codes() + svg() — hand-built SVG from the qrcode module matrix
│  └─ tests.rs        unit tests for the scanner and the QR renderer
├─ preprocessor.rs    MarkdownPreprocessor trait + Identity/Chain/HtmlImageTags
├─ pages/
│  ├─ splitter.rs     PageSplitter trait + DefaultSplitter (line-based)
│  └─ auto.rs         classify() — explicit > shape > positional > default
├─ backends/
│  ├─ pandoc.rs       #[cfg(feature = "pandoc")]  docx/odt/pptx/html-reveal
│  ├─ pptx_autofit.rs #[cfg(feature = "pandoc")]  add_autofit() — post-render normAutofit patch (issue #56)
│  ├─ reveal_assets.rs #[cfg(feature = "pandoc")] materialise_subtree()/brand_style_file()/brand_logo_file() — html-reveal temp-root materialisation
│  ├─ reveal_brand.rs #[cfg(feature = "pandoc")]  brand_css()/logo_html() — brand → reveal.js CSS/HTML projection (issue #57), pure string-building
│  └─ typst/          #[cfg(feature = "typst")]   pdf/pdf-presentation
│     ├─ mod.rs         TypstBackend, driver assembly, builds CompileInputs, hands off to compiler::TypstCompiler
│     ├─ compiler/      the compiler seam (issue #73) — selectable in-process/subprocess
│     │  ├─ mod.rs      CompileInputs/CompileFormat, TypstCompiler enum (Library default / Cli)
│     │  ├─ library.rs  TypstCompiler::Library — today's in-process TypstEngine compile, unified over CompileInputs
│     │  ├─ cli.rs      TypstCompiler::Cli/CliCompiler — temp-dir materialisation + `typst compile` subprocess, timeout+kill, safe_join path-traversal guard, optional RLIMIT_AS cap
│     │  └─ cli_tests.rs  real-`typst`-binary tests (pdf/html/error/timeout+reap), skip gracefully without one on PATH
│     ├─ virtual_files.rs  fetch_deduped() + collect_images_for_typst()/collect_layout_assets()
│     ├─ fonts.rs       collect_fonts() — ResolvedDoc.fonts → font book bytes
│     ├─ markdown/      md_to_typst() — markdown → Typst-markup conversion
│     │  ├─ mod.rs      render_events() state machine + inline helpers
│     │  ├─ tests.rs    unit tests for md_to_typst()
│     │  └─ table.rs    TableBuilder — `#table(...)` projection for GFM tables
│     ├─ context.rs     build_context_source() — DocMeta/BrandSpec/assets → `/context.typ`
│     └─ template.rs    TemplateDoc + render_template()/render_template_html() (Library-default) and render_template_with()/render_template_html_with() (explicit TypstCompiler) — user template + data → PDF/HTML, no markdown
├─ bin/mdcast/
│  ├─ main.rs           CLI (render / explain / render-template), Cli/Cmd, provider stack
│  ├─ load_doc.rs        load_doc()/load_brand() + PreSteps (--html-image-tags/--mermaid/--qr, flattened onto render and explain)
│  ├─ fs_assets.rs       FsAssets — filesystem-backed AssetProvider for --assets DIR
│  ├─ render_template.rs #[cfg(feature = "typst")] render-template subcommand: RenderTemplateArgs, Format enum (--format pdf|html), --generated FILE.json
│  └─ typst_compiler_args.rs #[cfg(feature = "typst")] TypstCompilerArgs — --typst-compiler/--typst-bin/--typst-timeout-secs, `#[command(flatten)]`d onto both `render` and `render-template` (issue #73)
└─ bin/mdcast-server/   `server` feature — HTTP service speaking mdcast_api::wire
   ├─ main.rs           wiring only: config → blob store → prefill → Registry → router → axum::serve
   ├─ config.rs         Config::from_env — every MDCAST_* variable, fails fast without a token
   ├─ auth.rs           require_bearer middleware + is_authorized (constant-time, subtle)
   ├─ error.rs          ApiError → status + ErrorResponse; json_body(); internal detail → log only
   ├─ pandoc.rs          probe_pandoc() — startup `pandoc --version` check, feeds GET /v1/capabilities (issue #81)
   ├─ blobs/
   │  ├─ store.rs       BlobStore/BlobWriter — in-memory, byte-budgeted, second-chance ("lives") LRU, sha256-verified, pin()/mirror_path() for #72/pandoc
   │  ├─ provider.rs    BlobAssets — AssetManifest-backed AssetProvider; resolve() reports every missing blob
   │  └─ prefill.rs     prefill() — MDCAST_PREFILL_DIR startup warmup: walk, hash, ingest, pin, mirror (issue #72)
   └─ routes/
      ├─ mod.rs         AppState, router(), /health, artifact_response/content_type
      ├─ capabilities.rs GET /v1/capabilities — unauthenticated deployment-shape probe (targets/template_formats/max_upload_bytes/version, issue #81)
      ├─ blobs.rs       POST /v1/blobs/check + POST /v1/blobs (multipart, streamed, idempotent)
      ├─ render.rs      POST /v1/render — resolve_doc()/provider_stack(), the CLI pipeline minus the filesystem
      └─ template.rs    POST /v1/render/template — TemplateDoc over the manifest + `generated` entries (MemoryAssets, 400 on bad entry/collision); Html arm needs the `typst-html` build feature in `library` mode, needs nothing extra in `cli` mode (issue #73)

embedded/             rust-embed source — keys mirror these paths
├─ typst/layouts/{pdf,pdf-presentation}/{class}.typ
├─ revealjs/{dist,plugin}/…  vendored reveal.js 4.6.0 (fonts stripped)
└─ reference/         reference.{docx,odt,pptx} — real, named styles per class
```

## Architectural seams (don't violate)

- **Pure data lives in `mdcast-api`, behaviour stays in `mdcast`.** A type
  that is just fields (`Target`, `DocMeta`, `BrandSpec` & co.) belongs in
  `mdcast-api`; anything that touches IO, `anyhow` or `tokio` stays here and
  operates on the re-exported type — that is why `frontmatter::extract` did
  not move, and why `mdcast::brand::from_toml` (the `anyhow`-wrapped loader)
  stays a free function here rather than becoming the *only*
  `BrandSpec::from_toml`. `mdcast` re-exports every moved type from its
  original path (`mdcast::Target`, `mdcast::BrandSpec`, `mdcast::DocMeta`, …)
  and also `pub use mdcast_api;`, so nothing downstream has to name the new
  crate. `mdcast-api`'s **default** feature set stays `serde`-only — that is
  the crate's contract; the one exception is `BrandSpec::from_toml` (issue
  #80), an inherent method gated behind an *optional*, off-by-default `toml`
  feature (`mdcast-api/src/brand.rs`) for a caller that has a `brand.toml`
  but must not compile `mdcast` to parse it (e.g. one built on
  `mdcast-client`) — `mdcast` enables that feature on its own `mdcast-api`
  dependency and `brand::from_toml` here delegates to it instead of calling
  `toml::from_str` itself, so there is exactly one parse implementation. Do
  not add any *other* dependency to `mdcast-api`, default or optional,
  without the same bar: a real caller that must not compile `mdcast` hitting
  the gap.
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
  backend code *to read source data* — trait is `async` (boxed-future,
  dyn-safe). Image refs are resolved through it via `images::resolve_images`
  *before* pandoc sees the markdown. The one narrow exception: `path()`
  (issue #71) lets a provider hand back a real filesystem path it already
  controls instead of bytes; `PandocBackend` then calls `std::fs::hard_link`
  on *that provider-supplied path* (never a path it guessed itself) to avoid
  copying potentially large, provider-cached bytes into its own temp dir on
  every render — falling back to the ordinary `get()` + write when `path()`
  returns `None`. `AssetProvider` is still the sole gateway to *which* bytes
  a backend can see; this only changes how the already-authorized bytes get
  materialised.
- **Per-target template namespacing.** A class (`hero`) resolves to a
  *different* file per target: `typst/layouts/pdf/hero.typ` vs
  `typst/layouts/pdf-presentation/hero.typ`. Author writes the class once.
- **`PageSplitter` is a trait.** Consumers can plug in their own (e.g. for a
  different slide-marker convention or transclusion). `DefaultSplitter`
  handles `<page class="X">`, `::: {.X}`, and `---` thematic breaks.
- **Classifier rule order:** explicit class > content-shape > positional
  (first/last) > default. Content-shape predicates are a closed enum in v1
  (`SingleH1Only`, `SingleImageOnly`, `SingleBlockquoteOnly`, `Empty`).
- **The HTTP service is a binary, not a library module.** `mdcast-server`
  lives entirely under `src/bin/mdcast-server/` behind the `server` feature:
  it composes `Registry::render_to_bytes`, `AssetProvider` and the wire types,
  and adds nothing the library needs to know about. Consequences worth
  remembering: nothing outside the bin can `use` its modules (so its
  end-to-end tests are router-level `#[cfg(test)]` tests via
  `tower::ServiceExt::oneshot`, not files under `tests/`), and the wire
  protocol itself stays in `mdcast-api` where `mdcast-client` can reach it
  without compiling an engine.
- **`mdcast-client` must never depend on `mdcast`.** Not with
  `default-features = false`, not at all — that dependency edge is the entire
  reason the workspace exists (`PROJECT_PLAN.md` §7.1). Its permitted
  dependencies are `mdcast-api`, `reqwest` (default features off, +
  `rustls-tls`/`json`/`multipart`, matching the root manifest), `serde`,
  `serde_json`, `sha2`, `bytes` and `thiserror`; `wiremock` + `tokio` are
  dev-only. Verify with `cargo tree -p mdcast-client -e normal` — `mdcast`,
  `rust-embed`, `clap`, `typst` and `pulldown-cmark` must all be absent (~100
  crates, against `mdcast`'s ~329). Anything the client needs from the
  protocol goes into `mdcast-api`, never into a new `mdcast` dependency.
- **The client owns the negotiation; the caller never hashes anything.**
  `AssetBundle` is `key → bytes`; the client computes every sha256, builds the
  `AssetManifest`, and on `409` uploads exactly the blobs the server named.
  **Exactly one retry** — a second `409` for the same digests is
  `Error::BlobsStillMissing`, never a loop, because the only causes are
  eviction racing the retry or a server bug and spinning would hide both. The
  pessimistic path (`check` → `upload` → render) is exposed as
  `Client::preflight` but is not the default.
- **`render_template`/`render_template_html` are a parallel entry point, not
  a second IR.** `backends/typst/template.rs`'s `TemplateDoc`/
  `render_template`/`render_template_html` bypass `ResolvedDoc` entirely (no
  pages, no splitter, no classifier, no `md_to_typst`) for the
  structured-data case (user typst template + JSON → PDF, or HTML behind the
  `typst-html` feature). `Registry`/`Backend` stay untouched — they're
  `Target`-keyed and markdown-shaped, and there is no `Target` variant this
  belongs under. Reuses `/context.typ` and the compiler seam below;
  does not reuse layouts, fonts, or `ResolvedDoc.assets` (no per-class layout
  to declare chrome for — a template reaches its own images via sibling
  files instead).
- **The Typst compiler is a runtime-selectable seam, not always in-process
  (issue #73).** `backends/typst/compiler/` defines `CompileInputs` (a flat,
  already-resolved virtual filesystem: sources, files, fonts, the main file's
  path, and `CompileFormat::{Pdf,Html}`) and `TypstCompiler::{Library,Cli}`.
  Both compile sites — `mod.rs::render_to_bytes` (markdown) and
  `template.rs::compile_template` (data-driven) — reduce their inputs to a
  `CompileInputs` and call `self.compiler.compile(inputs)`/
  `compiler.compile(inputs)`; neither knows or cares which mode actually
  runs. `Library` (default) is exactly the pre-#73 in-process
  `typst-as-lib` compile, just reshaped onto the shared input type — an
  existing deployment setting nothing sees byte-identical behaviour. `Cli`
  materialises `CompileInputs` into a temp directory (`tempfile::TempDir`,
  owner-only permissions, always removed on every return path since the
  guard is held for the whole function) and shells out to `typst compile
  --root <dir> --font-path <dir>/.mdcast-fonts`, wrapped in
  `tokio::time::timeout` with an explicit `child.kill().await` (which also
  reaps) on elapse — the thing `spawn_blocking` structurally cannot offer.
  `safe_join` is the one place every path this module writes is checked for
  `..`/absolute escapes before touching disk — necessary because
  `template.rs`'s sibling discovery deliberately preserves caller-influenced
  `/`-nested provider keys (unlike the markdown path's `images::sanitize_key`,
  which already flattens them), so an unsanitised manifest key must not
  become an arbitrary-file-write outside the temp dir. HTML export
  (`--features html`) works in `Cli` mode regardless of the `typst-html`
  cargo feature — only `Library` mode needs it, since that feature is really
  `typst-as-lib`'s process-wide `typst::Feature::Html` toggle (see the
  `render_template_html` bullet below). `Registry::with_typst_compiler` /
  `TypstBackend::with_compiler` / `render_template_with` /
  `render_template_html_with` are the compiler-explicit entry points;
  `Registry::with_defaults` / `TypstBackend::new` / `render_template` /
  `render_template_html` all still exist unchanged and default to `Library`.

## Asset key namespace

Flat paths, slash-separated. Not a filesystem contract — `EmbeddedAssets`
happens to mirror them.

| Prefix                                | Used by         |
|---------------------------------------|-----------------|
| `typst/layouts/{target}/{class}.typ`  | typst backend   |
| `revealjs/dist/...`, `revealjs/plugin/...` | pandoc html-reveal |
| `revealjs/brand.css`                  | pandoc html-reveal — raw per-class CSS escape hatch, appended to the generated brand `<style>` block (issue #57) |
| `reference/reference.{docx,odt,pptx}` | pandoc (matching target) |
| `mermaid/diagram-{N}.svg`             | mermaid pre-step output (`mermaid::render_diagrams`, `mermaid` feature) — the caller layers these over their provider |
| `qr/qr-{N}.svg`                       | QR pre-step output (`generators::qr::render_qr_codes`, `qr` feature) — layered like mermaid's |
| anything else (e.g. `img/...`)        | image refs in markdown |

## Build / test

Day-to-day commands are wrapped in the `Makefile` — run a bare `make` to list
all targets:

Feature flags belong mostly to `mdcast`, so every `mdcast`-scoped target
passing `--features` is scoped `-p mdcast`; `mdcast-client` has no features
of its own and runs under `--workspace`. `mdcast-api` has exactly one
optional feature, `toml` (issue #80, `BrandSpec::from_toml` — see
"Architectural seams" above), off by default and checked/tested/linted with
its own `-p mdcast-api --features toml` target rather than folded into
`--workspace`, matching the pattern the `mdcast`-side off-by-default features
already use.

```
make build              # cargo build --workspace — default = pandoc + typst + mermaid + qr (+ rt-multi-thread for the bin)
make check-all          # cargo check -p mdcast-api (+ --features toml) + -p mdcast-client + all mdcast feature combos (core / pandoc / typst / qr / bin-without-mermaid-and-qr / default / +typst-html / +remote-images / +server / +server,typst-html)
make test                # cargo test --workspace — default features only; unit suite runs in <1s
make test-api-toml       # cargo test -p mdcast-api --features toml (off-by-default BrandSpec::from_toml, issue #80)
make test-typst-html     # cargo test -p mdcast --features typst-html (off-by-default HTML export, issue #53)
make test-remote-images  # cargo test -p mdcast --features remote-images (off-by-default http(s) image fetch, issue #54)
make test-server         # cargo test -p mdcast --features server (+ the server,typst-html bin) — 81 mdcast-server tests; a handful skip gracefully (printed, not failed) without a `typst` binary on PATH (TypstCompiler::Cli, issue #73)
make lint                # cargo fmt --all --check + cargo clippy --workspace --all-targets -- -D warnings (+ mdcast-api toml, + typst-html, + remote-images, + server, + server,typst-html)
make coverage            # cargo llvm-cov → lcov.info + summary (needs cargo-llvm-cov)
make verify              # lint + check-all + test + test-api-toml + test-typst-html + test-remote-images + test-server — the pre-"done" gate
make run-server          # MDCAST_TOKEN=dev-token cargo run --bin mdcast-server
make deploy              # kubectl apply -f deploy.yml — mdcast-server into the k8s `services` namespace
make deploy-restart      # re-roll + wait for the mdcast deployment — picks up Secret/ConfigMap edits, never a new image (the tag is pinned)
```

Container/deployment: `Dockerfile` builds the `mdcast-server` image (debian
runtime with `pandoc` and a pinned musl `typst` binary, so `MDCAST_TYPST=cli`
works out of the box; `MDCAST_BIND=0.0.0.0:8080` baked in);
`.github/workflows/docker.yml` pushes `ghcr.io/xmiksay/mdcast` on every `v*`
tag (plus `workflow_dispatch`), tagged sha/semver/latest. `deploy.yml` is the
k8s manifest set (Secret + ConfigMap + Deployment + Service in the
`services` namespace, **no Ingress** — internal-only at
`http://mdcast.services`); the committed `MDCAST_TOKEN` is a placeholder,
keep the real one only in your local copy. The Deployment pins the image to
the release tag (`ghcr.io/xmiksay/mdcast:X.Y.Z`, `IfNotPresent`), not
`:latest` — an `Always` pull of `:latest` once resolved to a stale image
through a registry-edge cache right after a push. It requests a full CPU
(typst is CPU-bound; a `100m` request let a loaded node stretch a ~1s
compile past the kill window) and sets `MDCAST_TYPST_TIMEOUT_SECS=100` —
well below the site client's 120s request timeout, since it bounds only the
typst child. `mdcast-server` drains in-flight requests on SIGTERM/Ctrl-C
(`axum::serve(..).with_graceful_shutdown`), and the Deployment's
`terminationGracePeriodSeconds: 110` gives the longest compile time to
finish before the SIGKILL.

The `server` feature is off by default and pulls in `pandoc`, `typst`,
`mermaid`, `qr` and `rt-multi-thread` itself (a render service without engines is
useless) plus axum/sha2/subtle; the `[[bin]] mdcast-server`
`required-features` spells the whole set out again, so `cargo build` without
the feature simply skips the binary.

`tests/render_smoke.rs` drives the real engines (in-process typst, subprocess
pandoc) to a genuine artifact per `Target` — the rest of the suite is unit
tests and never invokes an engine. The pandoc-backed smoke tests skip
gracefully (not `#[ignore]`) when `pandoc` isn't on `PATH`, so `cargo test`
stays green with or without it installed locally.

`.github/workflows/ci.yml` has two jobs, tuned so no commit is fully
compiled/tested more than twice end-to-end (once on its PR, once on merge —
issue #68): `test` runs `make lint`, `make check-all`, `make test`,
`make test-api-toml`, `make test-typst-html`, `make test-remote-images`, and
`make test-server` on every pull request
*and* every push to `master`, with `pandoc` and a prebuilt `typst` CLI binary
both installed so the OOXML/revealjs smoke tests actually exercise pandoc and
the `TypstCompiler::Cli` tests (issue #73) actually exercise a real `typst`
binary in CI, rather than each taking their graceful-skip path. `coverage`
runs `make coverage`
(cargo-llvm-cov) only on release PRs — head branch matching `release-v*` or
a `chore(release)`-prefixed title — writing the summary table to the Actions
job summary and uploading `lcov.info` + the HTML report as a
`coverage-report` artifact; it no longer runs on every master push, since
the release PR's coverage snapshot already covers the commit that lands.

`.github/workflows/release.yml` publishes to crates.io on every `v*` tag
push: it checks the tag against the `Cargo.toml` version and `deploy.yml`'s
pinned image tag, confirms the tag
commit already has a successful `test` check run (via the GitHub API — the
tagged commit is a just-merged, CI-green `master` commit, so there's nothing
to re-verify), then publishes **`mdcast-api` → `mdcast` → `mdcast-client`**
(both dependants name `mdcast-api` by version; `cargo publish` blocks until
each upload lands in the index), authenticated via crates.io Trusted
Publishing (OIDC, `rust-lang/crates-io-auth-action`) — no registry token
secret, and no `make verify` re-run (`cargo publish` performs its own
verification build). All three crates take their version from
`[workspace.package]`, so one bump covers them and the tag check asserts it
for each. Each crate needs its **own** trusted publisher configured on
crates.io — one publisher does not cover a workspace — and trusted
publishing **cannot create a crate that doesn't exist yet**: a brand-new
crate's first publish is a one-time manual `cargo publish` with an API
token, after which its trusted publisher can be configured and the workflow
takes over. The workflow's publish step skips any crate whose tagged version
is already on crates.io (sparse-index check), so a tag run right after such
a manual bootstrap — or a re-run of a partially failed one — is safe.
Release flow: bump the workspace version and `deploy.yml`'s image tag,
merge, tag `vX.Y.Z`, push the tag; once `docker.yml` has pushed the image,
`make deploy`.

## Run

```
./target/debug/mdcast render INPUT.md --target html-reveal --out out.html [--assets DIR] [--brand brand.toml] [--toc-depth N] [--html-image-tags] [--mermaid] [--qr] [--layout-asset KEY]... [--layout-font KEY]... [--typst-compiler library|cli] [--typst-bin PATH] [--typst-timeout-secs N]
./target/debug/mdcast explain INPUT.md [--brand brand.toml] [--html-image-tags] [--mermaid] [--qr]  # prints per-page (class, origin)
./target/debug/mdcast render-template TEMPLATE --data data.json --out out.pdf [--assets DIR] [--brand brand.toml] [--generated generated.json] [--format pdf|html] [--typst-compiler library|cli] [--typst-bin PATH] [--typst-timeout-secs N]  # typst-only, no markdown
```

`--assets DIR` layers a filesystem-backed provider over `EmbeddedAssets`.
That's the easy way to supply images referenced from markdown without writing
Rust. `--html-image-tags` enables the built-in `HtmlImageTags` preprocessor:
`<img src="X">` / `<image path="X">` become `![alt](X)` before splitting, so
the auto-classifier and both engines see real image nodes. `--mermaid` runs
the mermaid pre-step (`mermaid::render_diagrams`, `mermaid` feature): every
```` ```mermaid ```` fence renders to SVG (pure Rust, no Node/Chromium) and
is rewritten to `![](mermaid/diagram-N.svg)`, served from an in-memory layer
over the provider stack; a diagram that fails to render warns and stays a
code block. `--qr` runs the QR pre-step (`generators::qr::render_qr_codes`,
`qr` feature): ```` ```qr ```` fences and `<qr>…</qr>` / `<qr path="KEY">`
tags (attrs `ec`, `size`) become `![](qr/qr-N.svg)`; unlike mermaid it is
async because `path=` reads through the provider stack (`--assets` /
embedded on the CLI, blobs over embedded on the server), so `load_doc`/
`resolve_doc` build the base provider *before* the pre-steps and layer the
SVGs over it after; a failing block warns and stays as written.
`render-template --generated FILE.json` takes the wire's `GeneratedAsset`
array, paths relative to the template's directory; a bad entry or a path
the provider already serves is an error. `--layout-asset` / `--layout-font` declare `ResolvedDoc.assets` /
`.fonts` entries for typst layouts (see README).

`--typst-compiler` (issue #73) selects `TypstCompiler::Library` (default, the
in-process compile) or `Cli` (shells out to `--typst-bin`, default `typst`,
killed after `--typst-timeout-secs`, default `30`). Both `render` and
`render-template` accept it (`main.rs`'s `TypstCompilerArgs`, flattened onto
each subcommand); ignored by pandoc targets. See README's "Selecting the
Typst compiler" section.

`render-template` is the data-driven entry point (issue #52): `TEMPLATE` is an
`AssetProvider` key for the main `.typ` file, `--data` a JSON file deserialized
into `TemplateDoc.data`. See README's "Data-driven template rendering"
section. Only present when built with the `typst` feature. `--format html`
needs the `typst-html` cargo feature in `library` mode, needs nothing extra in
`cli` mode.

## HTTP service (`server` feature)

```
cargo build --features server && MDCAST_TOKEN=… ./target/debug/mdcast-server
```

| Env var                   | Default                  | Meaning |
|---------------------------|--------------------------|---------|
| `MDCAST_TOKEN`             | unset — unauthenticated | Shared bearer token for every `/v1` route. Unset/empty starts the service anyway with every `/v1` route open (no `require_bearer` layer applied at all — see `routes::router`/`config::Config.token: Option<String>`, issue #70) and fires a startup `tracing::warn!`, louder when `MDCAST_BIND` isn't loopback. Only run it token-less behind infrastructure that already authenticates callers. |
| `MDCAST_BIND`             | `127.0.0.1:8080`         | Listen address |
| `MDCAST_MAX_CACHE_BYTES`  | `268435456` (256 MiB)    | In-memory cache budget; second-chance eviction runs on insert |
| `MDCAST_CACHE_LIVES`      | `3`                      | Eviction-survival chances a newly-cached blob starts with |
| `MDCAST_MAX_CACHE_LIVES`  | `32`                     | Ceiling repeated hits can grow a blob's lives to (startup fails if below `MDCAST_CACHE_LIVES`) |
| `MDCAST_MAX_UPLOAD_BYTES` | `268435456` (256 MiB)    | Ceiling for **every** request body, not just uploads |
| `MDCAST_PREFILL_DIR`      | unset — no prefill       | Directory of predefined assets (fonts, brand logo, house `reference.docx`, shared layouts) to warm into the blob store at startup — see below (issue #72) |
| `MDCAST_TYPST`            | `library`                | `library` (in-process, default) or `cli` (shells out to a `typst` binary per compile — issue #73). The remaining `MDCAST_TYPST_*` variables only matter in `cli` mode. |
| `MDCAST_TYPST_BIN`        | `typst`                  | Binary name/path for `cli` mode, looked up on `PATH` unless it's itself a path |
| `MDCAST_TYPST_TIMEOUT_SECS` | `30`                    | Compile timeout for `cli` mode. Exceeded → the child is killed and reaped before the request's error response is returned |
| `MDCAST_TYPST_MAX_MEMORY_BYTES` | unset — unbounded  | Address-space cap (`setrlimit(RLIMIT_AS)`) applied to the `cli`-mode child. Unix-only; a no-op elsewhere |
| `RUST_LOG`                | `info`                   | Same `tracing` filter the CLI uses |

No `MDCAST_BLOB_DIR` — the blob cache is entirely in memory (issue #71) and
does not survive a restart; a caller referencing an evicted-by-restart blob
just gets a `409` and re-uploads it, same as any other eviction. The one
piece of disk it still touches is a lazily-populated, process-scratch mirror
(`blobs/store.rs::mirror_path`) that `PandocBackend` hard-links assets from
instead of rewriting them into every render's temp dir — a hard link (not a
symlink) so the mirror's own eviction can never race a subprocess mid-read;
see `store.rs`'s module doc comment for the full rationale. `BlobStore::pin`/
`unpin`/`pinned_bytes` back the prefill routine below, which pins predefined
assets to exempt them from eviction outright; size `MDCAST_MAX_CACHE_BYTES`
comfortably above whatever the prefilled set ends up being, since pinned
bytes exceeding the budget is a permanent (logged, non-fatal) overshoot
rather than something eviction can fix.

**Startup prefill (`MDCAST_PREFILL_DIR`, issue #72).** When set, `main`
calls `blobs::prefill::prefill` before the router is built: it walks the
directory recursively, and for every regular file under it (the asset *key*
namespace plays no role here — the blob store is purely content-addressed)
computes its sha256, ingests it through the same digest-verifying
`begin_write`/`write`/`finish` path a real `/v1/blobs` upload goes through,
then unconditionally `pin()`s it and calls `mirror_path()` so the pandoc
hard-link mirror is already materialised before any caller asks — a
prefilled asset is known in advance to be reused, so there's no reason to
make the first render pay either cost. A digest already present is skipped
(but still (re-)pinned/mirrored, covering a crash between a previous run's
insert and its pin), so restarting against the same directory ingests
nothing new. A missing/unreadable directory, or a file that fails to read,
fails startup with an error naming the offending path — a broken prefill
source is a broken deployment, not a transient condition to warn past and
continue. `main` logs one `tracing::info!` after prefill completes:
`ingested`/`already_present` file counts, `bytes` ingested this run, and the
store's resulting `pinned_bytes`. Nothing about the wire protocol changes —
prefill only affects which digests are already present when a caller's
manifest is checked, so a client that already knows the predefined digests
sees a `200` on its very first request instead of a `409` round trip.

| Endpoint | Auth | Success | Failure |
|---|---|---|---|
| `GET /health` | no | `200 ok` | — |
| `GET /v1/capabilities` | no | `200 CapabilitiesResponse` | — |
| `POST /v1/blobs/check` | if `MDCAST_TOKEN` set | `200 BlobCheckResponse` | `400`/`401`/`413` |
| `POST /v1/blobs` | if `MDCAST_TOKEN` set | `204` | `400` (bad digest / hash mismatch), `401`, `413` |
| `POST /v1/render` | if `MDCAST_TOKEN` set | `200` artifact bytes | `409 MissingBlobsError`, `400`, `401`, `413`, `422`, `500` |
| `POST /v1/render/template` | if `MDCAST_TOKEN` set | `200` artifact bytes | as above, plus `501` for `format: "html"` without `typst-html` |

`GET /v1/capabilities` (issue #81) sits outside the `require_bearer` layer
entirely — added to the `/v1` router *after* `route_layer` is applied, so it
is unauthenticated in both modes, same as `/health`. It reports
`{targets, template_formats, generators, max_upload_bytes, version}`:
`generators` lists `generators::available()` (today `["qr"]`);
`targets`' typst entries (`pdf`, `pdf-presentation`) are always present, the
pandoc entries (`docx`/`odt`/`pptx`/`html-reveal`) only when
`pandoc::probe_pandoc` found a working binary at startup (`AppState.
pandoc_available`, checked once, not per-request); `template_formats`
includes `html` only when `state.typst_compiler` is `Cli` or the binary was
built with `typst-html`; `max_upload_bytes` echoes `MDCAST_MAX_UPLOAD_BYTES`;
`version` is `CARGO_PKG_VERSION`.

Protocol: a render request carries an `AssetManifest` (asset key → sha256);
the whole manifest is checked up front, and a request naming blobs the server
lacks gets **one** `409` listing every one of them. Upload is
`multipart/form-data`, one part per blob, the part *name* being its claimed
digest — the server re-hashes every byte and rejects a mismatch, so a caller's
digest is never trusted. Content addressing means a stored blob can't be
stale: there is no invalidation protocol and eviction is invisible (a `409`
just means "upload it again").

Response bodies are the artifact itself, with `Content-Type` from the target
and `Content-Disposition: attachment; filename="output.<ext>"` from
`RenderedArtifact::filename`. Errors are `ErrorResponse { code, message }`
(`409` alone has its own shape); internal error text never leaves the process
— it goes to `tracing::error!` under an incident id the response quotes.

## HTTP client (`mdcast-client`)

The counterpart crate: everything above, typed, with the negotiation folded
into one call. No feature flags, async only (`reqwest`'s async client — a
blocking wrapper is deliberately not offered).

```rust
let client = Client::builder("http://mdcast.internal:8080").token(token).build()?;
let bundle = AssetBundle::new().with("img/chart.svg", bytes);
let artifact = client.render(request::markdown(md, Target::Pdf), &bundle).await?;
```

- `AssetBundle` — `key → bytes`, `.with()`/`.insert()`; `manifest()` hashes
  every entry, `bind()` either fills an empty caller manifest from the bundle
  or verifies a caller-supplied one against it (missing key → `MissingAsset`,
  wrong digest → `DigestMismatch`, both *before* a round trip, since a `409`
  can only name a digest). An entry can also be declared digest-only via
  `.with_digest(key, digest, fetch)`/`.insert_digest(...)` (issue #79) —
  `fetch` is an `Fn() -> Future<Output = Result<Bytes, E>>` that `manifest()`/
  `bind()`/`check()` never call (the stored `digest` is enough); it runs only
  from inside an upload (`upload_missing` for a `409`, or `Client::upload` by
  explicit digest), and only for a key the server actually named — the
  steady-state render for a caller whose storage is already content-addressed
  (e.g. a DB keyed by sha256) never touches the bytes at all. A fetch that
  errors becomes `Error::BlobFetchFailed`; bytes that hash to something other
  than the declared digest become `Error::DigestMismatch` — never a silent
  skip, since the retry would just `409` again. `AssetBundle::get` returns
  `None` for a digest-only key (no bytes in hand to return).
- `Client::render` / `render_template` — post, and on `409` upload exactly the
  named blobs (one multipart request, part name = digest) and post the
  identical body again. Once. `Client::check`/`upload`/`preflight` are the
  pessimistic path.
- `Client::capabilities` (issue #81) — `GET /v1/capabilities`, decoded into
  `CapabilitiesResponse`. No negotiation, no bundle — server-side the route
  is unauthenticated, so this is the one call a caller can make before it has
  a token wired up, to feature-gate a UI ahead of any render attempt.
- `Artifact { bytes, filename, content_type }` — headers win; the fallbacks
  are derived (`output.<Target::extension()>`, `application/octet-stream`) and
  a filename carrying a path separator is discarded rather than sanitised.
- `Error` — `Config`, `Transport`, `Unauthorized` (401), `PayloadTooLarge`
  (413), `BlobsStillMissing` (the 409 that survived the retry),
  `Server { status, code: ErrorCode, message }` (the code is carried through,
  never flattened), `UnexpectedStatus`, `InvalidResponse`, `MissingAsset`,
  `UnknownDigest`, `DigestMismatch`, `BlobFetchFailed` (a digest-only entry's
  `fetch` closure errored).

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
- pptx *does* get one post-render patch already (issue #56):
  `backends/pptx_autofit.rs::add_autofit` reopens the pandoc-produced pptx as
  a zip, and for every `ppt/slides/slide*.xml` streams a `quick-xml`
  rewrite that inserts `<a:normAutofit/>` into each body placeholder's
  `<a:bodyPr>` — pandoc's pptx writer never emits this, and the reference doc
  can't express it either, since autofit lives on the slide's own shape
  (`ppt/slides/slideN.xml`), not the layout/master `reference.pptx` supplies.
  A placeholder is "body" if its `<p:ph>` has no `type` attribute or any
  `type` other than `title`/`ctrTitle` (the schema default for a missing
  `type` is `body`); title placeholders and non-placeholder shapes are left
  untouched. An existing `a:noAutofit`/`a:normAutofit`/`a:spAutoFit` child is
  replaced rather than duplicated. `<a:normAutofit/>` carries no precomputed
  `fontScale` — PowerPoint/LibreOffice recompute the shrink amount on
  open/load; estimating `fontScale`/`lnSpcReduction` ourselves for static
  renderers is a possible follow-up, not done here. Every zip entry other
  than the slide XMLs is copied through via `raw_copy_file` (still-compressed
  bytes, untouched) so only the slides actually differ from pandoc's own
  output. This is the smallest instance of the `zip` + `quick-xml`
  template-injection seam `PROJECT_PLAN.md` §10 names for future OOXML
  patching — including the per-class pptx layout selection two bullets up.
- html-reveal gets its own brand projection (issue #57):
  `backends/reveal_brand.rs::brand_css` maps known `BrandSpec.palette`/
  `.fonts` keys onto reveal.js 4.x's `--r-*` theme CSS custom properties
  (scoped to `.reveal`), plus a `--brand-<key>` passthrough for every
  palette key; `logo_html` renders a `position: fixed` `<img>` data-URI
  overlay from an optional `BrandSpec.logo` (`LogoSpec`: provider key,
  corner `position`, optional `width`). `pandoc.rs`'s html-reveal arm calls
  both, writing the results into the render's temp root and wiring them in
  via `--include-in-header` (a `<style data-brand>` block — also carries the
  raw contents of the provider's `revealjs/brand.css` key, if present, as an
  escape hatch for per-class rules like `.reveal section.hero { ... }`) and
  `--include-after-body` (the logo `<img>`); both are `None`/no-op for an
  unbranded doc, so output stays byte-identical to before this existed. Base64
  encoding is hand-rolled in `reveal_brand.rs` rather than pulling in a
  crate — CSS/HTML generation here is deliberately just string building.
  Brand fonts are **name-only** for reveal.js (`--r-main-font: <family>`
  assumes the family is available to the browser) — unlike typst's
  `ResolvedDoc.fonts` (see below), there's no webfont-file packaging yet;
  that pairs with #50 if it lands. A `revealjs/brand.css` value that isn't
  valid UTF-8, or a `logo.key` the provider has no bytes for, each warn via
  `tracing::warn!` and degrade (skip that piece) rather than failing the
  render — same treatment as a missing typst layout asset/font.
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
- Typst runs **in-process by default** (`typst-as-lib`) — no `typst` binary is
  needed to render PDF targets. As of issue #73, the compiler is a seam
  (`backends/typst/compiler/`): `TypstCompiler::Library` is this in-process
  path; `TypstCompiler::Cli` shells out to a real `typst compile` in a temp
  dir instead, selected via `MDCAST_TYPST=cli` (server) or `--typst-compiler
  cli` (CLI). Both `render_to_bytes` (the markdown path) and
  `template.rs`'s `render_template_with`/`render_template_html_with` (the
  data-driven path) build the same `CompileInputs` and hand it to whichever
  compiler is configured — see `compiler/mod.rs`'s module docs. Pandoc
  remains the only *unconditional* external binary dependency; typst becomes
  one too only when `cli` mode is chosen.
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
  --format html` (`bin/mdcast/render_template.rs::Format`). The `Html`
  variant is *not* feature-gated (issue #73): with the default
  `--typst-compiler library`, selecting it without the `typst-html` cargo
  feature still fails, but now at runtime (from
  `compiler::library::compile`'s `CompileFormat::Html` arm) rather than at
  clap's argument-parsing stage; `--typst-compiler cli` needs no such build
  feature at all.
- **`mdcast-server`'s default (`MDCAST_TYPST=library`) compiler has no
  compile time or memory limit — mitigated by `MDCAST_TYPST=cli` (issue
  #73).** A caller-supplied typst template that loops forever wedges one
  `spawn_blocking` thread for the life of the process: `spawn_blocking`
  cannot be cancelled, so a timeout wrapped around the `.await` would free
  the request without reclaiming the thread. This was a deliberate deferral
  in `library` mode, not an oversight — the trust boundary is internal
  callers — and it is exactly what `cli` mode (`backends/typst/compiler/cli.rs`)
  now offers: each compile is a real child process, so
  `MDCAST_TYPST_TIMEOUT_SECS` bounds it with an actual kill+reap (verified by
  `cli::tests::timeout_kills_and_reaps_a_non_terminating_compile` and
  `routes::tests::a_non_terminating_cli_compile_times_out_instead_of_hanging_the_request`
  — both assert the process/thread count returns to baseline, the specific
  check `library` mode structurally cannot pass), and
  `MDCAST_TYPST_MAX_MEMORY_BYTES` can cap its address space via
  `setrlimit(RLIMIT_AS)`. `library` mode is still the default — an existing
  deployment that sets nothing sees byte-identical behaviour and keeps this
  limitation; `cli` mode is the fix, opted into per-deployment. Still do
  **not** paper over the `library`-mode limitation with a
  `tokio::time::timeout` — it would free the request without reclaiming the
  thread.
- `mdcast-server`'s template route deliberately does *not* layer
  `EmbeddedAssets` under the request's blobs, unlike the markdown route.
  `render_template` discovers sibling files by listing the template's own
  directory, and the submitted source is served from the provider root
  (`main.typ`) so relative `#import`s resolve against manifest keys — layering
  the embedded catalog underneath would register the entire vendored reveal.js
  tree and the reference documents as typst virtual files on every request.
  The markdown route layers them normally, so an unmentioned
  `reference/reference.docx` or layout still resolves to the built-in.
- Blob eviction can race a render: a blob present at manifest resolution may be
  evicted before the render reads it, in which case `BlobAssets::get` returns
  `None` and the asset degrades exactly as an unsupplied key does (warn +
  skip). Content addressing makes recovery trivial — upload and retry — so no
  lock or refcount is worth the complexity at a single instance.

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
- Don't split an *engine* into its own crate. Backends are feature-gated
  modules, full stop. The workspace has exactly three members and the two
  extra ones exist for the client side of the HTTP protocol (`mdcast-api` is
  data, `mdcast-client` is transport) — see `PROJECT_PLAN.md` §7.1 for what
  overturned the original single-crate rule and what still holds of it.
- Don't add a `mdcast` dependency to `mdcast-client`, and don't add a
  blocking wrapper to it — it is async-only on purpose.
- Don't add a `--remove-pandoc` flag or similar. Pandoc stays for DOCX/PPTX
  indefinitely — no mature Rust OOXML alternative exists; documented in
  `PROJECT_PLAN.md` §10.
