//! The SVG-outline step as a bounded child process (`cli` mode). The parent
//! frames the render's files and fonts onto the child's stdin (`ipc.rs`), the
//! child — one of mdcast's own binaries in its hidden
//! [`OUTLINE_CHILD_ARG`](crate::backends::typst::OUTLINE_CHILD_ARG) mode —
//! runs the very same `classify` the in-process path does and frames one
//! outcome per file back. Every file goes over, not just the SVGs: a linked
//! `<image href>` resolves against its siblings.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::super::compiler::OutlineCommand;
use super::super::process::{Bounds, run_bounded};
use super::{LIMITS, Outlined, apply, classify, ipc};

pub(super) async fn outline_in_child(
    cmd: &OutlineCommand,
    bounds: Bounds,
    files: Vec<(String, Vec<u8>)>,
    fonts: Vec<Vec<u8>>,
) -> Result<Outlined> {
    let request = ipc::encode_request(&files, &fonts);
    let mut command = Command::new(&cmd.program);
    command.args(&cmd.args);
    let cap = ipc::response_cap(files.len(), LIMITS.output);

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
    let outcomes = ipc::decode_response(&done.stdout, files.len(), LIMITS.output)
        .context("malformed SVG outline child response")?;
    let (files, dropped) = apply(files, outcomes);
    Ok(Outlined {
        files,
        fonts,
        dropped,
    })
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
    drop(buf);
    let outcomes = classify(&request.files, &request.fonts, LIMITS);
    output
        .write_all(&ipc::encode_response(&outcomes))
        .context("write SVG outline response")?;
    output.flush().context("flush SVG outline response")
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;
