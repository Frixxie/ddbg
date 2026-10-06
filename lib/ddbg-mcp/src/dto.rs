//! Serializable views of ddbg types, returned as MCP structured content.

use std::path::Path;

use ddbg_core::breakpoint::{Breakpoint, Location};
use ddbg_core::command::ScopeVariables;
use ddbg_core::event::{ExceptionInfo, StopInfo};
use ddbg_core::frame::StackFrame;
use ddbg_core::session::StopReason;
use ddbg_core::thread::Thread;
use ddbg_core::variable::{Evaluation, Variable};
use ddbg_core::watch::Watch;
use ddbg_driver::{DebugTestResult, Halt, TestCase, TestOutcome};
use schemars::JsonSchema;
use serde::Serialize;

fn path(p: &Path) -> String {
    p.display().to_string()
}

/// Runner evidence, not an interpretation of the debugger's exit code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DebugTestResultDto {
    pub test: String,
    /// passed, failed, ignored, or unknown (including incomplete/missing output).
    pub outcome: &'static str,
    pub counts: Option<TestCountsDto>,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TestCountsDto {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
}

impl From<DebugTestResult> for DebugTestResultDto {
    fn from(result: DebugTestResult) -> Self {
        Self {
            test: result.test,
            outcome: match result.outcome {
                Some(TestOutcome::Passed) => "passed",
                Some(TestOutcome::Failed) => "failed",
                Some(TestOutcome::Ignored) => "ignored",
                None => "unknown",
            },
            counts: result.counts.map(|c| TestCountsDto {
                total: c.total,
                passed: c.passed,
                failed: c.failed,
                ignored: c.ignored,
            }),
            diagnostic: result.diagnostic,
        }
    }
}

/// State of the program after a resuming command.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // short-lived, one per command
pub enum HaltDto {
    Stopped(StopDto),
    Exited {
        exit_code: i64,
    },
    Terminated,
    /// The program did not halt within the timeout and may still run.
    Running {
        timeout_ms: u64,
    },
}

