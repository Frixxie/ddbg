use std::path::PathBuf;

use clap::Parser;

/// A terminal-first, language-agnostic debugger with first-class test
/// debugging, built on DAP.
#[derive(Debug, Parser)]
#[command(name = "ddbg", version = crate::BUILD_VERSION, args_conflicts_with_subcommands = true)]
pub struct Args {
    /// Debug adapter command (default by language: lldb-dap for Rust/C/C++,
    /// netcoredbg for .NET, `python3 -m debugpy.adapter` for Python).
    #[arg(long, global = true)]
    pub adapter: Option<String>,

    /// Write DAP traffic and debug logs to this file.
    #[arg(long, value_name = "FILE", global = true)]
    pub log_dap: Option<PathBuf>,

    /// Show debug adapter console messages.
    #[arg(short, long)]
    pub verbose: bool,

    /// Stop at the program entry point after launching.
    #[arg(long)]
    pub stop_on_entry: bool,

    /// Launch immediately instead of waiting for `run`.
    #[arg(long)]
    pub run: bool,

    /// Attach immediately to an existing local process.
    #[arg(long, value_name = "PID", value_parser = clap::value_parser!(u32).range(1..), conflicts_with_all = ["run", "stop_on_entry", "program"])]
    pub attach: Option<u32>,

    /// Disable auto-discovery of the project and binary to debug.
    #[arg(long)]
    pub no_detect: bool,

    /// Use the full-screen terminal UI instead of the line REPL.
    #[arg(long)]
    pub tui: bool,

    #[command(subcommand)]
    pub command: Option<Subcommand>,

    /// Program to debug, followed by its arguments (after `--`).
    #[arg(last = true, value_name = "PROGRAM [ARGS]...")]
    pub program: Vec<String>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Subcommand {
    /// Initialize a debug adapter and print its capabilities.
    AdapterTest {
        /// Adapter command, e.g. `lldb-dap` or `netcoredbg --interpreter=vscode`.
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        adapter: Vec<String>,
    },
    /// Serve the debugger to AI agents over the Model Context Protocol (stdio).
    Mcp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_program_after_dashes() {
        let a = Args::try_parse_from(["ddbg", "--run", "--", "./foo", "-x", "1"]).unwrap();
        assert!(a.run);
        assert_eq!(a.program, ["./foo", "-x", "1"]);
    }

    #[test]
    fn attach_requires_a_positive_pid_and_conflicts_with_launch_options() {
        let a = Args::try_parse_from(["ddbg", "--attach", "42"]).unwrap();
        assert_eq!(a.attach, Some(42));
        for args in [
            vec!["ddbg", "--attach", "0"],
            vec!["ddbg", "--attach", "42", "--run"],
            vec!["ddbg", "--attach", "42", "--stop-on-entry"],
            vec!["ddbg", "--attach", "42", "--", "app"],
        ] {
            assert!(Args::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn parses_adapter_test() {
        let a =
            Args::try_parse_from(["ddbg", "adapter-test", "netcoredbg", "--interpreter=vscode"])
                .unwrap();
        let Some(Subcommand::AdapterTest { adapter }) = a.command else {
            panic!()
        };
        assert_eq!(adapter, ["netcoredbg", "--interpreter=vscode"]);
    }
}
