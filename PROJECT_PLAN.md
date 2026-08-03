# Markdown Multi-Format Exporter — Project Plan

> **Project name:** TBD (you choose). Placeholder used below: `mdcast`.
> Available on crates.io as of writing: `mdcast`, `mdexport`, `polymd`, `mdweave`, `mdmorph`, `mdmulti`, `omnimd`, `mdpress`, `markdown-export`, others.

## 1. Goal

A focused Rust tool that takes Markdown and produces five outputs:

- **DOCX**
- **PDF**
- **PDF presentation**
- **PPTX**
- **HTML bundle** (reveal.js)

It replaces a slow, heavy `pandoc + LaTeX` toolchain with a lean, fast, reproducible one, and is built so that pieces can be **owned in-house later** without a rewrite.

## 2. Guiding principle

> Build for **replaceability at the seams**, not flexibility everywhere.

The value of this tool is *not* the format conversion — that's a solved problem (pandoc + typst). The value is the F13-specific layer: branding across all formats, platform/MCP integration, and a reproducible pipeline. So the conversion core stays borrowed and boring; the seams are placed exactly where future carve-outs will happen.

## 3. Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| PDF / PDF-presentation engine | **Typst** | Single fast Rust binary; replaces the entire heavy LaTeX/TeXLive toolchain. The slow path stops being slow. |
| DOCX / PPTX / reveal.js engine | **Pandoc** | Native OOXML + revealjs writers; already fast (never touched LaTeX); mature output fidelity nobody has matched in Rust. |
| Quarto | **Rejected** | Heavyweight Deno-based external tool, not embeddable as a Rust library. (Note: it does *not* require Jupyter — but the embeddability/weight objection stands.) |
| Diagrams | **Existing in-house Mermaid→SVG (Rust)** | Already built; faster and better-integrated than Kroki. Integrated as a Rust pre-processing step, not a pandoc Lua filter — so it survives any future engine change. |
| Internal representation | **Own `ResolvedDoc` type** | Never pandoc's JSON AST. This single decision keeps pandoc removable. |
| Crate structure | ~~**Single crate** (no sub-crates)~~ → **workspace: `mdcast` + `mdcast-api`** | *Changed — see §7.1.* The original call stands for the engines: backends stay feature-gated modules inside `mdcast`, not crates. What overturned it is the HTTP service: an out-of-process client would otherwise compile `rust-embed` (the whole vendored `embedded/` tree through a proc macro), `clap`, `tempfile`, `toml`, `regex`, `pulldown-cmark`, `futures` and a fat `tokio` just to name a few request structs. |
| Engine coupling | **Hidden behind one `Backend` trait** | Core never knows which engine a target uses. Swapping the PDF path later = one new impl + a registry change. |
| Scripting / filters | **Lua now (minimal), Rhai later** | Lua filters are a pandoc feature and are throwaway. Push permanent logic into Rust pre/post steps. Rhai (Rust-native, sandboxable) is for the future own-transformer phase. |

## 4. Scope

**In scope (v1):** the five exporters, brand-spec single source of truth, Mermaid→SVG integration, version-pinned engines, reproducible container image, CLI.

**Added to scope:** an **HTTP service** (`mdcast-server`, a bin inside the `mdcast` crate behind a `server` feature) and a **standalone client** (`mdcast-client`), speaking the wire protocol in `mdcast-api`. This is the "platform can drive exports programmatically" leg of Phase 5, pulled forward: one host owns pandoc/typst/fonts, everyone else posts markdown. Assets are negotiated by content digest rather than shipped on every request — the client sends a manifest, the server answers `409` with the digests it lacks, the client uploads those and retries.

**Explicitly out of scope (v1), addable later at existing seams:**

- Fragment / transclusion composition model
- Caching (diagram + output)
- MCP skin / platform integration
- Any replacement of pandoc

## 5. Format → engine map

| Target | Engine | Style asset |
|---|---|---|
| `docx` | pandoc (OOXML writer) | `reference.docx` |
| `pptx` | pandoc (OOXML writer) | `reference.pptx` |
| `html` (reveal.js bundle) | pandoc (`revealjs` writer) | theme CSS + pinned reveal dist |
| `pdf` | Typst (doc template) → `typst compile` | `doc.typ` |
| `pdf-presentation` | Typst + **touying** slides → `typst compile` | `slides.typ` |