impl From<Halt> for HaltDto {
    fn from(h: Halt) -> Self {
        match h {
            Halt::Stopped(info) => Self::Stopped(info.into()),
            Halt::Exited(exit_code) => Self::Exited { exit_code },
            Halt::Terminated => Self::Terminated,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StopDto {
    /// breakpoint, step, pause, entry, exception or an adapter-specific reason.
    pub reason: String,
    /// Breakpoint ids that were hit, for breakpoint stops.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub breakpoints: Vec<u32>,
    pub thread: Option<i64>,
    pub description: Option<String>,
    pub frame: Option<FrameDto>,
    pub exception: Option<ExceptionDto>,
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub watches: Vec<WatchDto>,
}

impl From<StopInfo> for StopDto {
    fn from(s: StopInfo) -> Self {
        let (reason, breakpoints) = match s.reason {
            StopReason::Breakpoint(ids) => ("breakpoint".into(), ids.iter().map(|i| i.0).collect()),
            StopReason::Step => ("step".into(), vec![]),
            StopReason::Pause => ("pause".into(), vec![]),
            StopReason::Entry => ("entry".into(), vec![]),
            StopReason::Exception(_) => ("exception".into(), vec![]),
            StopReason::Other(o) => (o, vec![]),
        };
        Self {
            reason,
            breakpoints,
            thread: s.thread.map(|t| t.0),
            description: s.description,
            frame: s.frame.map(|f| FrameDto::new(None, f)),
            exception: s.exception.map(Into::into),
            elapsed_ms: s.elapsed.map(|d| d.as_millis() as u64),
            watches: s.watches.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExceptionDto {
    pub id: String,
    pub description: Option<String>,
    pub type_name: Option<String>,
    pub message: Option<String>,
    pub stack_trace: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inner: Vec<ExceptionDto>,
}

impl From<ExceptionInfo> for ExceptionDto {
    fn from(e: ExceptionInfo) -> Self {
        Self {
            id: e.id,
            description: e.description,
            type_name: e.type_name,
            message: e.message,
            stack_trace: e.stack_trace,
            inner: e.inner.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FrameDto {
    /// Position in the backtrace, 0 being the innermost frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    pub name: String,
    pub path: Option<String>,
    pub line: u32,
    pub column: u32,
}

impl FrameDto {
    pub fn new(index: Option<usize>, f: StackFrame) -> Self {
        Self {
            index,
            name: f.name,
            path: f.path.as_deref().map(path).or(f.source_name),
            line: f.line,
            column: f.column,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BreakpointDto {
    pub id: u32,
    /// The location as requested, e.g. `src/main.rs:14` or `my_fn`.
    pub requested: String,
    pub condition: Option<String>,
    pub kind: &'static str,
    /// Where the adapter bound the breakpoint, if known.
    pub resolved: Option<String>,
    pub verified: bool,
    pub message: Option<String>,
}

impl From<Breakpoint> for BreakpointDto {
    fn from(b: Breakpoint) -> Self {
        Self {
            id: b.id.0,
            kind: match b.requested {
                Location::Source(_) => "source",
                Location::Function(_) => "function",
            },
            requested: b.requested.to_string(),
            condition: b.condition,
            resolved: b.resolved.map(|r| r.to_string()),
            verified: b.verified,
            message: b.message.or_else(|| {
                (!b.verified).then(|| {
                    "pending: binds when the code is loaded, or the location has no code".into()
                })
            }),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WatchDto {
    pub id: u32,
    pub expression: String,
    /// Unavailable until evaluated or while the program is not stopped.
    pub value: Option<String>,
    pub type_name: Option<String>,
    pub has_children: bool,
    pub error: Option<String>,
}

impl From<Watch> for WatchDto {
    fn from(w: Watch) -> Self {
        let (value, type_name, has_children, error) = match w.result {
            Some(Ok(v)) => (Some(v.value), v.type_name, v.has_children, None),
            Some(Err(e)) => (None, None, false, Some(e)),
            None => (None, None, false, None),
        };
        Self {
            id: w.id.0,
            expression: w.expression,
            value,
            type_name,
            has_children,
            error,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThreadDto {
    pub id: i64,
    pub name: String,
}

impl From<Thread> for ThreadDto {
    fn from(t: Thread) -> Self {
        Self {
            id: t.id.0,
            name: t.name,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VariableDto {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// Whether the value has children that can be inspected with `evaluate`.
    pub has_children: bool,
    /// Expression that evaluates to this variable.
    pub evaluate_name: Option<String>,
}

impl From<Variable> for VariableDto {
    fn from(v: Variable) -> Self {
        Self {
            name: v.name,
            value: v.value,
            type_name: v.type_name,
            has_children: v.children.is_some(),
            evaluate_name: v.evaluate_name,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EvaluationDto {
    pub value: String,
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<VariableDto>,
}

impl EvaluationDto {
    pub fn new(e: Evaluation, children: Vec<Variable>) -> Self {
        Self {
            value: e.value,
            type_name: e.type_name,
            children: children.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScopeDto {
    pub scope: String,
    pub variables: Vec<VariableDto>,
}

impl From<ScopeVariables> for ScopeDto {
    fn from(s: ScopeVariables) -> Self {
        Self {
            scope: s.scope,
            variables: s.variables.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TestCaseDto {
    /// 1-based index, usable in place of the name in `run_test`/`debug_test`.
    pub index: usize,
    pub name: String,
    pub display_name: String,
    pub source: Option<String>,
    pub line: Option<u32>,
}

impl TestCaseDto {
    pub fn new(index: usize, t: TestCase) -> Self {
        Self {
            index,
            name: t.name,
            display_name: t.display_name,
            source: t.source.as_deref().map(path),
            line: t.line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddbg_core::breakpoint::{BreakpointId, SourceLocation};
    use ddbg_core::frame::FrameId;

    fn frame() -> StackFrame {
        StackFrame {
            id: FrameId(1),
            name: "main".into(),
            path: Some("/p/src/main.rs".into()),
            source_name: Some("main.rs".into()),
            line: 14,
            column: 5,
        }
    }

    #[test]
    fn stopped_halt_serializes_with_state_tag() {
        let halt = Halt::Stopped(StopInfo {
            reason: StopReason::Breakpoint(vec![BreakpointId(1)]),
            thread: None,
            description: None,
            frame: Some(frame()),
            exception: None,
            elapsed: None,
            watches: Vec::new(),
        });
        let v = serde_json::to_value(HaltDto::from(halt)).unwrap();
        assert_eq!(v["state"], "stopped");
        assert_eq!(v["reason"], "breakpoint");
        assert_eq!(v["breakpoints"][0], 1);
        assert_eq!(v["frame"]["line"], 14);
        assert_eq!(v["frame"]["path"], "/p/src/main.rs");
    }

    #[test]
    fn exited_halt() {
        let v = serde_json::to_value(HaltDto::from(Halt::Exited(3))).unwrap();
        assert_eq!(v, serde_json::json!({"state": "exited", "exit_code": 3}));
    }

    #[test]
    fn test_results_report_failed_selection_and_unknown_without_changing_exit() {
        let result = DebugTestResult {
            test: "Ns.Tests.Selected".into(),
            outcome: Some(TestOutcome::Failed),
            counts: Some(ddbg_driver::TestCounts {
                total: 0,
                passed: 0,
                failed: 0,
                ignored: 0,
            }),
            diagnostic: Some("Zero tests ran".into()),
        };
        let mut value = serde_json::to_value(HaltDto::from(Halt::Exited(0))).unwrap();
        value["test_result"] = serde_json::to_value(DebugTestResultDto::from(result)).unwrap();
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["test_result"]["outcome"], "failed");
        assert_eq!(value["test_result"]["counts"]["total"], 0);
        let result = DebugTestResult {
            test: "Ns.Tests.Selected".into(),
            outcome: None,
            counts: None,
            diagnostic: None,
        };
        let value = serde_json::to_value(DebugTestResultDto::from(result)).unwrap();
        assert_eq!(value["outcome"], "unknown");
        assert!(value["counts"].is_null());
    }

    #[test]
    fn breakpoint() {
        let bp = Breakpoint {
            id: BreakpointId(2),
            requested: Location::Source(SourceLocation::new("src/main.rs", 14)),
            condition: Some("point.x == 3".into()),
            resolved: None,
            verified: false,
            message: None,
            adapter_id: None,
        };
        let v = serde_json::to_value(BreakpointDto::from(bp)).unwrap();
        assert_eq!(v["requested"], "src/main.rs:14");
        assert_eq!(v["kind"], "source");
        assert_eq!(v["condition"], "point.x == 3");
        assert!(v["message"].as_str().unwrap().starts_with("pending"));
    }
}
