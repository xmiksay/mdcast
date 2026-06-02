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
                AutoRule { when: ShapePredicate::SingleH1Only, class: "section-divider".to_string() },
                AutoRule { when: ShapePredicate::SingleImageOnly, class: "image-full".to_string() },
                AutoRule { when: ShapePredicate::SingleBlockquoteOnly, class: "callout".to_string() },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
