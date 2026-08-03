//! The document model: what to render (`DocMeta`) and what to render it into
//! (`Target`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Output target. One backend per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    Docx,
    Odt,
    Pdf,
    PdfPresentation,
    Pptx,
    HtmlReveal,
}

impl Target {
    pub fn as_str(&self) -> &'static str {
        match self {
            Target::Docx => "docx",
            Target::Odt => "odt",
            Target::Pdf => "pdf",
            Target::PdfPresentation => "pdf-presentation",
            Target::Pptx => "pptx",
            Target::HtmlReveal => "html-reveal",
        }
    }

    /// File extension (without the leading dot) for a suggested filename.
    pub fn extension(&self) -> &'static str {
        match self {
            Target::Docx => "docx",
            Target::Odt => "odt",
            Target::Pdf | Target::PdfPresentation => "pdf",
            Target::Pptx => "pptx",
            Target::HtmlReveal => "html",
        }
    }
}

/// Document-level metadata extracted from frontmatter / config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_serializes_as_its_kebab_case_str() {
        for target in [
            Target::Docx,
            Target::Odt,
            Target::Pdf,
            Target::PdfPresentation,
            Target::Pptx,
            Target::HtmlReveal,
        ] {
            let json = serde_json::to_string(&target).unwrap();
            assert_eq!(json, format!("\"{}\"", target.as_str()));
            assert_eq!(serde_json::from_str::<Target>(&json).unwrap(), target);
        }
    }

    #[test]
    fn target_extension_collapses_both_pdf_targets() {
        assert_eq!(Target::Pdf.extension(), "pdf");
        assert_eq!(Target::PdfPresentation.extension(), "pdf");
        assert_eq!(Target::HtmlReveal.extension(), "html");
    }

    #[test]
    fn doc_meta_round_trips_through_json() {
        let meta = DocMeta {
            title: Some("Q3 Review".to_string()),
            author: Some("Martin".to_string()),
            date: None,
            extra: BTreeMap::from([("subtitle".to_string(), "Board copy".to_string())]),
        };

        let json = serde_json::to_string(&meta).unwrap();
        assert_eq!(serde_json::from_str::<DocMeta>(&json).unwrap(), meta);
    }

    #[test]
    fn doc_meta_defaults_extra_when_absent() {
        let meta: DocMeta = serde_json::from_str(r#"{"title":"T","author":null,"date":null}"#)
            .expect("extra is optional");
        assert!(meta.extra.is_empty());
    }
}