**Known asymmetry:** four of five paths are "invoke engine with the right template." `pdf-presentation` is the **one bespoke path** — pandoc's Typst writer emits a *flowing* document, so mapping slide boundaries onto touying's slide construct is custom work. This stays bespoke regardless of front end; the future own-transformer will **not** make it disappear.

## 6. Architecture seam (the load-bearing spec)

The two types that determine whether the future is cheap:

```rust
/// The pipeline currency. OUR type — never pandoc's AST.
pub struct ResolvedDoc {
    pub body: String,            // resolved markdown (post pre-processing)
    pub meta: DocMeta,           // title, author, date, custom fields
    pub brand: BrandHandle,      // reference into the brand spec
    pub assets: Vec<AssetRef>,   // e.g. rendered SVG diagrams
}

pub enum Target { Docx, Pdf, PdfPresentation, Pptx, HtmlReveal }

/// Every output format is one impl. Pandoc is just one kind of guest.
pub trait Backend {
    fn target(&self) -> Target;
    fn render(&self, doc: &ResolvedDoc, brand: &BrandSpec, out: &Path)
        -> Result<Artifact>;
}
```

Dispatch is a registry of `Box<dyn Backend>` keyed by `Target`. **Rule:** no pandoc flag, no pandoc AST, no engine name ever appears in core. If it does, the seam is in the wrong place.

## 7. Project structure

Backends live in modules; engine dependencies are gated behind Cargo features so users can build with only the engines they need. The sketch below is the original single-crate plan — still accurate for everything under `src/`.

```
mdcast/
├─ Cargo.toml                  # single crate, features: pandoc, typst (default = both)
├─ src/
│  ├─ lib.rs                   # ResolvedDoc, Backend trait, registry, pipeline
│  ├─ brand.rs                 # BrandSpec
│  ├─ backends/
│  │  ├─ mod.rs
│  │  ├─ pandoc.rs             # #[cfg(feature = "pandoc")] — docx, pptx, revealjs
│  │  └─ typst.rs              # #[cfg(feature = "typst")]  — pdf, pdf-presentation
│  └─ bin/mdcast.rs            # CLI
├─ assets/
│  ├─ brand/brand.toml         # single source of truth: palette, fonts, logo, margins
│  ├─ reference/               # reference.docx, reference.pptx
│  ├─ typst/                   # doc.typ, slides.typ (touying)
│  └─ revealjs/                # theme css + pinned reveal dist
├─ filters/                    # lua: ONLY irreducibly pandoc-AST-shaped tweaks
└─ tests/golden/               # per-format snapshot fixtures
```

```toml
# Cargo.toml — feature sketch
[features]
default = ["pandoc", "typst"]
pandoc = []   # gates the pandoc-invoking backend module + its deps
typst  = []   # gates the typst-invoking backend module + its deps
```

> Stays a single crate. Split into a workspace only if a backend grows non-trivial native deps that pollute the build for users who don't need it — the load-bearing boundary is the `Backend` trait, not the crate count.

### 7.1 Amendment: the workspace split (HTTP service)

**The rule above no longer holds in full.** The repo is now a Cargo workspace:
the root `Cargo.toml` is both `[workspace]` and the `mdcast` `[package]`
(`src/`, `tests/`, `embedded/`, `examples/` did not move), plus two members:

```
Cargo.toml       [workspace] + [package] mdcast  — engines, CLI, and the server bin
mdcast-api/      serde-only shared types + HTTP wire protocol
mdcast-client/   async client for the service — mdcast-api + reqwest, never mdcast
```

The trigger was *not* the one the rule anticipated — no backend grew native
deps. It was the direction of dependency for the HTTP service: a
`mdcast-client` that depended on `mdcast` would compile `rust-embed` (and
with it the entire vendored `embedded/` tree — reveal.js plus the reference
OOXML documents — through a proc macro), `clap`, `tempfile`, `toml`,
`regex`, `pulldown-cmark`, `futures`, `tracing-subscriber` and a fat `tokio`,
in order to name a handful of request structs. That is the cost the original
rule exists to prevent; it just arrives from the consumer side rather than
the backend side.

