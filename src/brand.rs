//! Brand specification: single source of truth for palette, fonts, margins, and
//! the auto-layout rules. v1 only models what the page-layout system needs;
//! richer projection (palette → typst/CSS/reference docs) lands in Phase 4.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrandSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub auto_layout: AutoLayout,
    #[serde(default)]
    pub palette: BTreeMap<String, String>,
    #[serde(default)]
    pub fonts: BTreeMap<String, String>,
}

impl BrandSpec {
    pub fn from_toml(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn from_toml_parses_valid_spec() {
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
        "##;

        let spec = BrandSpec::from_toml(toml).unwrap();

        assert_eq!(spec.name, "Acme");
        assert_eq!(spec.auto_layout.first, Some("hero".to_string()));
        assert_eq!(spec.palette.get("primary"), Some(&"#112233".to_string()));
        assert_eq!(spec.fonts.get("body"), Some(&"Inter".to_string()));
    }

    #[test]
    fn from_toml_rejects_invalid_syntax() {
        let err = BrandSpec::from_toml("not = [valid toml").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

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
}
