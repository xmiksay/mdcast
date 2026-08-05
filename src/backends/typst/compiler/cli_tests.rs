//! Split out of `cli.rs` to keep both files under the size cap (same
//! convention as `backends/*_tests.rs`).

use super::*;

fn typst_on_path() -> Option<String> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|dir| Path::new(dir).join("typst"))
        .find(|p| p.is_file())
        .map(|_| "typst".to_string())
}

fn inputs(main: &str, sources: Vec<(&str, &str)>, format: CompileFormat) -> CompileInputs {
    CompileInputs {
        main: main.to_string(),
        sources: sources
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect(),
        files: Vec::new(),
        fonts: Vec::new(),
        format,
    }
}

#[test]
fn safe_join_rejects_parent_dir_escape() {
    let root = Path::new("/tmp/mdcast-root");
    assert!(safe_join(root, "../../etc/passwd").is_err());
    assert!(safe_join(root, "images/../../escape.png").is_err());
}

#[test]
fn safe_join_rejects_absolute_paths() {
    let root = Path::new("/tmp/mdcast-root");
    assert!(safe_join(root, "/etc/passwd").is_err());
}

#[test]
fn safe_join_allows_nested_relative_paths() {
    let root = Path::new("/tmp/mdcast-root");
    let joined = safe_join(root, "templates/partials/header.typ").unwrap();
    assert_eq!(
        joined,
        Path::new("/tmp/mdcast-root/templates/partials/header.typ")
    );
}

#[tokio::test]
async fn real_binary_compiles_pdf() {
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_secs(20));

    let bytes = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", "= Hello")],
            CompileFormat::Pdf,
        ))
        .await
        .unwrap();

    assert!(bytes.starts_with(b"%PDF-"), "not a PDF: {bytes:?}");
}

#[tokio::test]
async fn real_binary_compiles_html_without_the_typst_html_cargo_feature() {
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_secs(20));

    let bytes = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", "= Hello")],
            CompileFormat::Html,
        ))
        .await
        .unwrap();

    let html = String::from_utf8(bytes).unwrap();
    assert!(html.contains("<html"), "not html: {html}");
}

#[tokio::test]
async fn a_compile_error_is_reported_with_the_typst_diagnostic() {
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_secs(20));

    let err = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", "#nonexistent-fn()")],
            CompileFormat::Pdf,
        ))
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("unknown variable"),
        "expected the typst diagnostic in the error: {err}"
    );
}

#[tokio::test]
async fn timeout_kills_and_reaps_a_non_terminating_compile() {
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_millis(300));
    const SLOW: &str = r#"
#let x = {
  let i = 0
  let n = int(calc.pow(2, 62))
  for _ in range(n) { i = i + 1 }
  i
}
#x
"#;

    let before = count_processes_named("typst");
    let start = std::time::Instant::now();

    let err = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", SLOW)],
            CompileFormat::Pdf,
        ))
        .await
        .unwrap_err();

    assert!(
        start.elapsed() < Duration::from_secs(10),
        "compile did not return promptly after the timeout elapsed"
    );
    assert!(err.to_string().contains("timed out"), "{err}");

    // The killed child must be reaped, not left a zombie or orphan — this is
    // the behaviour `Library` mode structurally cannot offer (spawn_blocking
    // cannot be cancelled).
    let mut after = count_processes_named("typst");
    for _ in 0..20 {
        if after <= before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        after = count_processes_named("typst");
    }
    assert!(
        after <= before,
        "typst process count did not return to baseline: before={before} after={after}"
    );
}

/// Linux-only headcount via `/proc` — good enough to catch a leaked child in
/// CI without pulling in a process-listing crate. Returns 0 (never a false
/// failure) on any other target.
fn count_processes_named(name: &str) -> usize {
    #[cfg(target_os = "linux")]
    {
        let mut count = 0;
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return 0;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            if comm.trim() == name {
                count += 1;
            }
        }
        count
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        0
    }
}
