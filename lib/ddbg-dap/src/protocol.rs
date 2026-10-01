//! Hand-written subset of the Debug Adapter Protocol types.
//!
//! Only what ddbg needs is modelled. Unknown fields are ignored, unknown
//! events are preserved as raw [`Event`]s.

use std::collections::BTreeMap;

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    Request(Request),
    Response(Response),
    Event(Event),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub seq: i64,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub seq: i64,
    pub request_seq: i64,
    pub success: bool,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: i64,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// A typed DAP request.
pub trait DapRequest: Serialize {
    const COMMAND: &'static str;
    type Response: DeserializeOwned;
}

/// Response type for requests without a meaningful body.
pub type Empty = IgnoredAny;

macro_rules! request {
    ($ty:ty, $cmd:literal, $resp:ty) => {
        impl DapRequest for $ty {
            const COMMAND: &'static str = $cmd;
            type Response = $resp;
        }
    };
}

// ---------------------------------------------------------------------------
// Common types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakpoint {
    pub line: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    #[serde(default)]
    pub id: Option<i64>,
    pub verified: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub column: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub source: Option<Source>,
    pub line: i64,
    pub column: i64,
    #[serde(default)]
    pub presentation_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    #[serde(default)]
    pub expensive: bool,
    #[serde(default)]
    pub presentation_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    #[serde(default)]
    pub variables_reference: i64,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Adapter capabilities returned from `initialize`.
///
/// Well-known flags are typed; everything else is kept in `other`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub supports_configuration_done_request: bool,
    #[serde(default)]
    pub supports_function_breakpoints: bool,
    #[serde(default)]
    pub supports_conditional_breakpoints: bool,
    #[serde(default)]
    pub supports_hit_conditional_breakpoints: bool,
    #[serde(default)]
    pub supports_evaluate_for_hovers: bool,
    #[serde(default)]
    pub supports_step_back: bool,
    #[serde(default)]
    pub supports_set_variable: bool,
    #[serde(default)]
    pub supports_restart_frame: bool,
    #[serde(default)]
    pub supports_goto_targets_request: bool,
    #[serde(default)]
    pub supports_step_in_targets_request: bool,
    #[serde(default)]
    pub supports_completions_request: bool,
    #[serde(default)]
    pub supports_modules_request: bool,
    #[serde(default)]
    pub supports_exception_info_request: bool,
    #[serde(default)]
    pub support_terminate_debuggee: bool,
    #[serde(default)]
    pub supports_terminate_request: bool,
    #[serde(default)]
    pub supports_data_breakpoints: bool,
    #[serde(default)]
    pub supports_read_memory_request: bool,
    #[serde(default)]
    pub supports_disassemble_request: bool,
    #[serde(default)]
    pub supports_log_points: bool,
    #[serde(default)]
    pub supports_single_thread_execution_requests: bool,
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeArguments {
    #[serde(rename = "clientID")]
    pub client_id: String,
    pub client_name: String,
    #[serde(rename = "adapterID")]
    pub adapter_id: String,
    pub locale: String,
    pub lines_start_at1: bool,
    pub columns_start_at1: bool,
    pub path_format: String,
    pub supports_variable_type: bool,
    pub supports_variable_paging: bool,
    pub supports_run_in_terminal_request: bool,
    pub supports_progress_reporting: bool,
}

impl InitializeArguments {
    pub fn new(adapter_id: impl Into<String>) -> Self {
        Self {
            client_id: "ddbg".into(),
            client_name: "ddbg".into(),
            adapter_id: adapter_id.into(),
            locale: "en-US".into(),
            lines_start_at1: true,
            columns_start_at1: true,
            path_format: "path".into(),
            supports_variable_type: true,
            supports_variable_paging: false,
            supports_run_in_terminal_request: false,
            supports_progress_reporting: false,
        }
    }
}
request!(InitializeArguments, "initialize", Capabilities);

/// `launch` arguments are adapter specific.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct LaunchArguments(pub Value);
request!(LaunchArguments, "launch", Empty);

