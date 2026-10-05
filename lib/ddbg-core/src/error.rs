//! User-facing error messages. Errors are `anyhow::Error`s throughout.

pub use anyhow::{Error, Result};

pub const NOT_RUNNING: &str = "the program is not being run";
pub const NOT_STOPPED: &str = "the program is running; use `pause` first";
pub const NO_THREAD: &str = "no thread selected";
pub const NO_TARGET: &str = "no program to run; use `run <program> [args...]`";
pub const NO_FUNCTION_BREAKPOINTS: &str = "the debug adapter does not support function breakpoints";
pub const NO_CONDITIONAL_BREAKPOINTS: &str =
    "the debug adapter does not support conditional breakpoints";
