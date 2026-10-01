//! Command-line frontend.

mod adapter_test;
mod args;
mod commands;
mod logging;
pub mod parser;
mod render;
mod repl;

use std::sync::Arc;

use clap::Parser;
use ddbg_core::adapter::{DebugAdapter, LldbDapAdapter, adapter_for_command};
use ddbg_core::command::Command;
use ddbg_core::{EngineConfig, LaunchTarget, engine};

pub use args::{Args, Subcommand};

/// Entry point used by the `ddbg` binary.
pub async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    logging::init(&args)?;

    if let Some(Subcommand::AdapterTest { adapter }) = &args.command {
        return adapter_test::run(adapter).await;
    }

    let cwd = std::env::current_dir()?;
    let adapter: Arc<dyn DebugAdapter> = match &args.adapter {
        Some(cmd) => {
            let words = parser::split_words(cmd).map_err(anyhow::Error::msg)?;
            let (program, rest) = words
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("empty --adapter"))?;
            adapter_for_command(program, rest.to_vec()).into()
        }
        None => Arc::new(LldbDapAdapter::for_rust()),
    };

    let target = args.program.split_first().map(|(program, rest)| {
        let mut t = LaunchTarget::new(program, rest.to_vec());
        t.stop_on_entry = args.stop_on_entry;
        t
    });

    let mut initial = Vec::new();
    if args.run || (args.stop_on_entry && target.is_some()) {
        initial.push(Command::Run(None));
    }

    let engine = engine::spawn(EngineConfig {
        adapter,
        cwd: cwd.clone(),
        target,
    });
    repl::run(engine, cwd, initial).await
}
