//! `--typst-compiler`/`--typst-bin`/`--typst-timeout-secs`/
//! `--typst-max-output-bytes`, shared by `render` and `render-template`
//! (issue #73) via `#[command(flatten)]`.
//! Split out of `main.rs` to keep it under the size cap.

/// `--typst-bin`, `--typst-timeout-secs` and `--typst-max-output-bytes` only
/// matter when `--typst-compiler cli` is selected — `library` (the default)
/// ignores them.
#[derive(clap::Args)]
pub struct TypstCompilerArgs {
    /// Typst compiler backend: the in-process library (default), or a
    /// `typst` CLI subprocess that can be killed on timeout and
    /// memory-capped — the in-process compiler structurally cannot offer
    /// either.
    #[arg(long = "typst-compiler", value_enum, default_value_t = TypstCompilerArg::Library)]
    mode: TypstCompilerArg,
    /// Path to the `typst` binary, looked up on `PATH` unless it's itself a
    /// path.
    #[arg(long = "typst-bin", default_value = "typst")]
    bin: String,
    /// Compile timeout. Exceeded → the child is killed and reaped before the
    /// command reports the failure.
    #[arg(
        long = "typst-timeout-secs",
        default_value_t = 30,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    timeout_secs: u64,
    /// Largest artifact a compile may produce; a bigger one fails the render
    /// before it is read into memory.
    #[arg(long = "typst-max-output-bytes", default_value_t = mdcast::backends::typst::DEFAULT_MAX_OUTPUT_BYTES)]
    max_output_bytes: u64,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum TypstCompilerArg {
    Library,
    Cli,
}

impl TypstCompilerArgs {
    /// In `cli` mode the SVG-outline step runs as this same binary's hidden
    /// `__outline-svg` subcommand, under the compile's timeout.
    pub async fn build(&self) -> anyhow::Result<mdcast::backends::typst::TypstCompiler> {
        use anyhow::Context;
        use mdcast::backends::typst::{CliCompiler, OutlineCommand, TypstCompiler};
        let compiler = match self.mode {
            TypstCompilerArg::Library => TypstCompiler::Library,
            TypstCompilerArg::Cli => TypstCompiler::Cli(
                CliCompiler::new(
                    self.bin.clone(),
                    std::time::Duration::from_secs(self.timeout_secs),
                )
                .with_max_output_bytes(self.max_output_bytes)
                .with_outline(Some(OutlineCommand::current_exe()?)),
            ),
        };
        compiler.check_startup().await.context(
            "--typst-compiler cli needs a working typst binary (see --typst-bin); \
             use --typst-compiler library to compile in-process instead",
        )?;
        Ok(compiler)
    }
}
