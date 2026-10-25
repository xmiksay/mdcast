//! The child protocol in-process (`run_child` over byte buffers) and the
//! parent's failure handling against stand-in programs. The real binaries'
//! hidden mode is covered by `tests/svg_outline_child.rs`.

use std::time::{Duration, Instant};

use std::collections::HashSet;

use super::super::tests::{NO_TEXT, PNG_1X1};
use super::super::{Outcome, outline_svgs};
use super::*;
use crate::backends::typst::compiler::{CliCompiler, TypstCompiler};
use crate::backends::typst::test_support::count_child_processes_named;

const TEXT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><text y="8">Hi ↔</text></svg>"#;

fn files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("t.svg".into(), TEXT.as_bytes().to_vec()),
        ("plain.svg".into(), NO_TEXT.as_bytes().to_vec()),
        ("broken.svg".into(), b"<svg><g></svg>".to_vec()),
        ("x.png".into(), PNG_1X1.to_vec()),
    ]
}

fn bounds(timeout: Duration) -> Bounds {
    Bounds {
        timeout,
        max_memory_bytes: None,
        max_file_bytes: None,
        cpu_secs: None,
    }
}

fn command(program: &str, args: &[&str]) -> OutlineCommand {
    OutlineCommand::new(program, args.iter().copied())
}

fn refs(files: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
    files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect()
}

#[test]
fn run_child_answers_exactly_what_classify_does_in_process() {
    let files = files();
    let request = ipc::encode_request(&refs(&files), &[]);
    let mut response = Vec::new();
    run_child(request.as_slice(), &mut response).unwrap();

    let outcomes = ipc::decode_response(&response, 4, LIMITS.output).unwrap();
    assert_eq!(outcomes, classify(&refs(&files), &[], LIMITS));
    assert!(
        matches!(&outcomes[0], Outcome::Replace(svg) if !svg.windows(5).any(|w| w == b"<text"))
    );
    assert_eq!(outcomes[1], Outcome::Keep);
    assert!(matches!(outcomes[2], Outcome::Drop(_)));
    assert_eq!(outcomes[3], Outcome::Keep);
}

#[test]
fn run_child_rejects_a_garbled_request() {
    let mut response = Vec::new();
    assert!(run_child(&b"MDO1\x05"[..], &mut response).is_err());
    assert!(response.is_empty(), "nothing written for a bad request");
}

#[tokio::test]
async fn a_child_that_never_answers_is_killed_and_reaped() {
    let before = count_child_processes_named("sleep");
    let start = Instant::now();

    let err = outline_in_child(
        &command("sleep", &["30"]),
        bounds(Duration::from_secs(1)),
        files(),
        Vec::new(),
    )
    .await
    .err()
    .expect("a hung child must fail the render");

    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "did not return promptly"
    );
    let mut after = count_child_processes_named("sleep");
    for _ in 0..20 {
        if after <= before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        after = count_child_processes_named("sleep");
    }
    assert!(
        after <= before,
        "sleep child leaked: before={before} after={after}"
    );
}

#[tokio::test]
async fn a_garbage_answer_or_failed_exit_fails_the_render() {
    // `cat` echoes the request: well-formed bytes, wrong frame.
    let err = outline_in_child(
        &command("cat", &[]),
        bounds(Duration::from_secs(10)),
        files(),
        Vec::new(),
    )
    .await
    .err()
    .expect("echoed request is not a response");
    assert!(err.to_string().contains("malformed"), "{err:#}");

    let err = outline_in_child(
        &command("false", &[]),
        bounds(Duration::from_secs(10)),
        files(),
        Vec::new(),
    )
    .await
    .err()
    .expect("non-zero exit");
    assert!(err.to_string().contains("SVG outline failed"), "{err:#}");
}

