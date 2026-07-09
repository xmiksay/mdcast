use super::*;
use std::collections::BTreeMap;

fn spec_with(palette: &[(&str, &str)], fonts: &[(&str, &str)]) -> BrandSpec {
    BrandSpec {
        palette: palette
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        fonts: fonts
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        ..Default::default()
    }
}

#[test]
fn empty_spec_yields_no_css() {
    assert!(brand_css(&BrandSpec::default()).is_none());
}

#[test]
fn known_palette_keys_map_to_reveal_custom_properties() {
    let spec = spec_with(
        &[
            ("background", "#111111"),
            ("heading", "#222222"),
            ("text", "#333333"),
            ("link", "#444444"),
            ("accent", "#555555"),
        ],
        &[],
    );
    let css = brand_css(&spec).unwrap();

    assert!(css.contains("--r-background-color: #111111;"), "{css}");
    assert!(css.contains("--r-heading-color: #222222;"), "{css}");
    assert!(css.contains("--r-main-color: #333333;"), "{css}");
    assert!(css.contains("--r-link-color: #444444;"), "{css}");
    assert!(css.contains("--r-link-color-hover: #444444;"), "{css}");
    assert!(
        css.contains("--r-selection-background-color: #555555;"),
        "{css}"
    );
}

#[test]
fn primary_is_the_heading_fallback() {
    let spec = spec_with(&[("primary", "#666666")], &[]);
    let css = brand_css(&spec).unwrap();
    assert!(css.contains("--r-heading-color: #666666;"), "{css}");
}

#[test]
fn heading_key_wins_over_primary_fallback() {
    let spec = spec_with(&[("heading", "#aaaaaa"), ("primary", "#bbbbbb")], &[]);
    let css = brand_css(&spec).unwrap();
    // `--r-heading-color` maps from `heading`, not `primary` — `#bbbbbb`
    // still shows up via the unconditional `--brand-primary` passthrough,
    // just never as `--r-heading-color`.
    assert!(css.contains("--r-heading-color: #aaaaaa;"), "{css}");
    assert!(!css.contains("--r-heading-color: #bbbbbb;"), "{css}");
}

#[test]
fn known_font_keys_map_to_reveal_custom_properties() {
    let spec = spec_with(
        &[],
        &[
            ("body", "Inter"),
            ("heading", "Poppins"),
            ("code", "Fira Code"),
        ],
    );
    let css = brand_css(&spec).unwrap();

    assert!(css.contains("--r-main-font: Inter;"), "{css}");
    assert!(css.contains("--r-heading-font: Poppins;"), "{css}");
    assert!(css.contains("--r-code-font: Fira Code;"), "{css}");
}

#[test]
fn every_palette_key_also_emits_brand_passthrough() {
    let spec = spec_with(&[("navy", "#243752"), ("accent", "#ff8800")], &[]);
    let css = brand_css(&spec).unwrap();

    assert!(css.contains("--brand-navy: #243752;"), "{css}");
    assert!(css.contains("--brand-accent: #ff8800;"), "{css}");
}

#[test]
fn css_is_scoped_to_reveal_selector() {
    let spec = spec_with(&[("accent", "#ff8800")], &[]);
    let css = brand_css(&spec).unwrap();
    assert!(css.starts_with(".reveal {\n"), "{css}");
    assert!(css.trim_end().ends_with('}'), "{css}");
}

#[test]
fn palette_value_cannot_break_out_of_declaration_block() {
    let spec = spec_with(&[("accent", "#fff; } body { display: none")], &[]);
    let css = brand_css(&spec).unwrap();
    // Only the closing brace of the enclosing `.reveal { ... }` rule (and its
    // matching open brace) should survive — the value's own `{`/`}`/`;` must
    // be stripped so it can't terminate the declaration early or open a new
    // rule of its own.
    assert_eq!(css.matches('{').count(), 1, "{css}");
    assert_eq!(css.matches('}').count(), 1, "{css}");
}

