//! Command-line frontend.

mod adapter_test;
mod args;
mod logging;

use clap::Parser;

pub use args::{Args, Subcommand};

/// Entry point used by the `ddbg` binary.
pub async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    logging::init(&args)?;

    match args.command {
        Some(Subcommand::AdapterTest { ref adapter }) => adapter_test::run(adapter).await,
        None => {
            eprintln!("REPL not implemented yet");
            Ok(())
        }
    }
}
