use bytes::Bytes;
use mdcast_api::wire::{BoardOptions, BoardTheme, GeneratedSpec};

use super::chess::{board_from_attrs, fen_svg, pgn_svg, render_chess};
use super::*;
use crate::sync_provider;

const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
const RUY: &str = "1. e4 e5 2. Nf3 Nc6 3. Bb5 a6";
/// `RUY` after four half-moves (2... Nc6).
const AFTER_4: &str = "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R";
/// `RUY` at its end (3... a6).
const FINAL: &str = "r1bqkbnr/1ppp1ppp/p1n5/1B2p3/4P3/5N2/PPPP1PPP/RNBQK2R";

fn provider() -> impl AssetProvider {
    sync_provider(|key| match key {
        "start.fen" => Ok(Some(Bytes::from(format!("{START}\n")))),
        "ruy.pgn" => Ok(Some(Bytes::from_static(RUY.as_bytes()))),
        _ => Ok(None),
    })
}

async fn chess(markdown: &str) -> Generated {
    render_chess(markdown, &provider()).await
}

fn text(g: &Generated, i: usize) -> &str {
    std::str::from_utf8(&g.svgs[i].1).unwrap()
}

fn plain(fen: &str) -> String {
    fen_svg(fen, &BoardOptions::default(), Size::Normal).unwrap()
}

fn attrs(pairs: &[(&str, &str)]) -> Attrs {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn fen_fence_becomes_an_image_ref() {
    let r = chess(&format!("# Start\n\n```fen\n{START}\n```\n\nGo.\n")).await;
    assert_eq!(r.markdown, "# Start\n\n![](chess/fen-0.svg)\n\nGo.\n");
    assert_eq!(r.svgs[0].0, "chess/fen-0.svg");
    assert_eq!(text(&r, 0), plain(START));
}

#[tokio::test]
async fn fen_and_pgn_tags_and_paths_are_keyed_per_name() {
    let md = format!(
        "<fen>{START}</fen>\n\n<pgn move=\"4\">{RUY}</pgn>\n\n<fen path=\"start.fen\">\n\n<pgn path=\"ruy.pgn\">\n"
    );
    let r = chess(&md).await;
    assert_eq!(
        r.markdown,
        "![](chess/fen-0.svg)\n\n![](chess/pgn-0.svg)\n\n![](chess/fen-1.svg)\n\n![](chess/pgn-1.svg)\n"
    );
    let keys: Vec<_> = r.svgs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "chess/fen-0.svg",
            "chess/fen-1.svg",
            "chess/pgn-0.svg",
            "chess/pgn-1.svg"
        ]
    );
    assert_eq!(text(&r, 1), plain(START), "path content is trimmed");
    assert_eq!(text(&r, 2), plain(AFTER_4));
    assert_eq!(
        text(&r, 3),
        plain(FINAL),
        "no move= means the final position"
    );
}

#[test]
fn pgn_at_move_n_versus_final_position() {
    let opts = BoardOptions::default();
    let at = |ply| pgn_svg(RUY, ply, &opts, Size::Normal).unwrap();
    assert_eq!(at(Some(0)), plain(START));
    assert_eq!(at(Some(4)), plain(AFTER_4));
    assert_eq!(at(Some(6)), plain(FINAL));
    assert_eq!(at(None), plain(FINAL));
    assert!(pgn_svg(RUY, Some(7), &opts, Size::Normal).is_err());
}

#[test]
fn pgn_without_moves_is_the_start_position_and_empty_is_an_error() {
    let opts = BoardOptions::default();
    let headers = "[Event \"?\"]\n\n*";
    assert_eq!(
        pgn_svg(headers, None, &opts, Size::Normal).unwrap(),
        plain(START)
    );
    assert!(pgn_svg("  \n", None, &opts, Size::Normal).is_err());
}

#[test]
fn flip_theme_and_coordinates_change_the_output() {
    let base = plain(START);
    let with = |pairs: &[(&str, &str)]| {
        fen_svg(
            START,
            &board_from_attrs(&attrs(pairs)).unwrap(),
            Size::Normal,
        )
        .unwrap()
    };
    let flipped = with(&[("flip", "")]);
    assert_ne!(flipped, base);
    assert_eq!(with(&[("orientation", "black")]), flipped);
    assert_eq!(
        with(&[("orientation", "black"), ("flip", "true")]),
        base,
        "flip turns the board relative to orientation"
    );
    assert_ne!(with(&[("coordinates", "false")]), base);
    assert_eq!(with(&[("theme", "brown")]), base);
    let green = with(&[("theme", "Green")]);
    assert!(green.contains("#86a666"), "green dark squares");
    assert!(!base.contains("#86a666"));
    assert!(with(&[("theme", "blue")]).contains("#8ca2ad"));
}

