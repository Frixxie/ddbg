//! Application events consumed by frontends. Independent of raw DAP events.

use ddbg_dap::protocol as dap;

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
    /// A value was assigned; cached variables are stale.
    VariablesChanged,

    Output(Output),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopInfo {
    pub reason: StopReason,
    pub thread: Option<ThreadId>,
    pub description: Option<String>,
    /// Top frame of the stopped thread, if available.
    pub frame: Option<StackFrame>,
    /// Details from an `exceptionInfo` request, for exception stops.
    pub exception: Option<ExceptionInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExceptionInfo {
    /// Adapter-specific id, typically the exception type name.
    pub id: String,
    pub description: Option<String>,
    pub break_mode: Option<String>,
    pub type_name: Option<String>,
    pub message: Option<String>,
    pub stack_trace: Option<String>,
    /// Outermost first.
    pub inner: Vec<ExceptionInfo>,
}

impl ExceptionInfo {
    fn from_details(d: dap::ExceptionDetails) -> Self {
        let type_name = d.full_type_name.or(d.type_name);
        Self {
            id: type_name.clone().unwrap_or_default(),
            description: None,
            break_mode: None,
            type_name,
            message: d.message,
            stack_trace: d.stack_trace,
            inner: d
                .inner_exception
                .into_iter()
                .map(Self::from_details)
                .collect(),
        }
    }
}

impl From<dap::ExceptionInfoResponse> for ExceptionInfo {
    fn from(r: dap::ExceptionInfoResponse) -> Self {
        let mut info = r.details.map(Self::from_details).unwrap_or_default();
        info.id = r.exception_id;
        info.description = r.description;
        info.break_mode = r.break_mode;
        info
    }
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
