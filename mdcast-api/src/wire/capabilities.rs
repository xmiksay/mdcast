//! `GET /v1/capabilities` response body (issue #81) — deployment shape a
//! client can use to feature-gate its UI before it has even wired up
//! credentials, since the route sits outside the bearer gate.

use serde::{Deserialize, Serialize};

use crate::doc::Target;
use crate::wire::generated::Generator;
use crate::wire::render::TemplateFormat;

/// What this `mdcast-server` deployment can actually render right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    /// Targets `POST /v1/render` can produce. The typst-backed entries
    /// (`pdf`, `pdf-presentation`) are always present; the pandoc-backed ones
    /// (`docx`, `odt`, `pptx`, `html-reveal`) appear only when the server
    /// found a working `pandoc` binary at startup.
    pub targets: Vec<Target>,
    /// Formats `POST /v1/render/template` can produce. `pdf` is always
    /// present; `html` appears only when this deployment can actually export
    /// it — the `typst-html` build feature, or `MDCAST_TYPST=cli`.
    pub template_formats: Vec<TemplateFormat>,
    /// Generator kinds this build renders — in markdown (`RenderOptions`
    /// flags) and as `RenderTemplateRequest.generated` entries. Empty from a
    /// server older than the field.
    #[serde(default)]
    pub generators: Vec<Generator>,
    /// Echoes `MDCAST_MAX_UPLOAD_BYTES` — the ceiling on every request body,
    /// not just uploads — so a client can reject an oversized bundle before
    /// any round trip instead of discovering the limit via `413`.
    pub max_upload_bytes: u64,
    /// The running server's crate version (`CARGO_PKG_VERSION`).
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let res = CapabilitiesResponse {
            targets: vec![Target::Pdf, Target::PdfPresentation, Target::Docx],
            template_formats: vec![TemplateFormat::Pdf, TemplateFormat::Html],
            generators: vec![Generator::Qr, Generator::Fen, Generator::Pgn],
            max_upload_bytes: 268_435_456,
            version: "0.3.0".to_string(),
        };

        let json = serde_json::to_string(&res).unwrap();
        assert_eq!(
            serde_json::from_str::<CapabilitiesResponse>(&json).unwrap(),
            res
        );
    }

    #[test]
    fn targets_serialize_as_their_kebab_case_strings() {
        let res = CapabilitiesResponse {
            targets: vec![Target::HtmlReveal],
            template_formats: vec![],
            generators: vec![],
            max_upload_bytes: 0,
            version: String::new(),
        };

        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["targets"], serde_json::json!(["html-reveal"]));
    }
}
