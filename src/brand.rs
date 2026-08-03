//! Brand specification, re-exported from `mdcast-api`.
//!
//! The types themselves are pure data and live in `mdcast_api::brand` so a
//! client can speak the protocol without building mdcast. What stays here is
//! the part that cannot: [`from_toml`], which needs `toml` + `anyhow`. It is
//! a free function rather than `BrandSpec::from_toml` because an inherent
//! impl has to live in the crate that defines the type.

use anyhow::{Context, Result};

pub use mdcast_api::brand::{
    AutoLayout, AutoRule, BrandSpec, LogoPosition, LogoSpec, ShapePredicate,
};

/// Parse a `brand.toml` into a [`BrandSpec`].
pub fn from_toml(s: &str) -> Result<BrandSpec> {
    toml::from_str(s).context("invalid brand.toml")
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

        let spec = from_toml(toml).unwrap();

        assert_eq!(spec.name, "Acme");
        assert_eq!(spec.auto_layout.first, Some("hero".to_string()));
        assert_eq!(spec.palette.get("primary"), Some(&"#112233".to_string()));
        assert_eq!(spec.fonts.get("body"), Some(&"Inter".to_string()));
    }

    #[test]
    fn from_toml_rejects_invalid_syntax() {
        let err = from_toml("not = [valid toml").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn from_toml_without_logo_table_defaults_to_none() {
        let spec = from_toml("name = \"Acme\"").unwrap();
        assert!(spec.logo.is_none());
    }

    #[test]
    fn from_toml_parses_logo_table_with_explicit_position() {
        let toml = r##"
            name = "Acme"

            [logo]
            key = "img/logo.svg"
            position = "bottom-left"
            width = "120px"
        "##;

        let spec = from_toml(toml).unwrap();
        let logo = spec.logo.expect("logo table should parse");

        assert_eq!(logo.key, "img/logo.svg");
        assert_eq!(logo.position, LogoPosition::BottomLeft);
        assert_eq!(logo.width.as_deref(), Some("120px"));
    }

    #[test]
    fn logo_position_defaults_to_top_right() {
        let toml = r##"
            [logo]
            key = "img/logo.svg"
        "##;

        let spec = from_toml(toml).unwrap();
        let logo = spec.logo.expect("logo table should parse");

        assert_eq!(logo.position, LogoPosition::TopRight);
        assert_eq!(logo.width, None);
    }
}
