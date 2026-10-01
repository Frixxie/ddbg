use thiserror::Error;

use crate::breakpoint::BreakpointId;
use ddbg_dap::DapError;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error(transparent)]
    Dap(#[from] DapError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("{0} was not found in PATH")]
    NotFound(String),
    #[error("{adapter} cannot {operation}")]
    Unsupported {
        adapter: String,
        operation: &'static str,
    },
}

/// User-facing errors from executing a command in the wrong state.
#[derive(Debug, Error)]
pub enum CommandError {
    #[error("no program to run; use `run <program> [args...]`")]
    NoTarget,
    #[error("the program is not being run")]
    NotRunning,
    #[error("the program is already running; use `pause` first")]
    AlreadyRunning,
    #[error("the program is running; use `pause` first")]
    NotStopped,
    #[error("no thread selected")]
    NoThread,
    #[error("no thread {0}")]
    NoSuchThread(i64),
    #[error("no frame {0}")]
    NoSuchFrame(usize),
    #[error("no breakpoint {0}")]
    NoSuchBreakpoint(BreakpointId),
    #[error("{0} is not supported by this debug adapter")]
    Unsupported(&'static str),
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