So the split is drawn at data, not at engines:

- **`mdcast-api`** — `Target`, `DocMeta`, `BrandSpec` (+ `LogoSpec`,
  `LogoPosition`, `AutoLayout`, `AutoRule`, `ShapePredicate`), the asset
  manifest/digest types, and the request/response bodies. **serde and
  nothing else.** No `anyhow`, no `tokio`, no `toml`, no IO.
- **`mdcast`** — unchanged in shape. It depends on `mdcast-api` and
  re-exports every moved type from its original path, so `mdcast::Target`,
  `mdcast::BrandSpec` and friends still resolve. The loaders that need IO or
  `anyhow` stayed behind (`brand::from_toml`, `frontmatter::extract`).
- **`mdcast-client`** — depends on `mdcast-api` plus `reqwest`, `serde`,
  `serde_json`, `sha2`, `bytes` and `thiserror`. **Never on `mdcast`**, not
  even with `default-features = false`; that edge is the whole reason for the
  split, so it is a verified property, not an intention — `cargo tree -p
  mdcast-client -e normal` is ~100 crates with no `mdcast`, `rust-embed`,
  `clap`, `typst` or `pulldown-cmark` in it, against `mdcast`'s ~329.

Backends are still feature-gated modules, never crates. The `Backend` trait
is still the load-bearing boundary. All three crates share one version,
bumped and published together, in dependency order (`mdcast-api` first) —
each with its own crates.io trusted publisher.

### 7.2 The service: `mdcast-server`

Landed as a **binary inside `mdcast`** (`src/bin/mdcast-server/`, `server`
feature), not a crate — it composes `Registry::render_to_bytes`,
`AssetProvider` and the `mdcast-api` wire types and adds nothing the library
needs to know about. Deliberate shape, decided up front:

| Decision | Choice | Why |
|---|---|---|
| Trust boundary | Internal / trusted callers | One shared bearer token from `MDCAST_TOKEN`, no user model, no per-caller quota |
| Request model | **Synchronous** | `POST` blocks, the response body *is* the artifact. No queue, no polling, no `202` — a render is seconds, and a job store is a second source of truth |
| Assets | Content-addressed, negotiated | Manifest of `key → sha256`; missing blobs come back as one `409`, the caller uploads those and retries |
| Deployment | Single instance, node-local blob cache | LRU on disk, mtime-ordered — no shared store, no cache coherency problem |
| Compile limits | **None (deferred)** | See below |

Endpoints — all under `/v1` and bearer-gated except health:

| Method + path | Body → response |
|---|---|
| `GET /health` | liveness, unauthenticated |
| `POST /v1/blobs/check` | `BlobCheckRequest` → `BlobCheckResponse` (skip a doomed render round-trip) |
| `POST /v1/blobs` | `multipart/form-data`, part name = claimed digest → `204`; server re-hashes every byte |
| `POST /v1/render` | `RenderMarkdownRequest` → artifact bytes, or `409 MissingBlobsError` |
| `POST /v1/render/template` | `RenderTemplateRequest` → artifact bytes (`501` for `html` without `typst-html`) |

The whole manifest is checked before any work starts, so a `409` lists *every*
missing blob — a client that had to discover them one failed render at a time
is exactly the failure mode this protocol exists to avoid. Content addressing
carries the rest: a stored blob can never be stale, so there is no
invalidation protocol and eviction needs no client-visible signal.

**Deferred: no compile time or memory limit.** A caller-supplied typst
template that loops forever wedges one `spawn_blocking` thread for the life of
the process — `spawn_blocking` cannot be cancelled, so a `tokio::time::timeout`
would free the request while leaking the thread, which is worse than nothing
because it hides the problem. Accepted because the trust boundary is internal
callers. The trigger to take it: untrusted templates become a requirement, or
a wedged thread is observed in production. The scope when taken: a `Compiler`
trait with the compile behind it, and a subprocess implementation that can be
killed, with `RLIMIT_CPU`/`RLIMIT_AS` set — an additive change at a new seam,
in keeping with §10.

### 7.3 The client: `mdcast-client`

