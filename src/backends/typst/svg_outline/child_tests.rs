//! The child protocol in-process (`run_child` over byte buffers) and the
//! parent's failure handling against stand-in programs. The real binaries'
//! hidden mode is covered by `tests/svg_outline_child.rs`.

use std::time::{Duration, Instant};

use std::collections::HashSet;

use super::super::tests::{NO_TEXT, PNG_1X1};
use super::super::{Outcome, outline_svgs};
use super::*;
use crate::backends::typst::compiler::{CliCompiler, TypstCompiler};

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
    }
}

fn command(program: &str, args: &[&str]) -> OutlineCommand {
    OutlineCommand {
        program: program.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
    }
}

#[test]
fn run_child_answers_exactly_what_classify_does_in_process() {
    let request = ipc::encode_request(&files(), &[]);
    let mut response = Vec::new();
    run_child(request.as_slice(), &mut response).unwrap();

    let outcomes = ipc::decode_response(&response, 4, LIMITS.output).unwrap();
    assert_eq!(outcomes, classify(&files(), &[], LIMITS));
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
    let before = count_processes_named("sleep");
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
    let mut after = count_processes_named("sleep");
    for _ in 0..20 {
        if after <= before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        after = count_processes_named("sleep");
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

/// Linux-only headcount via `/proc`; 0 (never a false failure) elsewhere.
fn count_processes_named(name: &str) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok())
        .filter(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name))
        .count()
}
