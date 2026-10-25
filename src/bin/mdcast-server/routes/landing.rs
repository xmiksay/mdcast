//! `GET /` (issue #99) — a static page telling whoever opens the service in
//! a browser what is running there. Unauthenticated like `/health`, and
//! deliberately free of deployment detail (token state, config, limits): the
//! only dynamic value is the crate version, which `/v1/capabilities` already
//! exposes publicly.

use std::sync::LazyLock;

use axum::http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use axum::response::IntoResponse;

/// The page inlines all of its CSS and SVG, so nothing else is needed —
/// `default-src 'none'` also blocks any script, frame or outbound request.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; \
     base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

static PAGE: LazyLock<String> = LazyLock::new(|| {
    include_str!("landing.html").replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
});

pub async fn landing() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "text/html; charset=utf-8"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (CONTENT_SECURITY_POLICY, CSP),
        ],
        PAGE.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_placeholder_is_filled_and_nothing_is_fetched() {
        assert!(!PAGE.contains("{{VERSION}}"));
        assert!(PAGE.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
        // Only the repository link may point off-host; anything else (a
        // font, script or image URL) would break the no-egress promise.
        assert_eq!(PAGE.matches("://").count(), 1, "unexpected URL in page");
        assert!(PAGE.contains(r#"href="https://github.com/xmiksay/mdcast""#));
        assert!(!PAGE.contains("<script"));
    }
}
