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
| Engine coupling | **Hidden behind one `Backend` trait** | Core never knows which engine a target uses. Swapping the PDF path later = one new impl + a registry change. |
| Scripting / filters | **Lua now (minimal), Rhai later** | Lua filters are a pandoc feature and are throwaway. Push permanent logic into Rust pre/post steps. Rhai (Rust-native, sandboxable) is for the future own-transformer phase. |

## 4. Scope

**In scope (v1):** the five exporters, brand-spec single source of truth, Mermaid→SVG integration, version-pinned engines, reproducible container image, CLI.

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

Single crate. Backends live in modules; engine dependencies are gated behind Cargo features so users can build with only the engines they need.

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

### Phase 5 — Composition + integration (deferred / optional)
Fragment transclusion with cycle detection (only if genuinely needed). `mdcast-mcp` skin so KB/blog/platform can call "render this page as branded PDF."
**Done when:** the platform can drive exports programmatically.

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
