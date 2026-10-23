//! Server-side generated images — QR codes and chess boards — that a
//! template render asks for by *content* instead of uploading as a blob.
//!
//! A [`GeneratedAsset`] names the virtual path the template references
//! (`#image("qr.svg")`) and a [`GeneratedSpec`] saying what to draw there. The
//! server renders it with the same code the markdown pre-step uses (`qr`,
//! `fen` and `pgn` fences and tags), so a caller needs no QR or chess library
//! of its own.

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
    /// A chess board showing the position `fen` (full FEN or just its
    /// placement field), as SVG.
    ///
    /// ```json
    /// {"path": "board.svg", "kind": "fen", "fen": "8/8/8/4k3/8/8/8/4K3", "flip": true}
    /// ```
    Fen {
        fen: String,
        #[serde(flatten)]
        board: BoardOptions,
    },
    /// A chess board showing a PGN game's mainline after half-move `move`
    /// (`0` = the starting position); absent means the final position.
    Pgn {
        pgn: String,
        #[serde(default, rename = "move")]
        ply: Option<usize>,
        #[serde(flatten)]
        board: BoardOptions,
    },
}

/// How a `fen`/`pgn` board is drawn — the same knobs the markdown fences and
/// tags take as attributes. Every field is optional on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BoardOptions {
    /// Draw the board from Black's side.
    pub flip: bool,
    pub theme: BoardTheme,
    /// File/rank labels along the edges.
    pub coordinates: bool,
}

impl Default for BoardOptions {
    fn default() -> Self {
        Self {
            flip: false,
            theme: BoardTheme::default(),
            coordinates: true,
        }
    }
}

/// Board square colours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoardTheme {
    #[default]
    Brown,
    Blue,
    Green,
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
    Fen,
    Pgn,
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
    fn chess_entries_are_flat_json_with_wire_name_move() {
        let asset = GeneratedAsset {
            path: "game.svg".to_string(),
            spec: GeneratedSpec::Pgn {
                pgn: "1. e4 e5".to_string(),
                ply: Some(1),
                board: BoardOptions {
                    flip: true,
                    theme: BoardTheme::Green,
                    coordinates: false,
                },
            },
        };
        let json = serde_json::to_value(&asset).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "path": "game.svg", "kind": "pgn", "pgn": "1. e4 e5", "move": 1,
                "flip": true, "theme": "green", "coordinates": false
            })
        );
        assert_eq!(
            serde_json::from_value::<GeneratedAsset>(json).unwrap(),
            asset
        );
    }

    #[test]
    fn chess_board_options_default() {
        let fen: GeneratedAsset =
            serde_json::from_str(r#"{"path": "a.svg", "kind": "fen", "fen": "8/8/8/8/8/8/8/4K3"}"#)
                .unwrap();
        assert_eq!(
            fen.spec,
            GeneratedSpec::Fen {
                fen: "8/8/8/8/8/8/8/4K3".to_string(),
                board: BoardOptions {
                    flip: false,
                    theme: BoardTheme::Brown,
                    coordinates: true,
                },
            }
        );
        let pgn: GeneratedAsset =
            serde_json::from_str(r#"{"path": "a.svg", "kind": "pgn", "pgn": "1. e4"}"#).unwrap();
        assert_eq!(
            pgn.spec,
            GeneratedSpec::Pgn {
                pgn: "1. e4".to_string(),
                ply: None,
                board: BoardOptions::default(),
            }
        );
    }

    #[test]
    fn unknown_board_theme_is_rejected() {
        let err = serde_json::from_str::<GeneratedAsset>(
            r#"{"path": "a.svg", "kind": "fen", "fen": "8/8/8/8/8/8/8/8", "theme": "pink"}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn generators_serialize_kebab_case() {
        assert_eq!(
            serde_json::to_value([Generator::Qr, Generator::Fen, Generator::Pgn]).unwrap(),
            serde_json::json!(["qr", "fen", "pgn"])
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