#[test]
fn palette_value_cannot_inject_newline_or_html() {
    let spec = spec_with(&[("accent", "red\n</style><script>evil()</script>")], &[]);
    let css = brand_css(&spec).unwrap();
    // Every declaration line still ends `;` and only real declarations
    // introduce a newline — the value's own `<`/`>`/`\n` are stripped so it
    // can't close the enclosing `<style>` tag or fake extra lines.
    assert!(!css.contains('<'), "{css}");
    assert!(!css.contains('>'), "{css}");
    for line in css
        .lines()
        .filter(|l| !l.trim().is_empty() && *l != ".reveal {" && *l != "}")
    {
        assert!(line.trim_end().ends_with(';'), "{css}");
    }
}

fn logo(position: LogoPosition, width: Option<&str>) -> LogoSpec {
    LogoSpec {
        key: "img/logo.svg".into(),
        position,
        width: width.map(str::to_string),
    }
}

#[test]
fn logo_html_embeds_data_uri() {
    let html = logo_html(&logo(LogoPosition::TopRight, None), b"AB", "image/png");
    // base64("AB") == "QUI="
    assert!(html.contains("data:image/png;base64,QUI="), "{html}");
    assert!(html.starts_with("<img "), "{html}");
}

#[test]
fn logo_html_top_right_position() {
    let html = logo_html(&logo(LogoPosition::TopRight, None), b"A", "image/png");
    assert!(html.contains("top: 20px;"), "{html}");
    assert!(html.contains("right: 20px;"), "{html}");
    assert!(!html.contains("bottom:"), "{html}");
    assert!(!html.contains("left:"), "{html}");
}

#[test]
fn logo_html_top_left_position() {
    let html = logo_html(&logo(LogoPosition::TopLeft, None), b"A", "image/png");
    assert!(html.contains("top: 20px;"), "{html}");
    assert!(html.contains("left: 20px;"), "{html}");
    assert!(!html.contains("bottom:"), "{html}");
    assert!(!html.contains("right:"), "{html}");
}

#[test]
fn logo_html_bottom_right_position() {
    let html = logo_html(&logo(LogoPosition::BottomRight, None), b"A", "image/png");
    assert!(html.contains("bottom: 20px;"), "{html}");
    assert!(html.contains("right: 20px;"), "{html}");
    assert!(!html.contains("top:"), "{html}");
    assert!(!html.contains("left:"), "{html}");
}

#[test]
fn logo_html_bottom_left_position() {
    let html = logo_html(&logo(LogoPosition::BottomLeft, None), b"A", "image/png");
    assert!(html.contains("bottom: 20px;"), "{html}");
    assert!(html.contains("left: 20px;"), "{html}");
    assert!(!html.contains("top:"), "{html}");
    assert!(!html.contains("right:"), "{html}");
}

#[test]
fn logo_html_includes_width_when_set() {
    let html = logo_html(
        &logo(LogoPosition::TopRight, Some("120px")),
        b"A",
        "image/png",
    );
    assert!(html.contains("width: 120px;"), "{html}");
}

#[test]
fn logo_html_width_value_cannot_break_out_of_style_attribute() {
    let html = logo_html(
        &logo(LogoPosition::TopRight, Some("120px\" onerror=\"evil()")),
        b"A",
        "image/png",
    );
    // A width value with an embedded `"` must not be able to close the
    // `style="..."` attribute early and open a new one (`onerror="..."`) —
    // the quote count stays at exactly the 4 the template itself owns.
    assert_eq!(html.matches('"').count(), 4, "{html}");
    assert!(html.trim_end().ends_with('>'), "{html}");
}

#[test]
fn logo_html_width_value_is_escaped() {
    let html = logo_html(
        &logo(
            LogoPosition::TopRight,
            Some("120px; } body { display: none"),
        ),
        b"A",
        "image/png",
    );
    // A malicious width value must not be able to close the `style`
    // attribute's declaration list early (via `;`/`}`) or the attribute
    // itself (via `"`/`<`/`>`) — the whole `<img>` tag stays a single,
    // well-formed element.
    // Exactly 4 quotes: `src="..."` and `style="..."` — a value that
    // introduced its own `"` would change this count.
    assert_eq!(html.matches('"').count(), 4, "{html}");
    assert!(html.trim_end().ends_with('>'), "{html}");
    assert!(
        html.contains("pointer-events: none;\">"),
        "style attribute should still close normally after the injected value: {html}"
    );
}

#[test]
fn base64_encode_matches_known_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}
