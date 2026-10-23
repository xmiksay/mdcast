//! The SVG-outline step as a bounded child process (`cli` mode). The parent
//! frames what the child needs onto its stdin (`ipc.rs`), the child — one of
//! mdcast's own binaries in its hidden
//! [`OUTLINE_CHILD_ARG`](crate::backends::typst::OUTLINE_CHILD_ARG) mode —
//! runs the very same `classify` the in-process path does and frames one
//! outcome per file it was sent back; every file it was not sent is `Keep`.
//!
//! The child runs under the compile's own `RLIMIT_AS`, so it is sent as
//! little as `classify` can still answer from (`select`): the SVG
//! candidates, the rasters a text-bearing one may link via `<image href>`
//! (the only thing a sibling is resolved for), and the fonts only when some
//! candidate may carry text — never `.typ` sources, JSON or unlinked photos.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::super::compiler::OutlineCommand;
use super::super::process::{Bounds, run_bounded};
use super::resolve::{is_gzip, linked_kind};
use super::{LIMITS, Outcome, Outlined, apply, classify, ipc, may_contain_text, svg_kind};

pub(super) async fn outline_in_child(
    cmd: &OutlineCommand,
    bounds: Bounds,
    files: Vec<(String, Vec<u8>)>,
    fonts: Vec<Vec<u8>>,
) -> Result<Outlined> {
    let selection = select(&files);
    let sent: Vec<(&str, &[u8])> = files
        .iter()
        .zip(&selection.files)
        .filter(|(_, send)| **send)
        .map(|((p, b), _)| (p.as_str(), b.as_slice()))
        .collect();
    let sent_fonts: Vec<&[u8]> = if selection.fonts {
        fonts.iter().map(Vec::as_slice).collect()
    } else {
        Vec::new()
    };
    let n_sent = sent.len();
    let request = ipc::encode_request(&sent, &sent_fonts);

    let mut command = Command::new(&cmd.program);
    command.args(&cmd.args);
    let cap = ipc::response_cap(n_sent, LIMITS.output);
    let done = run_bounded(command, Some(request), bounds, cap, "SVG outline").await?;
    if !done.status.success() {
        bail!(
            "SVG outline failed ({}):\n{}",
            done.status,
            String::from_utf8_lossy(&done.stderr)
        );
    }
    if done.stdout_overflow {
        bail!("SVG outline child response exceeds the {cap}-byte cap");
    }
    let answered = ipc::decode_response(&done.stdout, n_sent, LIMITS.output)
        .context("malformed SVG outline child response")?;
    let (files, dropped) = apply(files, spread(&selection.files, answered));
    Ok(Outlined {
        files,
        fonts,
        dropped,
    })
}

/// What the child is sent: a flag per file, plus whether the fonts go too.
#[derive(Debug, PartialEq)]
struct Selection {
    files: Vec<bool>,
    fonts: bool,
}

/// Only an SVG that may carry text is outlined, and only outlining resolves
/// linked images or needs fonts — a text-free one is validated with images
/// disabled. A gzipped candidate counts as text-bearing without inflating
/// it here: the parent stays cheap and the child decides.
fn select(files: &[(String, Vec<u8>)]) -> Selection {
    let candidate: Vec<bool> = files
        .iter()
        .map(|(p, b)| svg_kind(p, b).is_some())
        .collect();
    let texty: Vec<&[u8]> = files
        .iter()
        .zip(&candidate)
        .filter(|(_, c)| **c)
        .map(|((_, b), _)| b.as_slice())
        .filter(|b| is_gzip(b) || may_contain_text(b))
        .collect();
    let files = files
        .iter()
        .zip(&candidate)
        .map(|((path, bytes), &c)| {
            c || (linked_kind(path, bytes).is_some() && texty.iter().any(|svg| may_link(svg, path)))
        })
        .collect();
    Selection {
        files,
        fonts: !texty.is_empty(),
    }
}

/// Conservative "could `svg` name `path` in an `<image href>`": a resolved
/// href always ends in the file's own name, so its absence rules the link
/// out — unless the SVG is gzipped, or could spell the name through a
/// character/entity reference, or the name itself needs XML escaping.
fn may_link(svg: &[u8], path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).as_bytes();
    let has = |needle: &[u8]| svg.windows(needle.len()).any(|w| w == needle);
    name.is_empty()
        || name.iter().any(|c| b"&<>\"'".contains(c))
        || is_gzip(svg)
        || has(b"&#")
        || has(b"<!ENTITY")
        || has(name)
}

/// The child's outcomes (one per sent file, in order) widened back to one
/// per file; `decode_response` already checked there is exactly one per
/// sent file.
fn spread(sent: &[bool], answered: Vec<Outcome>) -> Vec<Outcome> {
    let mut answered = answered.into_iter();
    sent.iter()
        .map(|&s| {
            if s {
                answered.next().unwrap_or(Outcome::Keep)
            } else {
                Outcome::Keep
            }
        })
        .collect()
}

/// The child half: read one framed request from `input`, outline it
/// in-process, write the framed outcomes to `output`. Nothing may log to
/// `output` — it is the response channel — so the outcomes carry their
/// reasons and the parent does the logging.
pub fn run_child(mut input: impl Read, mut output: impl Write) -> Result<()> {
    let mut buf = Vec::new();
    input
        .read_to_end(&mut buf)
        .context("read SVG outline request")?;
    let request = ipc::decode_request(&buf).context("decode SVG outline request")?;
    let outcomes = classify(&request.files, &request.fonts, LIMITS);
    output
        .write_all(&ipc::encode_response(&outcomes))
        .context("write SVG outline response")?;
    output.flush().context("flush SVG outline response")
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;
