//! Split out of `cli.rs` to keep both files under the size cap (same
//! convention as `backends/*_tests.rs`).

use std::sync::OnceLock;

use tokio::sync::Mutex;

use super::*;
use crate::backends::typst::test_support::count_processes_named;

/// Every test below spawns a real `typst` subprocess, and
/// `timeout_kills_and_reaps_a_non_terminating_compile` asserts on the
/// system-wide `typst` process count before/after its own compile. Cargo
/// runs a test binary's `#[tokio::test]`s concurrently on multiple threads
/// by default, so without this lock a sibling test's still-running compile
/// gets counted as a leak that isn't one. Scoped to this file only: no other
/// test in the `mdcast` lib spawns a real `typst` binary (the rest use
/// `TypstCompiler::Library`, in-process).
fn typst_process_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

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

#[cfg(target_os = "linux")]
#[test]
fn outline_child_is_the_running_image_not_its_path() {
    let cmd = OutlineCommand::current_exe().unwrap();
    assert_eq!(cmd.program, Path::new("/proc/self/exe"));
    assert_eq!(cmd.args, [crate::backends::typst::OUTLINE_CHILD_ARG]);
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
    let _guard = typst_process_lock().lock().await;
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
    let _guard = typst_process_lock().lock().await;
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
    let _guard = typst_process_lock().lock().await;
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
    let _guard = typst_process_lock().lock().await;
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

#[tokio::test]
async fn compile_errors_are_typed_diagnostics_without_the_temp_dir() {
    let _guard = typst_process_lock().lock().await;
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_secs(20));

    let err = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", "= Hi\n#image(\"missing.png\")\n")],
            CompileFormat::Pdf,
        ))
        .await
        .unwrap_err();

    let diags = err
        .downcast_ref::<crate::backends::typst::CompileDiagnostics>()
        .unwrap_or_else(|| panic!("not typed diagnostics: {err:#}"));
    let all = diags.diagnostics.join("\n");
    assert!(all.starts_with("main.typ:2:"), "{all}");
    assert!(all.contains("missing.png"), "{all}");
    assert!(!all.contains("mdcast-typst-"), "temp dir leaked: {all}");
}

/// A `@preview` import must fail, promptly, without a download: the proxy
/// variables point every request at a closed loopback port.
#[tokio::test]
async fn a_package_import_fails_fast_without_downloading() {
    let _guard = typst_process_lock().lock().await;
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_secs(20));
    let start = std::time::Instant::now();

    let err = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", "#import \"@preview/cetz:0.3.0\": *\nHi\n")],
            CompileFormat::Pdf,
        ))
        .await
        .unwrap_err();

    assert!(start.elapsed() < Duration::from_secs(10), "{err:#}");
    let message = format!("{err:#}");
    assert!(message.contains("cetz"), "{message}");
    assert!(
        message.contains("refused"),
        "not refused locally: {message}"
    );
}

#[tokio::test]
async fn output_over_the_cap_is_rejected_before_it_is_read() {
    let _guard = typst_process_lock().lock().await;
    let Some(bin) = typst_on_path() else {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    };
    let cli = CliCompiler::new(bin, Duration::from_secs(20)).with_max_output_bytes(64);

    let err = cli
        .compile(inputs(
            "main.typ",
            vec![("main.typ", "= Hello")],
            CompileFormat::Pdf,
        ))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("64-byte cap"), "{err:#}");
}

#[test]
fn bounds_cap_file_size_and_cpu_time() {
    let cli = CliCompiler::new("typst", Duration::from_secs(10)).with_max_output_bytes(1000);
    let bounds = cli.bounds();
    assert_eq!(bounds.max_file_bytes, Some(1000 + (1 << 20)));
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
    assert_eq!(bounds.cpu_secs, Some(11 * cores + 5));
}
