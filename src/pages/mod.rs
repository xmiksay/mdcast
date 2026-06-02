//! Page-aware IR for the doc body. Stage 1 (splitter) parses the surface
//! syntaxes into `RawPage`s; stage 2 (auto) assigns a `class` to produce
//! `Page`s ready for backends.

pub mod auto;
pub mod splitter;

use serde::{Deserialize, Serialize};

/// Output of stage 1. Class may be explicit (from `<page class="X">` or
/// `::: {.X}`) or absent (auto-classifier will fill it in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPage {
    pub explicit_class: Option<String>,
    pub body: String,
}

/// Output of stage 2. Always has a class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub class: String,
    pub body: String,
    pub origin: PageOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOrigin {
    /// Author wrote the class explicitly.
    Explicit,
    /// Class came from positional rule (first/last).
    AutoPositional,
    /// Class came from a content-shape predicate.
    AutoShape,
    /// Fallback `default` class.
    AutoDefault,
}
