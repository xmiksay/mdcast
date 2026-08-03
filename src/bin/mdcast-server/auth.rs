//! Bearer-token gate for everything under `/v1`. One shared token from the
//! environment — the trust boundary is "internal callers", not a user
//! directory, so there is no session, no user model, and nothing to revoke
//! individually.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use subtle::ConstantTimeEq;

use crate::error::ApiError;

pub async fn require_bearer(
    State(token): State<Arc<str>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    if is_authorized(presented, &token) {
        Ok(next.run(request).await)
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// Pure half of the middleware: does this `Authorization` header value carry
/// `token`? The scheme is matched case-insensitively (RFC 7235 says auth
/// schemes are), the token itself in constant time — a byte-by-byte `==` on
/// a shared secret is a timing oracle.
fn is_authorized(header: Option<&str>, token: &str) -> bool {
    let Some((scheme, presented)) = header.and_then(|h| h.split_once(' ')) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return false;
    }
    presented.trim().as_bytes().ct_eq(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "correct-horse-battery-staple";

    #[test]
    fn only_the_exact_token_behind_a_bearer_scheme_is_accepted() {
        assert!(is_authorized(Some(&format!("Bearer {TOKEN}")), TOKEN));
        // RFC 7235: the scheme is case-insensitive, the credential is not.
        assert!(is_authorized(Some(&format!("bearer {TOKEN}")), TOKEN));
        assert!(is_authorized(Some(&format!("Bearer  {TOKEN} ")), TOKEN));
    }

    #[test]
    fn everything_else_is_rejected() {
        for header in [
            None,
            Some(""),
            Some("Bearer"),
            Some(TOKEN),
            Some("Basic dXNlcjpwYXNz"),
            Some("Bearer "),
            Some("Bearer wrong-token"),
            // A prefix of the real token must not pass — the comparison is
            // over the whole value, not a `starts_with`.
            Some("Bearer correct-horse"),
            Some("Bearer correct-horse-battery-staple-and-more"),
        ] {
            assert!(
                !is_authorized(header.map(|h| h as &str), TOKEN),
                "accepted {header:?}"
            );
        }
    }
}
