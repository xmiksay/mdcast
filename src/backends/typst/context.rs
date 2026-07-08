//! Builds `context.typ` — a synthetic Typst source exposing `ResolvedDoc.meta`,
//! `.brand`, and `.assets` to layouts as plain dictionaries. Registered as the
//! project-root file `/context.typ` alongside the per-class layout sources;
//! a layout opts in with `#import "/context.typ": doc-meta, brand` (plus the
//! accessor helpers below) and is otherwise unaffected — nothing is pushed
//! onto layouts that don't import it.
//!
//! `assets` maps each `ResolvedDoc.assets` key that the provider actually
//! resolved to the virtual path it was registered under with the typst
//! engine (see `collect_layout_assets` in `mod.rs`) — a layout looks it up by
//! the same key rather than needing to know the sanitized virtual path.
//!
//! Dict keys are always emitted as string literals (`"key": value`) rather
//! than bare identifiers so that arbitrary `DocMeta.extra` / palette / font
//! keys (which may contain characters invalid in a Typst identifier) can't
//! produce malformed Typst source. Field access (`doc-meta.title`) still
//! works against a string-keyed dict — `.field` is sugar for `.at("field")`
//! regardless of how the key was declared.

use std::collections::BTreeMap;

use crate::{BrandSpec, DocMeta};

use super::markdown::typst_string;

/// Virtual path the context source is registered under with the typst
/// engine. Layouts import it via the project-root-relative `/context.typ`.
pub const CONTEXT_VIRTUAL_PATH: &str = "context.typ";

pub fn build_context_source(
    meta: &DocMeta,
    brand: &BrandSpec,
    assets: &BTreeMap<String, String>,
) -> String {
    let mut s = String::new();

    s.push_str("#let doc-meta = (\n");
    s.push_str(&dict_entry("title", meta.title.as_deref().unwrap_or("")));
    s.push_str(&dict_entry("author", meta.author.as_deref().unwrap_or("")));
    s.push_str(&dict_entry("date", meta.date.as_deref().unwrap_or("")));
    for (k, v) in &meta.extra {
        s.push_str(&dict_entry(k, v));
    }
    s.push_str(")\n\n");

    s.push_str("#let brand = (\n");
    s.push_str(&dict_entry("name", &brand.name));
    s.push_str(&format!("  {}\n", nested_dict("palette", &brand.palette)));
    s.push_str(&format!("  {}\n", nested_dict("fonts", &brand.fonts)));
    s.push_str(")\n\n");

    s.push_str("#let assets = (\n");
    for (k, v) in assets {
        s.push_str(&dict_entry(k, v));
    }
    s.push_str(")\n\n");

    s.push_str(
        "#let doc-meta-get(key, default: \"\") = if key in doc-meta { doc-meta.at(key) } else { default }\n",
    );
    s.push_str(
        "#let brand-color(key, default: black) = if key in brand.palette { rgb(brand.palette.at(key)) } else { default }\n",
    );
    s.push_str(
        "#let brand-font(key, default: none) = if key in brand.fonts { brand.fonts.at(key) } else { default }\n",
    );
    s.push_str(
        "#let asset-path(key, default: none) = if key in assets { assets.at(key) } else { default }\n",
    );
    s
}

fn dict_entry(key: &str, value: &str) -> String {
    format!("  {}: {},\n", typst_string(key), typst_string(value))
}

fn nested_dict(key: &str, entries: &BTreeMap<String, String>) -> String {
    let mut s = format!("{}: (\n", typst_string(key));
    for (k, v) in entries {
        s.push_str(&format!("    {}: {},\n", typst_string(k), typst_string(v)));
    }
    s.push_str("  ),");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(title: Option<&str>, extra: &[(&str, &str)]) -> DocMeta {
        DocMeta {
            title: title.map(String::from),
            author: None,
            date: None,
            extra: extra
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn empty_meta_and_brand_produce_valid_defaults() {
        let out =
            build_context_source(&DocMeta::default(), &BrandSpec::default(), &BTreeMap::new());
        assert!(out.contains(r#""title": "","#));
        assert!(out.contains(r#""author": "","#));
        assert!(out.contains(r#""date": "","#));
        assert!(out.contains("#let doc-meta-get"));
        assert!(out.contains("#let brand-color"));
        assert!(out.contains("#let brand-font"));
        assert!(out.contains("#let assets = ("));
        assert!(out.contains("#let asset-path"));
    }

    #[test]
    fn extra_keys_flatten_into_doc_meta() {
        let meta = meta_with(Some("Q3 Review"), &[("classification", "internal")]);
        let out = build_context_source(&meta, &BrandSpec::default(), &BTreeMap::new());
        assert!(out.contains(r#""title": "Q3 Review","#));
        assert!(out.contains(r#""classification": "internal","#));
    }

    #[test]
    fn brand_palette_and_fonts_are_nested_dicts() {
        let brand = BrandSpec {
            name: "F13".to_string(),
            palette: BTreeMap::from([("navy".to_string(), "#243752".to_string())]),
            fonts: BTreeMap::from([("sans".to_string(), "Montserrat".to_string())]),
            ..Default::default()
        };
        let out = build_context_source(&DocMeta::default(), &brand, &BTreeMap::new());
        assert!(out.contains(r#""name": "F13","#));
        assert!(out.contains(r#""palette": ("#));
        assert!(out.contains(r##""navy": "#243752","##));
        assert!(out.contains(r#""fonts": ("#));
        assert!(out.contains(r#""sans": "Montserrat","#));
    }

    #[test]
    fn special_characters_in_values_are_escaped() {
        let meta = meta_with(Some("Title \"with\" quotes\nand a newline"), &[]);
        let out = build_context_source(&meta, &BrandSpec::default(), &BTreeMap::new());
        assert!(out.contains(r#"\"with\""#));
        assert!(out.contains(r"\n"));
        assert!(!out.contains("with\" quotes\nand"));
    }

    #[test]
    fn assets_dict_maps_key_to_virtual_path() {
        let assets = BTreeMap::from([("logo".to_string(), "/assets/logo.svg".to_string())]);
        let out = build_context_source(&DocMeta::default(), &BrandSpec::default(), &assets);
        assert!(out.contains(r#""logo": "/assets/logo.svg","#));
    }
}
