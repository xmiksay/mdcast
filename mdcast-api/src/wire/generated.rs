//! Server-side generated images — a QR code today — that a template render
//! asks for by *content* instead of uploading as a blob.
//!
//! A [`GeneratedAsset`] names the virtual path the template references
//! (`#image("qr.svg")`) and a [`GeneratedSpec`] saying what to draw there. The
//! server renders it with the same code the markdown pre-step uses (`qr`
//! fences and `<qr>` tags), so a caller needs no QR library of its own.

use serde::{Deserialize, Serialize};

/// One generated file: `path` is where the template finds it, the flattened
/// `spec` is what it contains.
///
/// ```json
/// {"path": "qr.svg", "kind": "qr", "data": "SPD*1.0*ACC:CZ65…", "ec": "M"}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedAsset {
    /// Virtual path the template reads it under. Must not collide with a
    /// manifest key — the server rejects that rather than pick a winner.
    pub path: String,
    #[serde(flatten)]
    pub spec: GeneratedSpec,
}

/// What to generate. Non-exhaustive: new kinds arrive as new variants, and
/// [`super::CapabilitiesResponse::generators`] says which ones a server has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum GeneratedSpec {
    /// A QR code encoding `data` verbatim, as SVG.
    Qr {
        data: String,
        #[serde(default)]
        ec: QrEcLevel,
    },
}

/// QR error-correction level. `M` is the default, and what SPAYD ("QR
/// platba") recommends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QrEcLevel {
    L,
    #[default]
    M,
    Q,
    H,
}

/// The generator kinds a server can render — the values of
/// [`super::CapabilitiesResponse::generators`]. Also the markdown fence/tag
/// name each one answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Generator {
    Qr,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_entry_is_flat_json_tagged_by_kind() {
        let asset = GeneratedAsset {
            path: "qr.svg".to_string(),
            spec: GeneratedSpec::Qr {
                data: "SPD*1.0".to_string(),
                ec: QrEcLevel::H,
            },
        };
        let json = serde_json::to_value(&asset).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"path": "qr.svg", "kind": "qr", "data": "SPD*1.0", "ec": "H"})
        );
        assert_eq!(
            serde_json::from_value::<GeneratedAsset>(json).unwrap(),
            asset
        );
    }

    #[test]
    fn ec_defaults_to_m() {
        let asset: GeneratedAsset =
            serde_json::from_str(r#"{"path": "a.svg", "kind": "qr", "data": "x"}"#).unwrap();
        assert_eq!(
            asset.spec,
            GeneratedSpec::Qr {
                data: "x".to_string(),
                ec: QrEcLevel::M
            }
        );
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let err = serde_json::from_str::<GeneratedAsset>(
            r#"{"path": "a.svg", "kind": "barcode", "data": "x"}"#,
        );
        assert!(err.is_err());
    }
}
