# mdcast-api

Shared data types and HTTP wire protocol for [mdcast](https://github.com/xmiksay/mdcast)
— **serde-only, no engine dependencies**.

This crate exists so that [`mdcast-client`](https://crates.io/crates/mdcast-client)
(and any other embedder of the protocol) can speak to `mdcast-server` without
compiling the rendering engines: no typst, no vendored reveal.js tree, no
pandoc plumbing. Its default dependency set is `serde`, full stop.

## What's in it

- **Document types** — `Target` (docx / odt / pptx / html-reveal / pdf /
  pdf-presentation), `DocMeta`, `BrandSpec` and the auto-layout rule types.
- **Content addressing** — `Digest` (validated lowercase-hex sha256) and
  `AssetManifest` (asset key → digest).
- **Wire protocol** (`wire` module) — request/response bodies for
  `mdcast-server`'s endpoints: `RenderMarkdownRequest` / `RenderOptions`,
  `RenderTemplateRequest` / `TemplateFormat`, `BlobCheckRequest` /
  `BlobCheckResponse`, `MissingBlobsError` (the `409` body),
  `CapabilitiesResponse`, and `ErrorResponse` / `ErrorCode` for every other
  4xx/5xx.

## The protocol in one paragraph

A render request carries an `AssetManifest` naming every asset it may touch.
The server checks the whole manifest up front; if it lacks any blobs it
answers a single `409` listing all of them, the caller uploads exactly those
(multipart, part name = digest, server re-hashes every byte), and retries the
identical request. Content addressing means a stored blob can never be stale
— eviction is invisible, a `409` just means "upload it again".

## Feature flags

- `toml` *(off by default)* — `BrandSpec::from_toml`, for callers that have a
  `brand.toml` but must not compile `mdcast` itself.

## Related crates

- [`mdcast`](https://crates.io/crates/mdcast) — the rendering library, CLI,
  and (behind its `server` feature) the `mdcast-server` binary.
- [`mdcast-client`](https://crates.io/crates/mdcast-client) — async client
  with the manifest negotiation folded into one call.
