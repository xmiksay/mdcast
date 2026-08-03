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
}

impl ErrorResponse {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Machine-readable failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The body did not parse, or a field is out of range.
    BadRequest,
    /// The server was built without the feature this target/format needs
    /// (e.g. `html` without `typst-html`), or lacks the `pandoc` binary.
    UnsupportedTarget,
    /// The pipeline ran and failed — bad markdown, a template that does not
    /// compile, an engine error.
    RenderFailed,
    /// Anything the server cannot attribute to the request.
    Internal,
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
            (ErrorCode::UnsupportedTarget, "\"unsupported_target\""),
            (ErrorCode::RenderFailed, "\"render_failed\""),
            (ErrorCode::Internal, "\"internal\""),
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
        }
    }
}