The third crate, and the one that makes §7.1's split pay off: it depends on
`mdcast-api` plus an HTTP client, speaks exactly the protocol above, and never
links an engine.

Its job is to make the blob negotiation invisible. A caller fills an
`AssetBundle` with `key → bytes` — the same flat `AssetProvider` keys used
everywhere else — and calls `render`. The client hashes every asset, builds
the `AssetManifest`, posts the request, and on `409` uploads exactly the blobs
the server named (one multipart request, part name = digest) before posting
the identical body again.

| Decision | Choice | Why |
|---|---|---|
| Dependency on `mdcast` | **Forbidden** | The one constraint the crate exists for — see §7.1 |
| Sync/async | **Async only** | `reqwest`'s async client; a blocking wrapper would be a second API to keep honest for no new capability |
| Hashing | **The client's job** | A caller that has to compute sha256 or hand-build a manifest has been handed the protocol, not a client |
| Retry on `409` | **Exactly once** | A second `409` for the same digests means eviction is racing the retry or the server is broken; a loop would turn a bug into a hang, so it becomes a distinct typed error |
| Strategy | **Optimistic by default** | One request warm, three cold. `check`/`preflight` are exposed for callers that would rather pay a guaranteed round trip than discover a large upload mid-render |
| Bundle completeness | **Checked locally** | The server can only answer `409` with a digest; a manifest key with no bytes behind it fails before the round trip, where the *key* is still known |
| Errors | **`thiserror`, not flattened** | Transport, `401`, `413`, a surviving `409`, and the server's own `ErrorResponse` with its `ErrorCode` intact are all separate variants a caller can match on |

Test double is `wiremock` as a dev-dependency — it never reaches a consumer's
tree, and the real `mdcast-server` binary is deliberately not started: these
tests are about what the client does with the statuses the protocol defines,
and pulling an engine in to produce them would defeat the point of the crate.

## 8. Delivery plan (ordered by dependency, not calendar)

### Phase 0 — Skeleton
Single-crate skeleton with `pandoc`/`typst` features, config loading, `BrandSpec` schema, `ResolvedDoc`, `Backend` trait + registry, pinned pandoc & typst versions, OCI base image (the `pandoc/typst` image bundles both — use as the floor).
**Done when:** `mdcast render` runs end-to-end on a trivial doc through full dispatch (produces nothing useful yet, but the spine is proven).

### Phase 1 — DOCX + PDF
The two highest-value, lowest-risk targets. Throwaway styling. Lock golden tests now so later refactors can't silently regress.
**Done when:** one real F13 document renders to both, byte-stable under golden test.

### Phase 2 — PPTX + HTML bundle
Both are pandoc writers → mostly config. Decide HTML bundle shape: **single self-contained file** (`--embed-resources --standalone`) vs **asset folder/zip** (default to self-contained). Add the target-scoping filter (`::: {.only-deck}` / `::: {.only-print}`) — first point where doc-vs-deck divergence bites.
**Done when:** the same source document yields a correct deck and a correct report from one input.

### Phase 3 — PDF-presentation (the bespoke one, isolated)
Build the Typst + touying template and the slide-boundary mapping. Treat as a small spike before committing to the approach.
**Done when:** slide-delimited markdown produces a clean touying PDF deck.

### Phase 4 — Brand projection + diagrams + cache foundation
Generate all four style assets *from* `brand.toml` (one color change, one edit). Integrate the **existing Rust Mermaid→SVG renderer** as a pre-processing stage. Add content-hash diagram cache + output cache (skip targets whose resolved input + brand + engine versions hash unchanged).
**Done when:** rebranding is a single-file edit; diagrams flow to all five formats; unchanged inputs are skipped.

> **Status:** the diagram leg landed — `mermaid-svg` 0.7.0 (the in-house
> pure-Rust renderer, now on crates.io) is integrated as the `mermaid`
> feature's pre-splitter step (`src/mermaid.rs::render_diagrams`), flowing
> SVGs to every target through the existing `AssetProvider` pipeline. The
> content-hash diagram/output cache remains open.

### Phase 5 — Composition + integration (deferred / optional)
Fragment transclusion with cycle detection (only if genuinely needed). `mdcast-mcp` skin so KB/blog/platform can call "render this page as branded PDF."
**Done when:** the platform can drive exports programmatically.

