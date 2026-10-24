//! Resource-reference scrubbing for the CSS html-reveal hands pandoc.
//!
//! Under `--embed-resources` pandoc inlines every `url(...)` and `@import`
//! it finds in a stylesheet — and it resolves a bare path against the host
//! filesystem, so a caller-supplied `revealjs/brand.css` (or an overridden
//! `revealjs/dist/**.css`) holding `url(/etc/hostname)` would ship that file
//! inside the deck. Every reference that is not provably harmless is removed
//! before pandoc sees the CSS:
//!
//! * `data:` is always kept — it carries its own bytes.
//! * `http(s):` is kept only with the `remote-images` feature, the one build
//!   where fetching at render time is opted into; otherwise pandoc would make
//!   the request itself, from the server.
//! * A relative path is kept only for the reveal.js dist (`allow_relative`),
//!   and only one that cannot leave the directory it was materialised into:
//!   no leading `/`, no `..` component, no scheme, no `\` and no `%` escape.
//!
//! A removed `url(...)` becomes `none`; a removed `@import "…"` loses the
//! whole statement. Deliberately a scanner, not a CSS parser: it only has to
//! find what pandoc's own (equally simple) scan would find.

/// Scrubs `css` as described in the module docs. `origin` names the source in
/// the warning each removal logs.
pub(super) fn sanitize_css(css: &str, allow_relative: bool, origin: &str) -> String {
    let css = strip_imports(css, allow_relative, origin);
    strip_urls(&css, allow_relative, origin)
}

/// Makes `css` safe to place inside an inline `<style>` element: no `</` can
/// close it early (and smuggle in an `<img src>` pandoc would embed), and no
/// `&` survives for pandoc's HTML parser to decode — it decodes character
/// references inside `<style>`, so `url(&#47;etc&#47;passwd)` would read as a
/// path only *after* [`sanitize_css`] had looked at it. `\26 ` is CSS's own
/// escape for `&`; the cost is that a CSS-nesting `&` selector stops working.
pub(super) fn inline_style_safe(css: &str) -> String {
    css.replace("</", "<\\/").replace('&', "\\26 ")
}

fn allowed(url: &str, allow_relative: bool) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("data:") {
        return true;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return cfg!(feature = "remote-images");
    }
    allow_relative
        && !url.is_empty()
        && !url.starts_with('/')
        && !url.contains(['\\', '%', ':'])
        && !url.split('/').any(|part| part == "..")
}

/// Removes every `@import "<string>"` whose target is not [`allowed`]. The
/// `@import url(...)` form is left to [`strip_urls`].
fn strip_imports(css: &str, allow_relative: bool, origin: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = find_ci(rest, "@import") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "@import".len()..];
        let target = after.trim_start();
        match quoted(target) {
            Some((url, _)) if !allowed(url, allow_relative) => {
                report(
                    allow_relative,
                    origin,
                    url,
                    "dropping a CSS @import pandoc would embed",
                );
                rest = after.find(';').map_or("", |end| &after[end + 1..]);
            }
            _ => {
                out.push_str("@import");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replaces every `url(...)` whose target is not [`allowed`] with `none`.
fn strip_urls(css: &str, allow_relative: bool, origin: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = find_ci(rest, "url(") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "url(".len()..];
        let (url, consumed) = url_argument(after);
        if allowed(url, allow_relative) {
            out.push_str(&rest[at..at + "url(".len() + consumed]);
        } else {
            report(
                allow_relative,
                origin,
                url,
                "dropping a CSS url() pandoc would embed",
            );
            out.push_str("none");
        }
        rest = &after[consumed..];
    }
    out.push_str(rest);
    out
}

