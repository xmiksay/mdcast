//! Brand specification: single source of truth for palette, fonts, margins, and
//! the auto-layout rules. v1 only models what the page-layout system needs;
//! richer projection (palette → typst/CSS/reference docs) lands in Phase 4.
//!
//! Type definitions only by default, so this crate stays free of
//! `toml`/`anyhow`. `mdcast::brand::from_toml` is the `anyhow`-wrapped loader
//! for `mdcast` consumers; a caller that must not compile `mdcast` (issue
//! #80) can instead enable this crate's optional `toml` feature and call
//! [`BrandSpec::from_toml`] directly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub auto_layout: AutoLayout,
    #[serde(default)]
    pub palette: BTreeMap<String, String>,
    #[serde(default)]
    pub fonts: BTreeMap<String, String>,
    /// Optional logo overlaid on every reveal.js slide (issue #57). `None`
    /// (the default, and what an existing `brand.toml` with no `[logo]`
    /// table parses to) means no overlay — pandoc's html-reveal arm skips
    /// the fetch/embed entirely, so output stays byte-identical to before
    /// this field existed.
    #[serde(default)]
    pub logo: Option<LogoSpec>,
}

#[cfg(feature = "toml")]
impl BrandSpec {
    /// Parse a `brand.toml` document. Requires the `toml` feature.
    ///
    /// This is the supported no-`mdcast` path: `mdcast::brand::from_toml`
    /// wraps the same parse in `anyhow::Context` for CLI/library callers,
    /// but a caller that must not compile `mdcast` (e.g. one built on
    /// `mdcast-client`) can enable `mdcast-api`'s `toml` feature and call
    /// this directly instead of relying on the undocumented fact that
    /// `BrandSpec` derives `Deserialize`.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// A brand logo to overlay on every reveal.js slide. `key` is an
/// `AssetProvider` key — the same namespace page-body image refs use — fetched
/// by the pandoc backend and embedded as a data URI (see
/// `backends/reveal_brand.rs`). Typst layouts have their own logo mechanism
/// (`ResolvedDoc.assets` + `asset-path`) and ignore this field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogoSpec {
    pub key: String,
    #[serde(default)]
    pub position: LogoPosition,
    pub width: Option<String>,
}

/// Which corner of the slide a logo overlay anchors to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogoPosition {
    #[default]
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoLayout {
    /// Class for the first page if no explicit class is set. `None` disables.
    pub first: Option<String>,
    /// Class for the last page if no explicit class is set. `None` disables.
    pub last: Option<String>,
    /// Class used when no rule matches.
    pub default: String,
    /// Content-shape rules, evaluated in order; first match wins.
    #[serde(default)]
    pub rules: Vec<AutoRule>,
}

impl Default for AutoLayout {
    fn default() -> Self {
        Self {
            first: Some("hero".to_string()),
            last: Some("thanks".to_string()),
            default: "content".to_string(),
            rules: vec![
                AutoRule {
                    when: ShapePredicate::SingleH1Only,
                    class: "section-divider".to_string(),
                },
                AutoRule {
                    when: ShapePredicate::SingleImageOnly,
                    class: "image-full".to_string(),
                },
                AutoRule {
                    when: ShapePredicate::SingleBlockquoteOnly,
                    class: "callout".to_string(),
                },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRule {
    pub when: ShapePredicate,
    pub class: String,
}

/// Closed predicate set in v1. Extending this is intentionally a code change,
/// not a config knob — keeps the surface area small until a real need surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapePredicate {
    Empty,
    SingleH1Only,
    SingleImageOnly,
    SingleBlockquoteOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_layout_default_rule_set() {
        let auto = AutoLayout::default();

        assert_eq!(auto.first, Some("hero".to_string()));
        assert_eq!(auto.last, Some("thanks".to_string()));
        assert_eq!(auto.default, "content");
        assert_eq!(
            auto.rules,
            vec![
                AutoRule {
                    when: ShapePredicate::SingleH1Only,
                    class: "section-divider".to_string(),
                },
                AutoRule {
                    when: ShapePredicate::SingleImageOnly,
                    class: "image-full".to_string(),
                },
                AutoRule {
                    when: ShapePredicate::SingleBlockquoteOnly,
                    class: "callout".to_string(),
                },
            ]
        );
    }

    #[test]
    fn brand_spec_round_trips_through_json() {
        let spec = BrandSpec {
            name: "Acme".to_string(),
            auto_layout: AutoLayout::default(),
            palette: BTreeMap::from([("primary".to_string(), "#112233".to_string())]),
            fonts: BTreeMap::from([("body".to_string(), "Inter".to_string())]),
            logo: Some(LogoSpec {
                key: "img/logo.svg".to_string(),
                position: LogoPosition::BottomLeft,
                width: Some("120px".to_string()),
            }),
        };

        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<BrandSpec>(&json).unwrap(), spec);
    }

    #[test]
    fn brand_spec_deserializes_from_an_empty_object() {
        let spec: BrandSpec = serde_json::from_str("{}").expect("every field defaults");
        assert_eq!(spec, BrandSpec::default());
        assert!(spec.logo.is_none());
    }

    #[test]
    fn logo_position_serializes_kebab_case() {
        let json = serde_json::to_string(&LogoPosition::BottomRight).unwrap();
        assert_eq!(json, "\"bottom-right\"");
        assert_eq!(
            serde_json::from_str::<LogoPosition>(&json).unwrap(),
            LogoPosition::BottomRight
        );
    }

    #[test]
    fn shape_predicate_serializes_snake_case() {
        let json = serde_json::to_string(&ShapePredicate::SingleH1Only).unwrap();
        assert_eq!(json, "\"single_h1_only\"");
        assert_eq!(
            serde_json::from_str::<ShapePredicate>(&json).unwrap(),
            ShapePredicate::SingleH1Only
        );
    }

    #[cfg(feature = "toml")]
    #[test]
    fn from_toml_parses_a_full_brand_toml_without_mdcast() {
        let toml = r##"
            name = "Acme"

            [auto_layout]
            first = "hero"
            last = "thanks"
            default = "content"

            [palette]
            primary = "#112233"

            [fonts]
            body = "Inter"

            [logo]
            key = "img/logo.svg"
            position = "bottom-left"
            width = "120px"
        "##;

        let spec = BrandSpec::from_toml(toml).unwrap();

        assert_eq!(spec.name, "Acme");
        assert_eq!(spec.auto_layout.first, Some("hero".to_string()));
        assert_eq!(spec.palette.get("primary"), Some(&"#112233".to_string()));
        assert_eq!(spec.fonts.get("body"), Some(&"Inter".to_string()));
        let logo = spec.logo.expect("logo table should parse");
        assert_eq!(logo.key, "img/logo.svg");
        assert_eq!(logo.position, LogoPosition::BottomLeft);
        assert_eq!(logo.width.as_deref(), Some("120px"));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn from_toml_defaults_missing_fields() {
        let spec = BrandSpec::from_toml("name = \"Acme\"").unwrap();
        assert_eq!(
            spec,
            BrandSpec {
                name: "Acme".to_string(),
                ..BrandSpec::default()
            }
        );
    }

    #[cfg(feature = "toml")]
    #[test]
    fn from_toml_rejects_invalid_syntax() {
        assert!(BrandSpec::from_toml("not = [valid toml").is_err());
    }
}
