//! What pandoc may read while rendering: nothing but the files this render
//! materialised itself.
//!
//! Pandoc's writers fetch every resource the document names — an image for
//! docx/odt/pptx, and under html-reveal's `--embed-resources` also
//! `data-background-image` and friends — from wherever the path points, the
//! host filesystem included. `images::resolve_images` already removes every
//! image the provider could not resolve, but it sees the markdown through
//! pulldown-cmark, and pandoc's reader is not pulldown-cmark (a reference
//! definition on another page, header attributes, …). So the same rule is
//! enforced a second time on pandoc's *own* AST, by a Lua filter that runs
//! before any writer: a resource is either an inline `data:` URI or a file
//! directly inside this render's assets directory, or it is gone.
//!
//! The reader itself is narrowed too ([`READER`]): no raw HTML (`<img
//! src="/etc/…">` would be embedded verbatim into html-reveal), no raw
//! attribute blocks (```` ```{=html} ````, likewise), no raw TeX, and no
//! in-document YAML metadata (`css:`, `header-includes:` and
//! `title-slide-attributes:` all name files the template then embeds).
//! mdcast's own docx/odt page breaks used to be raw-attribute blocks; they are
//! now an empty `.mdcast-page-break` div this filter turns into the writer's
//! raw markup — raw output the filter creates is not subject to the reader's
//! extensions.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `--from` for every pandoc target.
pub(super) const READER: &str = "markdown-raw_html-raw_attribute-raw_tex-yaml_metadata_block";

/// The class `pandoc_input::build_input` marks a docx/odt page break with.
pub(super) const PAGE_BREAK_CLASS: &str = "mdcast-page-break";

const FILTER: &str = r#"
local function allowed(src)
  if src:sub(1, 5):lower() == "data:" then
    return true
  end
  if src:sub(1, #ASSETS) ~= ASSETS then
    return false
  end
  local name = src:sub(#ASSETS + 1)
  return name ~= "" and name ~= "." and name ~= ".."
    and not name:find("/", 1, true) and not name:find("\\", 1, true)
end

-- Attributes pandoc's writers resolve as resources (reveal.js turns header
-- `background-*` attributes into `data-background-*` on the slide).
local RESOURCE_KEYS = {
  ["src"] = true, ["data-src"] = true, ["poster"] = true,
  ["background"] = true, ["data-background"] = true,
  ["background-image"] = true, ["data-background-image"] = true,
  ["background-video"] = true, ["data-background-video"] = true,
  ["background-iframe"] = true, ["data-background-iframe"] = true,
}

local function scrub(el)
  if not el.attributes then
    return nil
  end
  local drop = {}
  for key, value in pairs(el.attributes) do
    if RESOURCE_KEYS[key] and not allowed(value) then
      drop[#drop + 1] = key
    end
  end
  if #drop == 0 then
    return nil
  end
  for _, key in ipairs(drop) do
    el.attributes[key] = nil
  end
  return el
end

local no_raw = {
  RawBlock = function() return {} end,
  RawInline = function() return {} end,
}

local resources = {
  Image = function(img)
    if not allowed(img.src) then
      return {}
    end
    return scrub(img)
  end,
  Inline = scrub,
  Block = scrub,
}

local page_breaks = {
  Div = function(div)
    if not div.classes:includes(PAGE_BREAK) then
      return nil
    end
    if FORMAT == "docx" then
      return pandoc.RawBlock("openxml", '<w:p><w:r><w:br w:type="page"/></w:r></w:p>')
    elseif FORMAT == "odt" then
      return pandoc.RawBlock("opendocument", '<text:p text:style-name="PageBreak"/>')
    end
    return {}
  end,
}

return { no_raw, resources, page_breaks }
"#;

/// Writes the filter for one render to `<root>/mdcast.lua`, allowing exactly
/// the files directly inside `assets_dir`, and returns its path.
pub(super) async fn write_filter(root: &Path, assets_dir: &Path) -> Result<PathBuf> {
    let path = root.join("mdcast.lua");
    tokio::fs::write(&path, filter_source(assets_dir))
        .await
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

fn filter_source(assets_dir: &Path) -> String {
    let prefix = format!("{}/", assets_dir.display());
    format!(
        "local ASSETS = {}\nlocal PAGE_BREAK = {}\n{FILTER}",
        lua_string(&prefix),
        lua_string(PAGE_BREAK_CLASS)
    )
}

fn lua_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_strings_cannot_be_broken_out_of() {
        assert_eq!(lua_string(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(lua_string("x\ny"), r#""x\ny""#);
    }

    #[test]
    fn the_filter_allows_exactly_the_assets_dir() {
        let src = filter_source(Path::new("/tmp/render/assets"));
        assert!(
            src.starts_with("local ASSETS = \"/tmp/render/assets/\"\n"),
            "{src}"
        );
        assert!(
            src.contains("local PAGE_BREAK = \"mdcast-page-break\""),
            "{src}"
        );
    }
}
