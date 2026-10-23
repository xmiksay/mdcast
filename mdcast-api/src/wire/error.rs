//! The generic error body every 4xx/5xx other than `409 Conflict` carries.
//! `409` has its own shape ([`crate::MissingBlobsError`]) because a client
//! acts on it rather than reporting it.

use serde::{Deserialize, Serialize};

/// A failed request: something for a human (`message`) and something for the
/// client to branch on (`code`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
    /// Sanitised engine diagnostics (`path:line:col: severity: message`) for a
    /// `render_failed`, present only when the server opts in
    /// (`MDCAST_EXPOSE_DIAGNOSTICS`). Omitted when empty, so a body without
    /// it is byte-identical to one from a server that predates the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
}

impl ErrorResponse {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: Vec::new(),
        }
    }

    pub fn with_detail(mut self, detail: Vec<String>) -> Self {
        self.detail = detail;
        self
    }
}

/// Machine-readable failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The body did not parse, or a field is out of range.
    BadRequest,
    /// No bearer token, a malformed `Authorization` header, or the wrong
    /// token. Distinct from [`ErrorCode::BadRequest`] because the client's
    /// remedy is to fix its credentials, not its payload.
    Unauthorized,
    /// The request body is larger than the server accepts. Distinct from
    /// [`ErrorCode::BadRequest`] because retrying the identical request is
    /// hopeless — the client has to split or shrink the upload.
    PayloadTooLarge,
    /// The server was built without the feature this target/format needs
    /// (e.g. `html` without `typst-html`), or lacks the `pandoc` binary.
    UnsupportedTarget,
    /// The pipeline ran and failed — bad markdown, a template that does not
    /// compile, an engine error.
    RenderFailed,
    /// Anything the server cannot attribute to the request.
    Internal,
    /// Every render slot stayed busy for the server's whole queue wait
    /// (`503`, with `Retry-After`). Nothing is wrong with the request; send
    /// it again later.
    Overloaded,
    /// A code this version of the protocol does not know — what a newer
    /// server's addition decodes to, instead of failing the whole body.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_round_trips_through_json() {
        let err = ErrorResponse::new(ErrorCode::RenderFailed, "typst: unclosed delimiter");

        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"code":"render_failed","message":"typst: unclosed delimiter"}"#
        );
        assert_eq!(serde_json::from_str::<ErrorResponse>(&json).unwrap(), err);
    }

    #[test]
    fn error_codes_serialize_snake_case() {
        for (code, expected) in [
            (ErrorCode::BadRequest, "\"bad_request\""),
            (ErrorCode::Unauthorized, "\"unauthorized\""),
            (ErrorCode::PayloadTooLarge, "\"payload_too_large\""),
            (ErrorCode::UnsupportedTarget, "\"unsupported_target\""),
            (ErrorCode::RenderFailed, "\"render_failed\""),
            (ErrorCode::Internal, "\"internal\""),
            (ErrorCode::Overloaded, "\"overloaded\""),
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
        }
    }

    #[test]
    fn an_unknown_code_decodes_instead_of_failing_the_body() {
        let err: ErrorResponse =
            serde_json::from_str(r#"{"code":"from_the_future","message":"m"}"#).unwrap();
        assert_eq!(err.code, ErrorCode::Unknown);
    }

    #[test]
    fn detail_is_omitted_when_empty_and_carried_when_present() {
        let with = ErrorResponse::new(ErrorCode::RenderFailed, "m")
            .with_detail(vec!["main.typ:1:2: error: x".into()]);

        let json = serde_json::to_string(&with).unwrap();
        assert_eq!(
            json,
            r#"{"code":"render_failed","message":"m","detail":["main.typ:1:2: error: x"]}"#
        );
        assert_eq!(serde_json::from_str::<ErrorResponse>(&json).unwrap(), with);
        let old: ErrorResponse =
            serde_json::from_str(r#"{"code":"render_failed","message":"m"}"#).unwrap();
        assert!(old.detail.is_empty());
    }
}
