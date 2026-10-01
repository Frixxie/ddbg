//! Adapter process transport (stdin/stdout).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::client::{DapClient, Incoming};
use anyhow::{Context, Result, bail};

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

    /// Resolve `program` to an absolute path.
    ///
    /// Looks in `PATH` first; on macOS falls back to `xcrun -f`, which is
    /// where Xcode ships `lldb-dap`.
    pub fn resolve(mut self) -> Result<Self> {
        if let Some(path) = find_program(&self.program) {
            self.program = path.to_string_lossy().into_owned();
            return Ok(self);
        }
        bail!("{} was not found in PATH", self.program)
    }
}

/// Find an executable by name, searching `PATH` (and `xcrun` on macOS).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("xcrun")
            .args(["-f", name])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if out.status.success() {
            let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
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
            .with_context(|| format!("failed to spawn adapter `{}`", cmd.program))?;

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