#[test]
fn board_attrs_map_to_wire_options() {
    let o = board_from_attrs(&attrs(&[("theme", "blue"), ("coordinates", "false")])).unwrap();
    assert_eq!(
        o,
        BoardOptions {
            flip: false,
            theme: BoardTheme::Blue,
            coordinates: false
        }
    );
    for bad in [
        ("theme", "pink"),
        ("orientation", "left"),
        ("flip", "yes"),
        ("coordinates", "1"),
    ] {
        assert!(board_from_attrs(&attrs(&[bad])).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn bare_flip_attribute_works_on_fences_and_tags() {
    let fence = chess(&format!("```fen flip theme=green\n{START}\n```\n")).await;
    let tag = chess(&format!("<fen flip theme=\"green\">{START}</fen>\n")).await;
    assert_eq!(fence.markdown, "![](chess/fen-0.svg)\n");
    assert_eq!(tag.markdown, "![](chess/fen-0.svg)\n");
    assert_eq!(fence.svgs[0].1, tag.svgs[0].1);
    assert_ne!(text(&tag, 0), plain(START));
}

#[test]
fn intrinsic_size_is_physical_and_keeps_the_viewbox() {
    let opts = BoardOptions::default();
    let normal = fen_svg(START, &opts, Size::Normal).unwrap();
    assert!(normal.starts_with("<svg"));
    assert!(normal.contains(r#"width="8cm" height="8cm""#), "{normal}");
    assert!(normal.contains(r#"viewBox="0 0 360 360""#));
    assert!(!normal.contains(r#"width="360""#));
    let small = fen_svg(START, &opts, Size::Small).unwrap();
    assert!(small.contains(r#"width="4cm" height="4cm""#));
    let large = fen_svg(START, &opts, Size::Large).unwrap();
    assert!(large.contains(r#"width="12cm" height="12cm""#));
}

#[tokio::test]
async fn size_attribute_scales_the_board() {
    let r = chess(&format!("<fen size=\"small\">{START}</fen>\n")).await;
    assert!(text(&r, 0).contains(r#"width="4cm""#));
}

#[tokio::test]
async fn invalid_blocks_stay_as_written_without_shifting_keys() {
    let md = format!(
        "```fen\nnot a fen\n```\n\n<fen theme=\"pink\">{START}</fen>\n\n<fen>{START}</fen>\n\n\
         <pgn move=\"two\">{RUY}</pgn>\n\n<pgn move=\"99\">{RUY}</pgn>\n\n<pgn>1. e4 Ke2??</pgn>\n\n\
         <pgn path=\"missing.pgn\">\n\n<pgn>{RUY}</pgn>\n"
    );
    let r = chess(&md).await;
    assert!(r.markdown.starts_with("```fen\nnot a fen\n```\n"));
    assert!(r.markdown.contains("<fen theme=\"pink\">"));
    assert!(r.markdown.contains("![](chess/fen-2.svg)"));
    for kept in [
        "<pgn move=\"two\">",
        "<pgn move=\"99\">",
        "<pgn>1. e4 Ke2??</pgn>",
        "<pgn path=\"missing.pgn\">",
    ] {
        assert!(r.markdown.contains(kept), "{kept} kept: {}", r.markdown);
    }
    assert!(r.markdown.ends_with("![](chess/pgn-4.svg)\n"));
    let keys: Vec<_> = r.svgs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["chess/fen-2.svg", "chess/pgn-4.svg"]);
}

#[tokio::test]
async fn chess_tags_inside_code_are_left_alone() {
    let md = format!("`<fen>{START}</fen>`\n\n```text\n<pgn>{RUY}</pgn>\n```\n");
    let r = chess(&md).await;
    assert_eq!(r.markdown, md);
    assert!(r.svgs.is_empty());
}

#[test]
fn render_spec_renders_fen_and_pgn_entries() {
    let fen = GeneratedSpec::Fen {
        fen: START.into(),
        board: BoardOptions::default(),
    };
    assert_eq!(&render_spec(&fen).unwrap()[..], plain(START).as_bytes());
    let pgn = GeneratedSpec::Pgn {
        pgn: RUY.into(),
        ply: Some(4),
        board: BoardOptions::default(),
    };
    assert_eq!(&render_spec(&pgn).unwrap()[..], plain(AFTER_4).as_bytes());
    let bad = GeneratedSpec::Fen {
        fen: "nope".into(),
        board: BoardOptions::default(),
    };
    assert!(render_spec(&bad).is_err());
}

#[test]
fn chess_generators_are_advertised() {
    let gens = available();
    assert!(gens.contains(&Generator::Fen) && gens.contains(&Generator::Pgn));
    assert!(!gens.contains(&Generator::Unknown));
}
