//! Adapter process transport (stdin/stdout).

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::client::{DapClient, Incoming};
use crate::error::{DapError, Result};

/// How to start a debug adapter process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl AdapterCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
}

/// A running adapter process plus its DAP connection.
pub struct AdapterProcess {
    pub child: Child,
    pub client: DapClient,
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
}

impl AdapterProcess {
    /// Spawn the adapter and connect a [`DapClient`] to its stdio.
    /// Adapter stderr is forwarded to the `dap::stderr` tracing target.
    pub fn spawn(cmd: &AdapterCommand) -> Result<Self> {
        let mut child = Command::new(&cmd.program)
            .args(&cmd.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| DapError::Spawn {
                command: cmd.program.clone(),
                source,
            })?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "dap::stderr", "{line}");
                }
            });
        }

        let (client, incoming) = DapClient::connect(stdout, stdin);
        Ok(Self {
            child,
            client,
            incoming,
        })
    }
}
