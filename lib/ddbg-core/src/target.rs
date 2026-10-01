//! Debug targets: everything debuggable reduces to launch or attach.

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugTarget {
    Launch(LaunchTarget),
    Attach(AttachTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTarget {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub stop_on_entry: bool,
}

impl LaunchTarget {
    /// Launch `program` with `args` from the current directory.
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            env: BTreeMap::new(),
            stop_on_entry: false,
        }
    }

    /// `program` made absolute relative to `cwd`.
    pub fn absolute_program(&self) -> PathBuf {
        if self.program.is_absolute() {
            self.program.clone()
        } else {
            self.cwd.join(&self.program)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachTarget {
    pub pid: u32,
}
