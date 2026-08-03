//! The client itself: one call per render, with the blob negotiation folded
//! into it.
//!
//! A render is optimistic by default — post the request, and if the server
//! answers `409` with digests it does not hold, upload exactly those and post
//! the identical request again. **Once.** A second `409` for the same digests
//! means eviction is racing the retry or the server is broken, and either way
//! a loop would turn a bug into a hang, so it surfaces as
//! [`Error::BlobsStillMissing`].
//!
//! The pessimistic strategy is [`Client::preflight`] (check → upload → render,
//! zero `409`s), for callers that would rather pay a round trip than have a
//! large upload discovered mid-render.

use mdcast_api::wire::{
    MissingBlob, MissingBlobsError, RenderMarkdownRequest, RenderTemplateRequest, TemplateFormat,
};
use reqwest::{Response, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::artifact::{self, Artifact};
use crate::bundle::AssetBundle;
use crate::error::{self, Error, Result};

pub(crate) const RENDER: &str = "/v1/render";
pub(crate) const RENDER_TEMPLATE: &str = "/v1/render/template";
pub(crate) const BLOBS: &str = "/v1/blobs";
pub(crate) const BLOBS_CHECK: &str = "/v1/blobs/check";

/// A handle on one `mdcast-server`. Cheap to clone — the underlying
/// `reqwest::Client` is a shared connection pool.
#[derive(Clone)]
pub struct Client {
    pub(crate) http: reqwest::Client,
    base_url: String,
    pub(crate) token: String,
}

/// Builder for a [`Client`]. `base_url` and `token` are required; the HTTP
/// client is optional so a caller can supply its own pool, timeouts, or proxy
/// configuration instead of taking `reqwest`'s defaults.
#[derive(Clone, Default)]
pub struct ClientBuilder {
    base_url: String,
    token: Option<String>,
    http: Option<reqwest::Client>,
}

/// `Debug` is written out rather than derived on both types: the token is a
/// shared secret, and a derived impl would print it into whatever log or panic
/// message formats the client.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Start building a client for the server at `base_url`
    /// (`http://mdcast.internal:8080`, with or without a trailing slash).
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
            ..ClientBuilder::default()
        }
    }

    /// The normalised base URL, without its trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Render markdown, negotiating any assets the server is missing.
    ///
    /// `request.assets` is filled from `bundle` when the caller left it empty,
    /// and verified against it otherwise.
    pub async fn render(
        &self,
        mut request: RenderMarkdownRequest,
        bundle: &AssetBundle,
    ) -> Result<Artifact> {
        request.assets = bundle.bind(request.assets)?;
        let fallback = format!("output.{}", request.target.extension());
        self.negotiate(RENDER, &request, bundle, &fallback).await
    }

    /// Render a typst template over structured data — same negotiation, same
    /// single retry.
    pub async fn render_template(
        &self,
        mut request: RenderTemplateRequest,
        bundle: &AssetBundle,
    ) -> Result<Artifact> {
        request.assets = bundle.bind(request.assets)?;
        let fallback = match request.format {
            TemplateFormat::Pdf => "output.pdf",
            TemplateFormat::Html => "output.html",
        };
        self.negotiate(RENDER_TEMPLATE, &request, bundle, fallback)
            .await
    }

    /// Post `body`, upload what came back missing, post it again. Exactly one
    /// retry — see the module docs.
    async fn negotiate<B: Serialize>(
        &self,
        path: &str,
        body: &B,
        bundle: &AssetBundle,
        fallback_name: &str,
    ) -> Result<Artifact> {
        let missing = match self.attempt(path, body, fallback_name).await? {
            Attempt::Rendered(artifact) => return Ok(artifact),
            Attempt::Missing(missing) => missing,
        };

        self.upload_missing(&missing, bundle).await?;

        match self.attempt(path, body, fallback_name).await? {
            Attempt::Rendered(artifact) => Ok(artifact),
            Attempt::Missing(missing) => Err(Error::BlobsStillMissing { missing }),
        }
    }

    async fn attempt<B: Serialize>(
        &self,
        path: &str,
        body: &B,
        fallback_name: &str,
    ) -> Result<Attempt> {
        let response = self.post_json(path, body).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await?;

        if status.is_success() {
            return Ok(Attempt::Rendered(artifact::from_response(
                &headers,
                bytes,
                fallback_name,
            )));
        }
        if status == StatusCode::CONFLICT {
            let body: MissingBlobsError = decode(status, &bytes)?;
            return Ok(Attempt::Missing(body.missing));
        }
        Err(error::from_status(status.as_u16(), &bytes))
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    pub(crate) async fn post_json<B: Serialize>(&self, path: &str, body: &B) -> Result<Response> {
        Ok(self
            .http
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?)
    }

    /// Read a JSON response body, mapping any non-success status onto the
    /// typed error first.
    pub(crate) async fn json_response<T: DeserializeOwned>(&self, response: Response) -> Result<T> {
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            return Err(error::from_status(status.as_u16(), &bytes));
        }
        decode(status, &bytes)
    }

    /// A response whose only interesting property is that it succeeded.
    pub(crate) async fn expect_success(&self, response: Response) -> Result<()> {
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let bytes = response.bytes().await?;
        Err(error::from_status(status.as_u16(), &bytes))
    }
}

