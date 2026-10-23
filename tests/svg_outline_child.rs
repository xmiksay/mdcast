//! The SVG-outline step (issue #92) run as the real binaries' hidden child
//! mode, the way `--typst-compiler cli` / `MDCAST_TYPST=cli` run it: same
//! result as in-process, the parent still logs the drops, and a pathological
//! SVG is stopped by the child's bounds instead of growing the parent.
#![cfg(all(feature = "typst", feature = "rt-multi-thread"))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use mdcast::backends::typst::{
    CliCompiler, OUTLINE_CHILD_ARG, OutlineCommand, TemplateDoc, TypstCompiler, outline_svg_files,
    render_template_with,
};
use mdcast::{AssetProvider, BoxFuture, BrandHandle, BrandSpec, DocMeta};

mod common;
use common::LogBuf;

const TEXT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="40"><text x="5" y="25" font-family="arial">HTTP ↔ doména</text></svg>"##;
const NO_TEXT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="currentColor"/></svg>"#;
const BROKEN: &str = "<svg><g></svg>";
const LINKING: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><text y="5">a</text><image width="1" height="1" href="dot.png"/></svg>"#;
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn files() -> Vec<(String, Vec<u8>)> {
    [
        ("t.svg", TEXT.as_bytes()),
        ("plain.svg", NO_TEXT.as_bytes()),
        ("broken.svg", BROKEN.as_bytes()),
        ("linking.svg", LINKING.as_bytes()),
        ("dot.png", PNG_1X1),
        ("data.json", b"{}"),
    ]
    .into_iter()
    .map(|(p, b)| (p.to_string(), b.to_vec()))
    .collect()
}

fn child_compiler(program: &str, timeout: Duration, max_memory: Option<u64>) -> TypstCompiler {
    TypstCompiler::Cli(
        CliCompiler::new("typst", timeout)
            .with_max_memory_bytes(max_memory)
            .with_outline(Some(OutlineCommand {
                program: program.into(),
                args: vec![OUTLINE_CHILD_ARG.to_string()],
            })),
    )
}

async fn assert_child_matches_in_process(program: &str) {
    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let compiler = child_compiler(program, Duration::from_secs(60), None);
    let (child, child_dropped) = outline_svg_files(&compiler, files(), Vec::new())
        .await
        .unwrap_or_else(|e| panic!("outline child failed: {e:#}"));
    let (local, local_dropped) = outline_svg_files(&TypstCompiler::Library, files(), Vec::new())
        .await
        .unwrap();

    assert_eq!(child, local, "child and in-process outlining differ");
    assert_eq!(child_dropped, local_dropped);
    assert_eq!(
        child_dropped.into_iter().collect::<Vec<_>>(),
        ["broken.svg"]
    );
    let get = |path: &str| &child.iter().find(|(p, _)| p == path).expect(path).1;
    assert!(!String::from_utf8_lossy(get("t.svg")).contains("<text"));
    assert_eq!(
        get("plain.svg"),
        NO_TEXT.as_bytes(),
        "text-free SVG kept byte-identical"
    );
    assert!(String::from_utf8_lossy(get("linking.svg")).contains("data:image/png;base64,"));
    assert_eq!(get("data.json"), b"{}");
    // Logged by the parent, with the same message as in-process.
    let logs = buf.contents();
    assert!(
        logs.contains("SVG could not be parsed; dropping it") && logs.contains("broken.svg"),
        "{logs}"
    );
}

#[tokio::test]
async fn mdcast_hidden_mode_outlines_like_in_process() {
    assert_child_matches_in_process(env!("CARGO_BIN_EXE_mdcast")).await;
}

#[cfg(feature = "server")]
#[tokio::test]
async fn mdcast_server_hidden_mode_outlines_like_in_process() {
    assert_child_matches_in_process(env!("CARGO_BIN_EXE_mdcast-server")).await;
}

/// 5 nested levels × 10 `<use>` of a 500-char label: ~50M glyph outlines
/// from ~2 KB of SVG. The byte caps cannot see this coming; only the child's
/// memory cap / timeout stop it.
fn use_fanout_bomb() -> String {
    let mut defs = format!(r#"<text id="l0" y="10">{}</text>"#, "W".repeat(500));
    for level in 1..=5 {
        let uses: String = (0..10)
            .map(|_| format!(r##"<use href="#l{}"/>"##, level - 1))
            .collect();
        defs.push_str(&format!(r#"<g id="l{level}">{uses}</g>"#));
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><defs>{defs}</defs><use href="#l5"/></svg>"##
    )
}

/// The address-space cap, not the timeout, is what stops it: the timeout is
/// set far beyond how long the cap takes to bite. A benign SVG under the same
/// cap first proves the child can run at all, so a cap too small to even
/// start in cannot pass for a working one.
#[cfg(unix)]
#[tokio::test]
async fn use_fanout_bomb_is_stopped_by_the_child_memory_cap() {
    const CAP: u64 = 256 << 20;
    let compiler = child_compiler(
        env!("CARGO_BIN_EXE_mdcast"),
        Duration::from_secs(120),
        Some(CAP),
    );
    let control = vec![("t.svg".to_string(), TEXT.as_bytes().to_vec())];
    if let Err(e) = outline_svg_files(&compiler, control, Vec::new()).await {
        eprintln!("skipping: the outline child cannot run under a {CAP}-byte cap here: {e:#}");
        return;
    }

    let err = outline_svg_files(
        &compiler,
        vec![("bomb.svg".into(), use_fanout_bomb().into_bytes())],
        Vec::new(),
    )
    .await
    .expect_err("the bomb must fail the render, not be outlined");

    assert!(err.to_string().contains("SVG outline failed"), "{err:#}");
}

/// Real prefix-scanning `list`, so sibling discovery finds the SVGs.
struct MapAssets(BTreeMap<&'static str, &'static [u8]>);

impl AssetProvider for MapAssets {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Bytes>>> {
        let v = self.0.get(key).map(|b| Bytes::from_static(b));
        Box::pin(async move { Ok(v) })
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<String>>> {
        let out = self
            .0
            .keys()
            .filter(|k| k.starts_with(prefix))
            .map(|k| k.to_string())
            .collect();
        Box::pin(async move { Ok(out) })
    }
}

fn typst_available() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whole `cli`-mode template render: the typst child sees the outlined text
/// SVG and the untouched text-free one — asserted by the template itself.
#[tokio::test]
async fn cli_template_render_sees_the_child_outlined_files() {
    if !typst_available() {
        eprintln!("skipping: no `typst` binary on PATH");
        return;
    }
    const MAIN: &str = r#"#assert(not read("t.svg").contains("<text"))
#assert(read("plain.svg").contains("currentColor"))
#image("t.svg", width: 4cm)
#image("plain.svg", width: 1cm)
"#;
    let assets = MapAssets(BTreeMap::from([
        ("tpl/main.typ", MAIN.as_bytes()),
        ("tpl/t.svg", TEXT.as_bytes()),
        ("tpl/plain.svg", NO_TEXT.as_bytes()),
    ]));
    let doc = TemplateDoc {
        template: "tpl/main.typ".into(),
        data: serde_json::json!({}),
        meta: DocMeta::default(),
        brand: BrandHandle(Arc::new(BrandSpec::default())),
        fonts: Vec::new(),
    };
    let compiler = child_compiler(env!("CARGO_BIN_EXE_mdcast"), Duration::from_secs(60), None);

    let artifact = render_template_with(&doc, &assets, &compiler)
        .await
        .unwrap_or_else(|e| panic!("render failed: {e:#}"));

    assert!(artifact.primary.starts_with(b"%PDF-"));
}
