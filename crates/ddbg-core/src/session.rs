//! Frontend-independent debug session state.

use std::collections::HashMap;

use ddbg_dap::protocol::Capabilities;

use crate::breakpoint::{BreakpointId, BreakpointStore};
use crate::frame::StackFrame;
use crate::target::DebugTarget;
use crate::thread::{Thread, ThreadId};
use crate::variable::{Scope, VarRef, Variable};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SessionStatus {
    #[default]
    Disconnected,
    Initializing,
    Configuring,
    Running,
    Stopped(StopReason),
    Terminated,
}

impl SessionStatus {
    /// An adapter connection exists and the debuggee may be alive.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Initializing | Self::Configuring | Self::Running | Self::Stopped(_)
        )
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint(Vec<BreakpointId>),
    Step,
    Pause,
    Entry,
    Exception(Option<String>),
    Other(String),
}

impl StopReason {
    pub fn from_dap(reason: &str, text: Option<String>, hit: Vec<BreakpointId>) -> Self {
        match reason {
            "breakpoint" | "function breakpoint" | "data breakpoint" => Self::Breakpoint(hit),
            "step" | "goto" => Self::Step,
            "pause" => Self::Pause,
            "entry" => Self::Entry,
            "exception" => Self::Exception(text),
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Generic DAP features, determined by capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    ConfigurationDone,
    ConditionalBreakpoints,
    FunctionBreakpoints,
    TerminateRequest,
    TerminateDebuggee,
    SetVariable,
    StepBack,
    Completions,
}

#[derive(Debug, Default)]
pub struct DebugSession {
    pub status: SessionStatus,
    pub target: Option<DebugTarget>,
    pub capabilities: Capabilities,

    pub breakpoints: BreakpointStore,

    pub threads: Vec<Thread>,
    pub selected_thread: Option<ThreadId>,

    pub stack: Vec<StackFrame>,
    /// Index into `stack`.
    pub selected_frame: Option<usize>,

    pub scopes: Vec<Scope>,
    pub variable_cache: HashMap<VarRef, Vec<Variable>>,

    pub exit_code: Option<i64>,
}

impl DebugSession {
    pub fn supports(&self, feature: Feature) -> bool {
        let c = &self.capabilities;
        match feature {
            Feature::ConfigurationDone => c.supports_configuration_done_request,
            Feature::ConditionalBreakpoints => c.supports_conditional_breakpoints,
            Feature::FunctionBreakpoints => c.supports_function_breakpoints,
            Feature::TerminateRequest => c.supports_terminate_request,
            Feature::TerminateDebuggee => c.support_terminate_debuggee,
            Feature::SetVariable => c.supports_set_variable,
            Feature::StepBack => c.supports_step_back,
            Feature::Completions => c.supports_completions_request,
        }
    }

    pub fn current_frame(&self) -> Option<&StackFrame> {
        self.selected_frame.and_then(|i| self.stack.get(i))
    }

    /// Drop everything that is only valid while suspended.
    /// No variable reference survives a resume.
    pub fn on_resume(&mut self) {
        self.stack.clear();
        self.selected_frame = None;
        self.scopes.clear();
        self.variable_cache.clear();
    }

    /// Reset per-run state when a debuggee goes away.
    pub fn on_terminated(&mut self) {
        self.on_resume();
        self.threads.clear();
        self.selected_thread = None;
        self.capabilities = Capabilities::default();
        self.breakpoints.reset_resolution();
        self.status = SessionStatus::Terminated;
    }
}
