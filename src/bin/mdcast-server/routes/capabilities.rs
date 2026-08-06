//! `GET /v1/capabilities` (issue #81) — like `/health`, deliberately outside
//! the `require_bearer` layer (see `mod.rs`'s `router`). It discloses only
//! deployment shape — which engines are installed, configured limits —
//! nothing about cached content or callers, so a client can feature-gate its
//! UI before it even has credentials wired up, and avoid a doomed upload
//! negotiation against a target the server can't actually produce.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use mdcast::backends::typst::TypstCompiler;
use mdcast::mdcast_api::Target;
use mdcast::mdcast_api::wire::{CapabilitiesResponse, TemplateFormat};

use super::AppState;

pub async fn capabilities(State(state): State<Arc<AppState>>) -> Json<CapabilitiesResponse> {
    let mut targets = vec![Target::Pdf, Target::PdfPresentation];
    if state.pandoc_available {
        targets.extend([Target::Docx, Target::Odt, Target::Pptx, Target::HtmlReveal]);
    }

    let mut template_formats = vec![TemplateFormat::Pdf];
    if html_template_export_available(&state.typst_compiler) {
        template_formats.push(TemplateFormat::Html);
    }

    Json(CapabilitiesResponse {
        targets,
        template_formats,
        max_upload_bytes: state.max_upload_bytes,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Mirrors `template.rs`'s own gate on `format: "html"`: `Cli` mode always
/// supports `--features html` regardless of how mdcast itself was built;
/// `Library` mode needs the `typst-html` cargo feature.
fn html_template_export_available(typst_compiler: &TypstCompiler) -> bool {
    matches!(typst_compiler, TypstCompiler::Cli(_)) || cfg!(feature = "typst-html")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_mode_needs_the_typst_html_cargo_feature() {
        let available = html_template_export_available(&TypstCompiler::Library);
        assert_eq!(available, cfg!(feature = "typst-html"));
    }

    #[test]
    fn cli_mode_always_supports_html_export() {
        let cli = TypstCompiler::Cli(mdcast::backends::typst::CliCompiler::new(
            "typst",
            std::time::Duration::from_secs(1),
        ));
        assert!(html_template_export_available(&cli));
    }
}
