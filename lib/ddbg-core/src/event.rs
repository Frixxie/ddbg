//! Application events consumed by frontends. Independent of raw DAP events.

use crate::breakpoint::Breakpoint;
use crate::frame::StackFrame;
use crate::session::StopReason;
use crate::thread::ThreadId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugEvent {
    SessionStarted,
    SessionStopped(StopInfo),
    SessionContinued,
    SessionExited(i64),
    SessionTerminated,

    BreakpointChanged(Breakpoint),
    ThreadsChanged,
    FrameChanged,

    Output(Output),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopInfo {
    pub reason: StopReason,
    pub thread: Option<ThreadId>,
    pub description: Option<String>,
    /// Top frame of the stopped thread, if available.
    pub frame: Option<StackFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCategory {
    Stdout,
    Stderr,
    Console,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub category: OutputCategory,
    pub text: String,
}

impl Output {
    pub fn from_dap(category: Option<&str>, text: String) -> Self {
        let category = match category {
            Some("stdout") => OutputCategory::Stdout,
            Some("stderr") => OutputCategory::Stderr,
            None | Some("console") | Some("important") => OutputCategory::Console,
            Some(_) => OutputCategory::Other,
        };
        Self { category, text }
    }
}
