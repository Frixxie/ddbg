//! Frontend-independent debug session state.

use std::collections::HashMap;

use ddbg_dap::protocol::Capabilities;

use crate::breakpoint::{BreakpointId, BreakpointStore};
use crate::frame::StackFrame;
use crate::target::DebugTarget;
use crate::thread::{Thread, ThreadId};
use crate::variable::{Scope, VarRef, Variable};
use crate::watch::WatchStore;

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
    SetExpression,
    StepBack,
    Completions,
    ExceptionInfo,
}

#[derive(Debug, Default)]
pub struct DebugSession {
    pub status: SessionStatus,
    pub target: Option<DebugTarget>,
    pub capabilities: Capabilities,

    pub breakpoints: BreakpointStore,
    pub watches: WatchStore,

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
            Feature::SetExpression => c.supports_set_expression,
            Feature::StepBack => c.supports_step_back,
            Feature::Completions => c.supports_completions_request,
            Feature::ExceptionInfo => c.supports_exception_info_request,
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
        self.watches.invalidate();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breakpoint::{FunctionLocation, Location};
    use crate::frame::FrameId;
    use crate::target::AttachTarget;
    use quickcheck_macros::quickcheck;

    fn populated_session(entries: Vec<(String, String)>, exit_code: i64) -> DebugSession {
        let mut session = DebugSession {
            status: SessionStatus::Stopped(StopReason::Pause),
            target: Some(DebugTarget::Attach(AttachTarget { pid: 42 })),
            exit_code: Some(exit_code),
            capabilities: Capabilities {
                supports_completions_request: true,
                ..Default::default()
            },
            ..Default::default()
        };
        for (i, (name, value)) in entries.into_iter().enumerate() {
            let id = i as i64 + 1;
            session.threads.push(Thread {
                id: ThreadId(id),
                name: name.clone(),
            });
            session.stack.push(StackFrame {
                id: FrameId(id),
                name: name.clone(),
                path: None,
                source_name: None,
                line: 1,
                column: 1,
            });
            session.scopes.push(Scope {
                name: name.clone(),
                reference: VarRef(id),
                expensive: false,
                is_locals: true,
            });
            session.variable_cache.insert(
                VarRef(id),
                vec![Variable {
                    name: name.clone(),
                    value: value.clone(),
                    type_name: None,
                    children: Some(VarRef(id)),
                    evaluate_name: None,
                }],
            );
            let (watch, _) = session.watches.add(name.clone());
            session.watches.set_result(watch, Err(value.clone()));
            let (breakpoint, _) = session.breakpoints.add(
                Location::Function(FunctionLocation::new(name, None)),
                std::path::Path::new("/"),
            );
            session.breakpoints.set_condition(breakpoint, Some(value));
        }
        if !session.stack.is_empty() {
            session.selected_frame = Some(session.stack.len() - 1);
            session.selected_thread = session.threads.last().map(|thread| thread.id);
        }
        let results: Vec<_> = session
            .breakpoints
            .iter()
            .map(|breakpoint| ddbg_dap::protocol::Breakpoint {
                id: Some(i64::from(breakpoint.id.0)),
                verified: true,
                message: Some("resolved".into()),
                source: Some(ddbg_dap::protocol::Source {
                    path: Some("/source.rs".into()),
                    ..Default::default()
                }),
                line: Some(42),
                column: None,
            })
            .collect();
        session.breakpoints.apply_function_results(&results);
        session
    }

    fn suspended_state_is_empty(session: &DebugSession) -> bool {
        session.stack.is_empty()
            && session.selected_frame.is_none()
            && session.scopes.is_empty()
            && session.variable_cache.is_empty()
            && session
                .watches
                .snapshot()
                .iter()
                .all(|watch| watch.result.is_none())
    }

    #[quickcheck]
    fn resume_only_clears_suspended_state(entries: Vec<(String, String)>, exit_code: i64) -> bool {
        let mut session = populated_session(entries, exit_code);
        let breakpoints: Vec<_> = session.breakpoints.iter().cloned().collect();
        let mut watches = session.watches.snapshot();
        for watch in &mut watches {
            watch.result = None;
        }
        let threads = session.threads.clone();
        let selected_thread = session.selected_thread;
        let capabilities = session.capabilities.clone();
        let target = session.target.clone();
        for _ in 0..2 {
            session.on_resume();
            assert!(suspended_state_is_empty(&session));
            assert_eq!(
                session.breakpoints.iter().cloned().collect::<Vec<_>>(),
                breakpoints
            );
            assert_eq!(session.watches.snapshot(), watches);
            assert_eq!(session.threads, threads);
            assert_eq!(session.selected_thread, selected_thread);
            assert_eq!(session.capabilities, capabilities);
            assert_eq!(session.target, target);
            assert_eq!(session.exit_code, Some(exit_code));
            assert_eq!(session.status, SessionStatus::Stopped(StopReason::Pause));
        }
        true
    }

    #[quickcheck]
    fn termination_clears_per_run_state_but_keeps_configuration(
        entries: Vec<(String, String)>,
        exit_code: i64,
    ) -> bool {
        let mut session = populated_session(entries, exit_code);
        let mut breakpoints: Vec<_> = session.breakpoints.iter().cloned().collect();
        for breakpoint in &mut breakpoints {
            breakpoint.resolved = None;
            breakpoint.verified = false;
            breakpoint.message = None;
            breakpoint.adapter_id = None;
        }
        let mut watches = session.watches.snapshot();
        for watch in &mut watches {
            watch.result = None;
        }
        let target = session.target.clone();
        for _ in 0..2 {
            session.on_terminated();
            assert!(suspended_state_is_empty(&session));
            assert!(session.threads.is_empty());
            assert!(session.selected_thread.is_none());
            assert_eq!(session.capabilities, Capabilities::default());
            assert_eq!(
                session.breakpoints.iter().cloned().collect::<Vec<_>>(),
                breakpoints
            );
            assert_eq!(session.watches.snapshot(), watches);
            assert_eq!(session.target, target);
            assert_eq!(session.exit_code, Some(exit_code));
            assert_eq!(session.status, SessionStatus::Terminated);
        }
        true
    }
}