#[tokio::test]
async fn no_svg_candidate_means_no_child_is_spawned() {
    let cli = CliCompiler::new("typst", Duration::from_secs(5))
        .with_outline(Some(command("/nonexistent/mdcast-outline", &[])));
    let compiler = TypstCompiler::Cli(cli);
    let png = vec![("x.png".to_string(), PNG_1X1.to_vec())];

    let outlined = outline_svgs(png.clone(), Vec::new(), &compiler)
        .await
        .unwrap();
    assert_eq!(outlined.files, png);

    let with_svg = vec![("a.svg".to_string(), NO_TEXT.as_bytes().to_vec())];
    let err = outline_svgs(with_svg, Vec::new(), &compiler).await.err();
    assert!(err.is_some(), "an SVG must reach the (missing) child");
}

#[tokio::test]
async fn cli_without_an_outline_command_still_outlines_in_process() {
    let compiler = TypstCompiler::Cli(CliCompiler::new("typst", Duration::from_secs(5)));
    let outlined = outline_svgs(files(), Vec::new(), &compiler).await.unwrap();
    assert_eq!(outlined.dropped, HashSet::from(["broken.svg".to_string()]));
    assert_eq!(outlined.files.len(), 3);
}

fn named(files: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    files
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_vec()))
        .collect()
}

#[test]
fn only_candidates_and_rasters_a_text_svg_may_link_are_sent() {
    let linking = r#"<svg xmlns="http://www.w3.org/2000/svg"><text>a</text><image href="img/dot.png"/></svg>"#;
    let sel = select(&named(&[
        ("main.typ", b"#image(\"a.svg\")"),
        ("a.svg", linking.as_bytes()),
        ("data.json", b"{}"),
        ("img/dot.png", PNG_1X1),
        ("photo.jpg", b"not even a jpeg"),
        ("magic-only", PNG_1X1),
        ("plain.svg", NO_TEXT.as_bytes()),
    ]));
    assert_eq!(
        sel.files,
        [false, true, false, true, false, false, true],
        "unlinked rasters and non-images stay in the parent"
    );
    assert!(sel.fonts, "a text-bearing SVG needs the fonts");
}

#[test]
fn text_free_svgs_need_no_fonts_and_no_rasters() {
    let sel = select(&named(&[
        ("plain.svg", NO_TEXT.as_bytes()),
        ("plain.png", PNG_1X1),
    ]));
    assert_eq!(sel.files, [true, false]);
    assert!(!sel.fonts);
}

#[test]
fn unreadable_names_send_every_linkable_raster() {
    let svg = |body: &str| {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><text>{body}</text></svg>"#)
    };
    let raster = ("x.png", PNG_1X1);
    for escaped in [svg("&#120;.png"), svg("<!ENTITY")] {
        let sel = select(&named(&[("a.svg", escaped.as_bytes()), raster]));
        assert_eq!(sel.files, [true, true], "{escaped}");
    }
    let gz = super::super::tests::gzip(svg("x.png").as_bytes());
    let sel = select(&named(&[("a.svgz", &gz), raster]));
    assert_eq!(sel.files, [true, true]);
    assert!(sel.fonts, "gzip is not inflated just to decide");
}

#[test]
fn a_name_with_a_space_is_always_sent() {
    // roxmltree reads `a<TAB>b.png` as `a b.png`, which the raw bytes never spell.
    let svg =
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>a</text><image href=\"a\tb.png\"/></svg>";
    let sel = select(&named(&[
        ("a.svg", svg.as_bytes()),
        ("img/a b.png", PNG_1X1),
    ]));
    assert_eq!(sel.files, [true, true]);
}

#[test]
fn spread_puts_each_answer_back_at_its_file() {
    let answered = vec![Outcome::Drop("x".into()), Outcome::Replace(b"s".to_vec())];
    assert_eq!(
        spread(&[false, true, false, true], answered),
        [
            Outcome::Keep,
            Outcome::Drop("x".into()),
            Outcome::Keep,
            Outcome::Replace(b"s".to_vec())
        ]
    );
}