/// What one render POST produced: the artifact, or the negotiation signal.
enum Attempt {
    Rendered(Artifact),
    Missing(Vec<MissingBlob>),
}

fn decode<T: DeserializeOwned>(status: StatusCode, bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| Error::InvalidResponse {
        status: status.as_u16(),
        reason: e.to_string(),
    })
}

impl ClientBuilder {
    /// The shared bearer token every `/v1` route requires.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Use this `reqwest::Client` — the caller's own connection pool, timeouts,
    /// redirect policy or proxy — instead of a freshly built default one.
    #[must_use]
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    pub fn build(self) -> Result<Client> {
        let base_url = self.base_url.trim_end_matches('/').to_string();
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(Error::Config(format!(
                "base URL must start with http:// or https://, got {:?}",
                self.base_url
            )));
        }

        let token = self.token.unwrap_or_default();
        if token.is_empty() {
            return Err(Error::Config(
                "a bearer token is required — every /v1 route on mdcast-server is gated".into(),
            ));
        }

        Ok(Client {
            http: self.http.unwrap_or_default(),
            base_url,
            token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(base_url: &str) -> Client {
        Client::builder(base_url)
            .token("t")
            .build()
            .expect("a valid configuration")
    }

    #[test]
    fn urls_are_joined_with_exactly_one_slash() {
        for base in ["http://h:8080", "http://h:8080/", "http://h:8080///"] {
            assert_eq!(client(base).url(RENDER), "http://h:8080/v1/render");
        }
        assert_eq!(client("https://h").base_url(), "https://h");
    }

    #[test]
    fn a_base_url_without_a_scheme_is_a_configuration_error() {
        for base in ["h:8080", "", "ftp://h", "//h"] {
            match Client::builder(base).token("t").build() {
                Err(Error::Config(message)) => assert!(message.contains("http://"), "{message}"),
                other => panic!("accepted base URL {base:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn debug_never_prints_the_bearer_token() {
        let secret = "correct-horse-battery-staple";
        let builder = Client::builder("http://h").token(secret);
        let client = builder.clone().build().unwrap();

        for rendered in [format!("{builder:?}"), format!("{client:?}")] {
            assert!(!rendered.contains(secret), "leaked the token: {rendered}");
            assert!(rendered.contains("<redacted>"), "{rendered}");
        }
    }

    #[test]
    fn a_missing_or_empty_token_is_a_configuration_error() {
        for builder in [
            Client::builder("http://h"),
            Client::builder("http://h").token(""),
        ] {
            match builder.build() {
                Err(Error::Config(message)) => assert!(message.contains("token"), "{message}"),
                other => panic!("built a client with no token: {other:?}"),
            }
        }
    }
}
