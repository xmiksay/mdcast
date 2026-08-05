//! `--typst-compiler`/`--typst-bin`/`--typst-timeout-secs`, shared by
//! `render` and `render-template` (issue #73) via `#[command(flatten)]`.
//! Split out of `main.rs` to keep it under the size cap.

/// `--typst-bin` and `--typst-timeout-secs` only matter when
/// `--typst-compiler cli` is selected — `library` (the default) ignores
/// them.
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
    #[arg(long = "typst-timeout-secs", default_value_t = 30)]
    timeout_secs: u64,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum TypstCompilerArg {
    Library,
    Cli,
}

impl TypstCompilerArgs {
    pub async fn build(&self) -> mdcast::backends::typst::TypstCompiler {
        use mdcast::backends::typst::{CliCompiler, TypstCompiler};
        let compiler = match self.mode {
            TypstCompilerArg::Library => TypstCompiler::Library,
            TypstCompilerArg::Cli => TypstCompiler::Cli(CliCompiler::new(
                self.bin.clone(),
                std::time::Duration::from_secs(self.timeout_secs),
            )),
        };
        compiler.log_startup_info().await;
        compiler
    }
}