/// The argument of a `url(` whose opening parenthesis has been consumed, and
/// how many bytes of `s` it spans including the closing `)` — all of `s` when
/// there is none, so an unterminated `url(` swallows the rest.
fn url_argument(s: &str) -> (&str, usize) {
    let lead = s.len() - s.trim_start().len();
    let body = &s[lead..];
    if let Some((url, len)) = quoted(body) {
        let tail = &body[len..];
        let end = tail.find(')').map_or(tail.len(), |i| i + 1);
        return (url, lead + len + end);
    }
    match body.find(')') {
        Some(i) => (body[..i].trim_end(), lead + i + 1),
        None => (body.trim_end(), s.len()),
    }
}

/// A leading `"…"`/`'…'` string: its contents and its length including the
/// quotes. An unterminated string runs to the end of `s`.
fn quoted(s: &str) -> Option<(&str, usize)> {
    let quote = s.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let inner = &s[1..];
    let mut escaped = false;
    for (i, c) in inner.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            c if c == quote => return Some((&inner[..i], i + 2)),
            _ => {}
        }
    }
    Some((inner, s.len()))
}

/// A scrubbed caller brand block warns; the reveal.js dist logs at debug,
/// because the vendored `league` theme's web-font `@import` would otherwise
/// warn on every html-reveal render, theme in use or not.
fn report(dist: bool, origin: &str, url: &str, what: &str) {
    if dist {
        tracing::debug!(origin, url, "{what}");
    } else {
        tracing::warn!(origin, url, "{what}");
    }
}

/// ASCII-case-insensitive `find`; lowercasing ASCII keeps byte offsets.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand(css: &str) -> String {
        sanitize_css(css, false, "test")
    }

    #[test]
    fn host_paths_in_url_are_replaced_with_none() {
        for css in [
            "a { background: url(/etc/passwd); }",
            "a { background: url('file:///etc/passwd'); }",
            "a { background: url(\"../x\"); }",
            "a { background: URL( /etc/passwd ); }",
            "a { background: url(x.png); }",
        ] {
            let out = brand(css);
            assert_eq!(out, "a { background: none; }", "{css}");
        }
    }

    #[test]
    fn data_urls_are_kept() {
        let css = "a { background: url(data:image/png;base64,AAAA); }";
        assert_eq!(brand(css), css);
        let quoted = "a { background: url('data:image/svg+xml;utf8,<svg/>'); }";
        assert_eq!(brand(quoted), quoted);
    }

    #[test]
    fn remote_urls_follow_the_remote_images_feature() {
        let css = "@import url(https://fonts.example/x.css);";
        let expected = if cfg!(feature = "remote-images") {
            css.to_string()
        } else {
            "@import none;".to_string()
        };
        assert_eq!(brand(css), expected);
    }

    #[test]
    fn string_imports_of_host_paths_lose_the_whole_statement() {
        assert_eq!(brand("@import \"/etc/x\";\na{}"), "\na{}");
        assert_eq!(brand("@IMPORT '/etc/x' screen; a{}"), " a{}");
        assert_eq!(brand("@import \"/etc/x\""), "");
    }

    #[test]
    fn relative_paths_are_kept_only_for_the_dist_and_only_inside_it() {
        let ok = "@font-face { src: url(fonts/a.woff); } @import \"theme/b.css\";";
        assert_eq!(sanitize_css(ok, true, "dist"), ok);
        for bad in [
            "url(../../etc/passwd)",
            "url(/etc/passwd)",
            "url(fonts/%2e%2e/x)",
            "url(fonts\\..\\x)",
            "url(file:x)",
        ] {
            assert_eq!(sanitize_css(bad, true, "dist"), "none", "{bad}");
        }
        assert_eq!(
            sanitize_css(ok, false, "brand"),
            "@font-face { src: none; } "
        );
    }

    #[test]
    fn an_unterminated_url_swallows_the_rest() {
        assert_eq!(brand("a { b: url(/etc/passwd"), "a { b: none");
    }

    #[test]
    fn inline_style_safe_closes_no_tags_and_leaves_no_entities() {
        let out = inline_style_safe("a{b:url(&#47;etc)}</style><img src=/etc/x>");
        assert!(!out.contains("</"), "{out}");
        assert!(!out.contains('&'), "{out}");
    }
}
