# mdcast-client

Async HTTP client for [`mdcast-server`](https://github.com/xmiksay/mdcast) —
markdown → DOCX / ODT / PDF / PDF-presentation / PPTX / reveal.js HTML over
HTTP, with the content-addressed asset negotiation made invisible.

Hand it a document and a bag of assets. It hashes them, builds the manifest,
posts the render, and — if the server answers `409` with blobs it does not
hold — uploads exactly those and retries once. You never compute a digest,
never build a manifest, and never write a retry loop.

```rust
use mdcast_client::{AssetBundle, Client, request};
use mdcast_api::Target;

let client = Client::builder("http://mdcast.internal:8080")
    .token(std::env::var("MDCAST_TOKEN")?)
    .build()?;

let bundle = AssetBundle::new().with("img/chart.svg", std::fs::read("chart.svg")?);
let artifact = client
    .render(request::markdown("# Q3\n\n![](img/chart.svg)", Target::Pdf), &bundle)
    .await?;

std::fs::write(&artifact.filename, &artifact.bytes)?;
```

That is the whole cold-cache exchange: three requests (`409` → upload →
`200`) behind one `await`. A warm cache costs one.

## Highlights

- **`AssetBundle`** — `key → bytes`; the client computes every sha256. Storage
  that already knows its hashes can register *digest-only* entries
  (`with_digest`) whose bytes are fetched lazily, only when the server
  actually asks for them in a `409`.
- **Exactly one retry** — a second `409` for the same digests is
  `Error::BlobsStillMissing`, never a loop: the only causes are eviction
  racing the retry or a server bug, and spinning would hide both.
- **Pessimistic path available** — `Client::preflight` (`check` → `upload` →
  render) for callers that would rather never discover a large upload
  mid-render.
- **`Client::capabilities()`** — ask a server which targets and template
  formats its deployment actually supports (pandoc present? HTML export?)
  before feature-gating UI.
- **Typed errors** — `Unauthorized`, `PayloadTooLarge`, `BlobsStillMissing`,
  `Server { status, code, message }` with the wire `ErrorCode` carried
  through, plus pre-flight bundle validation (`MissingAsset`,
  `DigestMismatch`) that fails before any round trip.
- **Template rendering** — `render_template` posts a typst template + JSON
  data for data-driven PDF/HTML generation, same negotiation.

## What it never depends on

`mdcast` itself. Speaking the protocol needs a handful of serde structs
([`mdcast-api`](https://crates.io/crates/mdcast-api)) and an HTTP client
(`reqwest`, rustls) — not a rendering engine. Async only; a blocking wrapper
is deliberately not offered.

## Server side

Run the server from the [`mdcast`](https://crates.io/crates/mdcast) crate:

```sh
cargo install mdcast --features server
MDCAST_TOKEN=… mdcast-server
```

See the [repository](https://github.com/xmiksay/mdcast) for the full
protocol, deployment and configuration reference.