/// `attach` arguments are adapter specific.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct AttachArguments(pub Value);
request!(AttachArguments, "attach", Empty);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsArguments {
    pub source: Source,
    pub breakpoints: Vec<SourceBreakpoint>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct SetBreakpointsResponse {
    pub breakpoints: Vec<Breakpoint>,
}
request!(
    SetBreakpointsArguments,
    "setBreakpoints",
    SetBreakpointsResponse
);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionBreakpoint {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// Replaces *all* function breakpoints of the session.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFunctionBreakpointsArguments {
    pub breakpoints: Vec<FunctionBreakpoint>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct SetFunctionBreakpointsResponse {
    pub breakpoints: Vec<Breakpoint>,
}
request!(
    SetFunctionBreakpointsArguments,
    "setFunctionBreakpoints",
    SetFunctionBreakpointsResponse
);

#[derive(Debug, Clone, Serialize)]
pub struct ConfigurationDoneArguments {}
request!(ConfigurationDoneArguments, "configurationDone", Empty);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueArguments {
    pub thread_id: i64,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueResponse {
    #[serde(default)]
    pub all_threads_continued: Option<bool>,
}
request!(ContinueArguments, "continue", ContinueResponse);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseArguments {
    pub thread_id: i64,
}
request!(PauseArguments, "pause", Empty);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextArguments {
    pub thread_id: i64,
}
request!(NextArguments, "next", Empty);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepInArguments {
    pub thread_id: i64,
}
request!(StepInArguments, "stepIn", Empty);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutArguments {
    pub thread_id: i64,
}
request!(StepOutArguments, "stepOut", Empty);

#[derive(Debug, Clone, Serialize)]
pub struct ThreadsArguments {}
#[derive(Debug, Clone, Deserialize)]
pub struct ThreadsResponse {
    pub threads: Vec<Thread>,
}
request!(ThreadsArguments, "threads", ThreadsResponse);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTraceArguments {
    pub thread_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_frame: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<i64>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTraceResponse {
    pub stack_frames: Vec<StackFrame>,
    #[serde(default)]
    pub total_frames: Option<i64>,
}
request!(StackTraceArguments, "stackTrace", StackTraceResponse);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopesArguments {
    pub frame_id: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ScopesResponse {
    pub scopes: Vec<Scope>,
}
request!(ScopesArguments, "scopes", ScopesResponse);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariablesArguments {
    pub variables_reference: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct VariablesResponse {
    pub variables: Vec<Variable>,
}
request!(VariablesArguments, "variables", VariablesResponse);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateArguments {
    pub expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateResponse {
    pub result: String,
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    #[serde(default)]
    pub variables_reference: i64,
}
request!(EvaluateArguments, "evaluate", EvaluateResponse);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisconnectArguments {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate_debuggee: Option<bool>,
}
request!(DisconnectArguments, "disconnect", Empty);

#[derive(Debug, Clone, Default, Serialize)]
pub struct TerminateArguments {}
request!(TerminateArguments, "terminate", Empty);

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedEvent {
    pub reason: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub thread_id: Option<i64>,
    #[serde(default)]
    pub all_threads_stopped: Option<bool>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub hit_breakpoint_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContinuedEvent {
    pub thread_id: i64,
    #[serde(default)]
    pub all_threads_continued: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExitedEvent {
    pub exit_code: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OutputEvent {
    #[serde(default)]
    pub category: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadEvent {
    pub reason: String,
    pub thread_id: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BreakpointEvent {
    pub reason: String,
    pub breakpoint: Breakpoint,
}

/// Typed view of an adapter event.
#[derive(Debug, Clone, PartialEq)]
pub enum DapEvent {
    Initialized,
    Stopped(StoppedEvent),
    Continued(ContinuedEvent),
    Exited(ExitedEvent),
    Terminated,
    Output(OutputEvent),
    Thread(ThreadEvent),
    Breakpoint(BreakpointEvent),
    /// Unknown or unparseable events are preserved verbatim.
    Other(Event),
}

impl From<Event> for DapEvent {
    fn from(event: Event) -> Self {
        fn parse<T: DeserializeOwned>(e: &Event) -> Option<T> {
            serde_json::from_value(e.body.clone().unwrap_or(Value::Null)).ok()
        }
        let typed = match event.event.as_str() {
            "initialized" => Some(DapEvent::Initialized),
            "terminated" => Some(DapEvent::Terminated),
            "stopped" => parse(&event).map(DapEvent::Stopped),
            "continued" => parse(&event).map(DapEvent::Continued),
            "exited" => parse(&event).map(DapEvent::Exited),
            "output" => parse(&event).map(DapEvent::Output),
            "thread" => parse(&event).map(DapEvent::Thread),
            "breakpoint" => parse(&event).map(DapEvent::Breakpoint),
            _ => None,
        };
        typed.unwrap_or(DapEvent::Other(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_stopped_event() {
        let e: Event = serde_json::from_value(json!({
            "seq": 3, "type": "event", "event": "stopped",
            "body": {"reason": "breakpoint", "threadId": 7, "hitBreakpointIds": [1]}
        }))
        .unwrap();
        let DapEvent::Stopped(s) = DapEvent::from(e) else {
            panic!()
        };
        assert_eq!(s.thread_id, Some(7));
        assert_eq!(s.hit_breakpoint_ids, vec![1]);
    }

    #[test]
    fn unknown_event_is_preserved() {
        let e = Event {
            seq: 1,
            event: "progressStart".into(),
            body: Some(json!({"x": 1})),
        };
        assert!(matches!(DapEvent::from(e), DapEvent::Other(_)));
    }

    #[test]
    fn capabilities_keep_unknown_fields() {
        let c: Capabilities = serde_json::from_value(json!({
            "supportsConfigurationDoneRequest": true,
            "exceptionBreakpointFilters": []
        }))
        .unwrap();
        assert!(c.supports_configuration_done_request);
        assert!(c.other.contains_key("exceptionBreakpointFilters"));
    }

    #[test]
    fn response_envelope_roundtrip() {
        let m: Message = serde_json::from_value(json!({
            "seq": 2, "type": "response", "request_seq": 1,
            "success": true, "command": "initialize"
        }))
        .unwrap();
        assert!(matches!(
            m,
            Message::Response(Response { request_seq: 1, .. })
        ));
    }
}
