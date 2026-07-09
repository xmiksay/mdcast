//! Brand → reveal.js CSS/HTML projection (issue #57). Pure string-building,
//! no I/O — `pandoc.rs`'s html-reveal arm fetches the logo bytes itself (via
//! the `AssetProvider`, the one sanctioned way backends reach files) and
//! passes them in here.

use crate::brand::{BrandSpec, LogoPosition, LogoSpec};

/// Map `BrandSpec.palette`/`.fonts` onto reveal.js 4.x theme CSS custom
/// properties scoped to `.reveal`, plus a `--brand-<key>` passthrough for
/// every palette key so per-class CSS (`.reveal section.hero { ... }`) can
/// reach colors the mapping below doesn't know about. `None` for a spec with
/// neither palette nor fonts set — the pandoc backend skips
/// `--include-in-header` entirely in that case, so an unbranded render stays
/// byte-identical to before this existed.
pub(crate) fn brand_css(spec: &BrandSpec) -> Option<String> {
    if spec.palette.is_empty() && spec.fonts.is_empty() {
        return None;
    }

    let mut vars = String::new();
    if let Some(v) = spec.palette.get("background") {
        push_var(&mut vars, "--r-background-color", v);
    }
    if let Some(v) = spec
        .palette
        .get("heading")
        .or_else(|| spec.palette.get("primary"))
    {
        push_var(&mut vars, "--r-heading-color", v);
    }
    if let Some(v) = spec.palette.get("text") {
        push_var(&mut vars, "--r-main-color", v);
    }
    if let Some(v) = spec.palette.get("link") {
        push_var(&mut vars, "--r-link-color", v);
        push_var(&mut vars, "--r-link-color-hover", v);
    }
    if let Some(v) = spec.palette.get("accent") {
        push_var(&mut vars, "--r-selection-background-color", v);
    }
    if let Some(v) = spec.fonts.get("body") {
        push_var(&mut vars, "--r-main-font", v);
    }
    if let Some(v) = spec.fonts.get("heading") {
        push_var(&mut vars, "--r-heading-font", v);
    }
    if let Some(v) = spec.fonts.get("code") {
        push_var(&mut vars, "--r-code-font", v);
    }
    for (key, value) in &spec.palette {
        push_var(&mut vars, &format!("--brand-{key}"), value);
    }

    Some(format!(".reveal {{\n{vars}}}\n"))
}

fn push_var(out: &mut String, name: &str, value: &str) {
    out.push_str("  ");
    out.push_str(name);
    out.push_str(": ");
    out.push_str(&escape_css_value(value));
    out.push_str(";\n");
}

/// Drop characters a `brand.toml` value could use to break out of the CSS
/// declaration it's spliced into, the enclosing `<style>` block, or (for
/// `LogoSpec.width`, the other caller) the `style="..."` HTML attribute
/// `logo_html` embeds it in: `{`/`}` close a rule early, `;` ends the
/// declaration early, `<`/`>` could close `<style>` and inject raw markup,
/// `"` could close the HTML attribute, and a literal newline lets one
/// `key = "value"` line masquerade as several. Palette/font values are
/// colors and font names in practice, so this never touches legitimate
/// input.
fn escape_css_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, '{' | '}' | ';' | '<' | '>' | '"' | '\n' | '\r'))
        .collect()
}

/// `<img>` overlay for a brand logo, positioned `fixed` in one corner of
/// every slide — reveal.js has no native persistent-chrome slot, and
/// pandoc's revealjs writer only understands slide content, so this is
/// injected once via `--include-after-body` instead. `z-index: 2` sits above
/// `.reveal .slides` (reveal.js's own default `z-index: 1`); `pointer-events:
/// none` keeps the overlay from intercepting slide-navigation clicks.
pub(crate) fn logo_html(spec: &LogoSpec, bytes: &[u8], mime: &str) -> String {
    let b64 = base64_encode(bytes);
    let (top, right, bottom, left) = corner_css(spec.position);
    let width = spec
        .width
        .as_deref()
        .map(|w| format!("width: {};", escape_css_value(w)))
        .unwrap_or_default();
    format!(
        "<img src=\"data:{mime};base64,{b64}\" style=\"position: fixed; {top}{right}{bottom}{left}{width} z-index: 2; pointer-events: none;\">\n"
    )
}

fn corner_css(position: LogoPosition) -> (&'static str, &'static str, &'static str, &'static str) {
    match position {
        LogoPosition::TopRight => ("top: 20px; ", "right: 20px; ", "", ""),
        LogoPosition::TopLeft => ("top: 20px; ", "", "", "left: 20px; "),
        LogoPosition::BottomRight => ("", "right: 20px; ", "bottom: 20px; ", ""),
        LogoPosition::BottomLeft => ("", "", "bottom: 20px; ", "left: 20px; "),
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard (padded) base64 encoding. Hand-rolled rather than pulling in a
/// dependency for a self-contained-HTML data URI — CSS/HTML generation here
/// is deliberately just string building (see the "Affected files" table in
/// issue #57: no new crates).
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[(n >> 18 & 0x3F) as usize] as char);
        out.push(BASE64_ALPHABET[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
#[path = "reveal_brand_tests.rs"]
mod tests;