> **Status:** the integration leg landed early and is now complete —
> `mdcast-server` (§7.2) is the "platform can drive exports programmatically"
> half over HTTP rather than MCP, and `mdcast-client` (§7.3) is the calling
> half, so a consumer embeds one crate and gets branded exports without
> pandoc, typst or fonts anywhere near its build. Any MCP skin sits on the
> same wire protocol. Fragment transclusion remains open and still optional.

## 9. Sequencing logic

Prove the dispatch architecture cheaply (0) → bank easy high-value wins with regression protection (1–2) → de-risk the one bespoke path in isolation (3) → make it *branded* and *fast* (4) → wire into the platform (5). Every deferred item lands as an **additive change at a seam already in place**, never a teardown.

## 10. Future evolution — with explicit triggers

The point of the architecture is that none of these are forced now, and each has a concrete trigger.

| Future move | Trigger (NOT "it would be nice") | Scope when taken |
|---|---|---|
| Own the `md → typst` path (drop pandoc for PDF/pdf-presentation) | Integration pain with pandoc on the typst path, or desire for in-process control | One new `Backend` impl under a new feature flag: comrak/markdown-rs → Typst → `typst` crate. Bounded. |
| Own `md → reveal.js` | Same | Easy; pure HTML emission. |
| Rhai instead of Lua | Comes *with* the own-transformer; no pandoc-Lua bridge once you parse yourself | Rhai = Rust-native, sandboxable (op limits, no ambient I/O) — fits the threat-model bias. |
| Own DOCX/PPTX | **Requirement change**, not dependency removal: need *programmatic fine-grained control* (positioned elements, generated tables) that pandoc's reference-doc model can't express | **Template-injection**, not from-scratch: hand-authored `template.pptx`/`reference.docx` + `zip` + `quick-xml` inject content. Sidesteps the fidelity moat. |

**Hard rule:** "replace pandoc" never means *all* of pandoc. Realistic end state = pandoc retained for DOCX/PPTX, own transformer for typst + html. Framing it as "remove pandoc entirely" recreates the OOXML problem for zero gain.

The `zip` + `quick-xml` template-injection seam described above is no longer
purely hypothetical: `src/backends/pptx_autofit.rs` (issue #56) already
post-render-patches pandoc's pptx output, inserting `<a:normAutofit/>` into
body placeholders so overflowing slide text shrinks instead of spilling off
the slide — a targeted single-element patch, not full template injection, but
the same seam future OOXML fine-grained control (e.g. per-class pptx layout
selection, above) would build on.

### Rust OOXML ecosystem note (why DOCX/PPTX stays on pandoc)
As of writing, no mature Rust crate does high-fidelity docx+pptx *generation*. `ooxmlsdk` (low-level, schema-generated types — you supply the semantics) and `litchi` (friendly API but pre-production, "not recommended for production") are the closest; PPTX generation specifically is the weakest spot. XLSX is the only well-served OOXML format (`rust_xlsxwriter`) — and it's the one not needed here. The moat isn't zip/XML plumbing; it's the accumulated knowledge of what XML makes Word/PowerPoint render correctly. File `ooxmlsdk` + the template-injection pattern away for the requirement-change trigger above.

## 11. Risks

- **pdf-presentation / touying mapping** — the one genuinely custom piece. Isolated in Phase 3; spike before committing.
- **Brand projection across four dialects** — easy to under-design. Stub the projection in Phase 0 even while styling is ugly; retrofitting it later is painful.
- **Mermaid heaviness via `mmdc`** — already mitigated by the in-house Rust renderer; keep it a pre-step, never a Chromium dependency.
- **Reproducibility** — pin pandoc + typst versions as part of the build contract (matters for ISO/SOC2 deliverables).

## 12. Definition of "solid start" (v1)

`Backend` trait + own `ResolvedDoc` + five backend impls (pandoc ×3, typst ×2) + Mermaid→SVG Rust pre-step + stubbed brand projection + pinned engine versions + one end-to-end golden test. A few hundred lines of orchestration over two trusted binaries — with every named future move reachable as an additive change at a deliberate seam.
