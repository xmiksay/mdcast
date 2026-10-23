//! Chess diagrams (`chess` feature): ```` ```fen ```` / ```` ```pgn ````
//! fences and `<fen>` / `<pgn>` tags in markdown, and `GeneratedSpec::Fen` /
//! `GeneratedSpec::Pgn` entries on the template path.
//!
//! A `fen` block draws the position it names; a `pgn` block replays the
//! game's mainline to half-move `move="N"` (`0` = the starting position) or,
//! without `move`, to its end. Board attributes, on both: `flip` (or
//! `orientation="black"`), `theme="brown|blue|green"`,
//! `coordinates="true|false"` and `size="small|large"`. The SVG itself comes
//! from `chess-diagram` (MIT; its `pgn` feature adds `cozy-chess`, also MIT).

use anyhow::{Context, Result, bail};
use chess_diagram::pgn::{PgnError, board_at};
use chess_diagram::{Board, Color, Options, Renderer, SvgRenderer, Theme};
use mdcast_api::wire::{BoardOptions, BoardTheme};

use super::{Attrs, Generated, Size, expand};
use crate::AssetProvider;

/// Intrinsic edge length at `Size::Normal` — a textbook diagram, readable
/// on paper without dominating the page.
const BASE_CM: f32 = 8.0;

/// Replace every `fen`/`pgn` fence and tag in `markdown` with an image
/// reference to its rendered board. Keys are `chess/fen-N.svg` and
/// `chess/pgn-N.svg`, each numbered separately in document order; the
/// returned `svgs` are grouped by generator (all `fen`, then all `pgn`),
/// document order within each.
pub async fn render_chess(markdown: &str, assets: &dyn AssetProvider) -> Generated {
    let fens = expand(markdown, "fen", "chess/fen", assets, |fen, attrs| {
        fen_svg(fen, &board_from_attrs(attrs)?, Size::from_attrs(attrs)?)
    })
    .await;
    let mut pgns = expand(&fens.markdown, "pgn", "chess/pgn", assets, |pgn, attrs| {
        pgn_svg(
            pgn,
            ply_from_attrs(attrs)?,
            &board_from_attrs(attrs)?,
            Size::from_attrs(attrs)?,
        )
    })
    .await;
    let mut svgs = fens.svgs;
    svgs.append(&mut pgns.svgs);
    Generated {
        markdown: pgns.markdown,
        svgs,
    }
}

/// Render the position `fen` names.
pub(crate) fn fen_svg(fen: &str, board: &BoardOptions, size: Size) -> Result<String> {
    let parsed = chess_diagram::parse(fen).map_err(|e| anyhow::anyhow!("invalid FEN: {e}"))?;
    render(&parsed, board, size)
}

/// Render `pgn`'s mainline after `ply` half-moves, or its final position.
pub(crate) fn pgn_svg(
    pgn: &str,
    ply: Option<usize>,
    board: &BoardOptions,
    size: Size,
) -> Result<String> {
    if pgn.trim().is_empty() {
        bail!("PGN has no content");
    }
    let position = match ply {
        Some(ply) => board_at(pgn, ply),
        // `board_at` only reports the mainline's length by refusing a ply
        // past its end, so ask for "everything" and replay to what it found.
        None => match board_at(pgn, usize::MAX) {
            Err(PgnError::PlyOutOfRange { available, .. }) => board_at(pgn, available),
            other => other,
        },
    }
    .map_err(|e| anyhow::anyhow!("invalid PGN: {e}"))?;
    render(&position, board, size)
}

fn render(position: &Board, board: &BoardOptions, size: Size) -> Result<String> {
    let opts = Options {
        orientation: if board.flip {
            Color::Black
        } else {
            Color::White
        },
        coordinates: board.coordinates,
        theme: match board.theme {
            BoardTheme::Brown => Theme::brown(),
            BoardTheme::Blue => Theme::blue(),
            BoardTheme::Green => Theme::green(),
        },
        ..Options::default()
    };
    let svg = SvgRenderer.render(position, &opts);
    // chess-diagram sizes the root in px; physical units make the DOCX/PDF
    // embed size deterministic instead of depending on the engine's DPI.
    let px = format!(r#"width="{0}" height="{0}""#, opts.size);
    let cm = size.scale(BASE_CM);
    let Some(at) = svg.find(&px) else {
        bail!("unexpected chess-diagram output: no {px} on the root element");
    };
    let mut out = svg;
    out.replace_range(
        at..at + px.len(),
        &format!(r#"width="{cm}cm" height="{cm}cm""#),
    );
    Ok(out)
}

pub(crate) fn board_from_attrs(attrs: &Attrs) -> Result<BoardOptions> {
    let black = match attrs
        .get("orientation")
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("white") => false,
        Some("black") => true,
        Some(other) => bail!("unknown orientation {other:?} (expected white or black)"),
    };
    let flip = bool_attr(attrs, "flip", false)?;
    let theme = match attrs
        .get("theme")
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("brown") => BoardTheme::Brown,
        Some("blue") => BoardTheme::Blue,
        Some("green") => BoardTheme::Green,
        Some(other) => bail!("unknown theme {other:?} (expected brown, blue or green)"),
    };
    Ok(BoardOptions {
        // `flip` turns the board relative to `orientation`.
        flip: black != flip,
        theme,
        coordinates: bool_attr(attrs, "coordinates", true)?,
    })
}

/// A bare attribute (`flip`) reads as an empty value, which means true.
fn bool_attr(attrs: &Attrs, name: &str, default: bool) -> Result<bool> {
    match attrs.get(name).map(|s| s.to_ascii_lowercase()).as_deref() {
        None => Ok(default),
        Some("" | "true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => bail!("{name}={other:?} is not true or false"),
    }
}

fn ply_from_attrs(attrs: &Attrs) -> Result<Option<usize>> {
    attrs
        .get("move")
        .map(|m| {
            m.trim()
                .parse()
                .with_context(|| format!("move={m:?} is not a half-move number"))
        })
        .transpose()
}
