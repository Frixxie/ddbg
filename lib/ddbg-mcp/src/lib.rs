//! Model Context Protocol server for ddbg.
//!
//! Exposes one debug session at a time as MCP tools, built on
//! [`ddbg_driver::Debugger`]. Commands that resume the program wait until it
//! halts (or a timeout passes) and report the resulting state. While such a
//! command waits, `pause`, `kill` and `end_session` still act immediately.

mod dto;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ddbg_cli::testing::render_list;
use ddbg_cli::{Args, Parser};
use ddbg_core::EngineHandle;
use ddbg_core::breakpoint::{BreakpointId, FunctionLocation, SourceLocation};
use ddbg_core::command::{Command, FrameSelector, Location, Reply};
use ddbg_core::thread::ThreadId;
use ddbg_driver::{Debugger, Timeout};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

pub use dto::*;

type ToolResult = Result<CallToolResult, ErrorData>;

/// Serve MCP over stdin/stdout until the client disconnects.
pub async fn serve_stdio() -> anyhow::Result<()> {
    let service = DdbgServer::new().serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

struct State {
    dbg: Debugger,
    /// Bytes of stdout/stderr already returned by `get_output`.
    stdout_seen: usize,
    stderr_seen: usize,
}

#[derive(Clone)]
pub struct DdbgServer {
    state: Arc<Mutex<Option<State>>>,
    /// The session's engine, reachable while a resuming command holds `state`.
    engine: Arc<std::sync::Mutex<Option<EngineHandle>>>,
    tool_router: ToolRouter<Self>,
}

impl Default for DdbgServer {
    fn default() -> Self {
        Self::new()
    }
}

// --- Parameters -------------------------------------------------------------

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct StartSessionParams {
    /// Project directory to debug in. Defaults to the server's working directory.
    pub cwd: Option<String>,
    /// Program to debug. When omitted, the binary is detected from the project.
    pub program: Option<String>,
    /// Arguments passed to the program.
    #[serde(default)]
    pub args: Vec<String>,
    /// Debug adapter command, e.g. `lldb-dap` or `netcoredbg --interpreter=vscode`.
    pub adapter: Option<String>,
    /// Stop at the program entry point; launches the program immediately.
    #[serde(default)]
    pub stop_on_entry: bool,
    /// Disable project and binary auto-detection.
    #[serde(default)]
    pub no_detect: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ResumeParams {
    /// How long to wait for the program to halt, in milliseconds (default 30000).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RunParams {
    /// Program to run instead of the current one. Bare names resolve against
    /// detected project binaries.
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// How long to wait for the program to halt, in milliseconds (default 30000).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetBreakpointParams {
    /// Source file, relative to the session directory or absolute. Combine with
    /// `line` for a line breakpoint, or with `function` to scope a function
    /// breakpoint to this file.
    pub file: Option<String>,
    /// 1-based line number in `file`.
    pub line: Option<u32>,
    /// Function name for a function breakpoint.
    pub function: Option<String>,
    /// Adapter-native expression; execution stops only when it is true.
    pub condition: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetBreakpointConditionParams {
    pub id: u32,
    /// New adapter-native condition. Omit to make the breakpoint unconditional.
    pub condition: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdParams {
    pub id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WatchParams {
    /// Adapter-native expression, evaluated in the selected frame at each stop.
    pub expression: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FrameParams {
    /// Frame index (0 = innermost), `up` or `down`. Omit to show the current frame.
    pub frame: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BacktraceParams {
    /// Include frames without source, e.g. framework internals (default false).
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvaluateParams {
    /// Expression to evaluate in the selected frame.
    pub expression: String,
    /// Evaluate in the adapter's REPL context, which may have side effects
    /// (e.g. calling functions or debugger commands).
    #[serde(default)]
    pub repl: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetValueParams {
    /// L-value expression to assign to, e.g. `point.x`.
    pub target: String,
    /// New value as an expression.
    pub value: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListTestsParams {
    /// Case-insensitive substring filter on the test name.
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TestParams {
    /// Fully-qualified test name, or the index from the last `list_tests`.
    pub test: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugTestParams {
    /// Fully-qualified test name, or the index from the last `list_tests`.
    pub test: String,
    /// Set a breakpoint at the start of the test.
    #[serde(default)]
    pub break_at_start: bool,
    /// How long to wait for the program to halt, in milliseconds (default 30000).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplParams {
    /// A ddbg REPL command line, e.g. `break src/main.rs:14` or `help`.
    pub line: String,
}

// --- Results ----------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct SessionInfo {
    pub cwd: String,
    /// Detected project kind: Rust, C, DotNet or Python.
    pub project: Option<String>,
    pub program: Option<String>,
    /// Binaries found by project detection, usable with `run`.
    pub candidates: Vec<String>,
    /// Project detection messages, e.g. that the binary is not built yet.
    pub notes: Vec<String>,
}

// --- Helpers ----------------------------------------------------------------

/// A result with a human-readable summary (as the REPL prints it) followed
/// by the structured value, also serialized as text for clients that
/// ignore structured content. MCP requires structured content to be an
/// object, so other values are wrapped in `{ "result": ... }`.
fn ok(summary: Option<String>, value: impl Serialize) -> ToolResult {
    let value =
        serde_json::to_value(value).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    let value = match value {
        Value::Object(_) => value,
        other => serde_json::json!({ "result": other }),
    };
    let mut result = CallToolResult::structured(value);
    if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
        result.content.insert(0, ContentBlock::text(s));
    }
    Ok(result)
}

fn text(t: impl Into<String>) -> ToolResult {
    Ok(CallToolResult::success(vec![ContentBlock::text(t.into())]))
}

fn fail(e: anyhow::Error) -> ToolResult {
    Ok(CallToolResult::error(vec![ContentBlock::text(format!(
        "{e:#}"
    ))]))
}

fn no_session() -> ToolResult {
    fail(anyhow::anyhow!(
        "no active debug session; call `start_session` first"
    ))
}

fn unexpected(reply: Reply) -> Value {
    Value::String(format!("{reply:?}"))
}

fn to_value(v: impl Serialize) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

enum Resume {
    Run(Option<(String, Vec<String>)>),
    Continue,
    Next,
    Step,
    Finish,
    Pause,
    Wait,
    TestDebug(String, bool),
}

impl DdbgServer {
    fn engine(&self) -> Option<EngineHandle> {
        self.engine.lock().unwrap().clone()
    }

    /// Run a resuming command and report how the program halted.
    async fn resume(&self, cmd: Resume, timeout_ms: Option<u64>) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        let dbg = &mut st.dbg;
        let previous = dbg.timeout();
        let timeout = timeout_ms.map_or(previous, Duration::from_millis);
        dbg.set_timeout(timeout);
        // Only report events caused by this command.
        dbg.take_transcript();
        let result = match cmd {
            Resume::Run(None) => dbg.run().await,
            Resume::Run(Some((p, a))) => dbg.run_program(p, a).await,
            Resume::Continue => dbg.cont().await,
            Resume::Next => dbg.next().await,
            Resume::Step => dbg.step().await,
            Resume::Finish => dbg.finish().await,
            Resume::Pause => dbg.pause().await,
            Resume::Wait => dbg.wait().await,
            Resume::TestDebug(t, b) => dbg.test_debug(&t, b).await,
        };
        dbg.set_timeout(previous);
        let transcript = dbg.take_transcript();
        match result {
            Ok(h) => ok(Some(transcript), HaltDto::from(h)),
            Err(e) if e.downcast_ref::<Timeout>().is_some() => {
                let mut summary = transcript;
                if !summary.is_empty() {
                    summary.push('\n');
                }
                summary.push_str(&format!(
                    "The program is still running after {timeout:?}; \
                     call `wait` to keep waiting or `pause` to interrupt it."
                ));
                ok(
                    Some(summary),
                    HaltDto::Running {
                        timeout_ms: timeout.as_millis() as u64,
                    },
                )
            }
            Err(e) => fail(e),
        }
    }

    /// Send a command to the engine, render the reply like the REPL, and
    /// return it with the structured value built by `structured`.
    async fn command(&self, cmd: Command, structured: impl FnOnce(Reply) -> Value) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        match st.dbg.execute(cmd.clone()).await {
            Ok(reply) => {
                let summary = st.dbg.renderer_mut().reply(&cmd, &reply);
                ok(summary, structured(reply))
            }
            Err(e) => fail(e),
        }
    }
}

// --- Tools ------------------------------------------------------------------

#[tool_router]
impl DdbgServer {
    pub fn new() -> Self {
        Self {
            state: Default::default(),
            engine: Default::default(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Start a debug session, replacing any existing one. Detects the \
        project (Rust, C/C++, .NET, Python) and debug adapter like the `ddbg` CLI. The program \
        is not launched until `run` unless `stop_on_entry` is set."
    )]
    async fn start_session(&self, Parameters(p): Parameters<StartSessionParams>) -> ToolResult {
        if let Some(engine) = self.engine.lock().unwrap().take() {
            // Unblocks a resuming command that may hold the session.
            tokio::spawn(async move { engine.execute(Command::Quit).await });
        }
        let mut guard = self.state.lock().await;
        if let Some(old) = guard.take() {
            let _ = old.dbg.quit().await;
        }
        let cwd = match p.cwd {
            Some(c) => PathBuf::from(c),
            None => match std::env::current_dir() {
                Ok(c) => c,
                Err(e) => return fail(e.into()),
            },
        };
        let mut argv = vec!["ddbg".to_owned()];
        if let Some(a) = p.adapter {
            argv.extend(["--adapter".to_owned(), a]);
        }
        if p.stop_on_entry {
            argv.push("--stop-on-entry".into());
        }
        if p.no_detect {
            argv.push("--no-detect".into());
        }
        if let Some(program) = p.program {
            argv.push("--".into());
            argv.push(program);
            argv.extend(p.args);
        }
        let prepared = Args::try_parse_from(argv)
            .map_err(anyhow::Error::from)
            .and_then(|args| Ok((args, cwd.canonicalize()?)))
            .and_then(|(args, cwd)| ddbg_cli::prepare(&args, cwd));
        let prepared = match prepared {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let dbg = Debugger::from_session(prepared.session);
        for cmd in prepared.initial {
            let engine = dbg.engine().clone();
            tokio::spawn(async move { engine.execute(cmd).await });
        }
        let info = SessionInfo {
            cwd: dbg.cwd().display().to_string(),
            project: prepared.project.map(|k| format!("{k:?}")),
            program: prepared.program.map(|p| p.display().to_string()),
            candidates: dbg
                .session()
                .candidates
                .iter()
                .map(|c| c.display().to_string())
                .collect(),
            notes: prepared.notes,
        };
        let mut summary = info.notes.join("\n");
        if summary.is_empty() {
            summary = match &info.program {
                Some(p) => format!("Debugging {p}"),
                None => "No program selected; pass one to `run`".into(),
            };
        }
        *self.engine.lock().unwrap() = Some(dbg.engine().clone());
        *guard = Some(State {
            dbg,
            stdout_seen: 0,
            stderr_seen: 0,
        });
        ok(Some(summary), info)
    }

    #[tool(description = "End the debug session and shut down the debug adapter.")]
    async fn end_session(&self) -> ToolResult {
        let engine = self.engine.lock().unwrap().take();
        if let Ok(mut guard) = self.state.try_lock() {
            return match guard.take() {
                Some(st) => match st.dbg.quit().await {
                    Ok(()) => text("Session ended."),
                    Err(e) => fail(e),
                },
                None => no_session(),
            };
        }
        // A resuming command holds the session: shut the engine down so it
        // returns, then drop the session.
        let Some(engine) = engine else {
            return no_session();
        };
        let quit = engine.execute(Command::Quit).await;
        self.state.lock().await.take();
        match quit {
            Ok(_) => text("Session ended."),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Launch (or restart) the program and wait until it stops at a \
        breakpoint, exits, or the timeout passes."
    )]
    async fn run(&self, Parameters(p): Parameters<RunParams>) -> ToolResult {
        let target = p.program.map(|prog| (prog, p.args));
        self.resume(Resume::Run(target), p.timeout_ms).await
    }

    #[tool(
        name = "continue",
        description = "Resume the program and wait until it halts."
    )]
    async fn cont(&self, Parameters(p): Parameters<ResumeParams>) -> ToolResult {
        self.resume(Resume::Continue, p.timeout_ms).await
    }

    #[tool(description = "Step over the current line.")]
    async fn next(&self, Parameters(p): Parameters<ResumeParams>) -> ToolResult {
        self.resume(Resume::Next, p.timeout_ms).await
    }

    #[tool(description = "Step into the call on the current line.")]
    async fn step(&self, Parameters(p): Parameters<ResumeParams>) -> ToolResult {
        self.resume(Resume::Step, p.timeout_ms).await
    }

    #[tool(description = "Run until the current function returns.")]
    async fn finish(&self, Parameters(p): Parameters<ResumeParams>) -> ToolResult {
        self.resume(Resume::Finish, p.timeout_ms).await
    }

    #[tool(
        description = "Interrupt the running program. If another call is waiting for the \
        program, that call reports where it stopped."
    )]
    async fn pause(&self, Parameters(p): Parameters<ResumeParams>) -> ToolResult {
        if self.state.try_lock().is_err() {
            let Some(engine) = self.engine() else {
                return no_session();
            };
            return match engine.execute(Command::Pause).await {
                Ok(_) => {
                    text("Pause requested; the pending call reports where the program stopped.")
                }
                Err(e) => fail(e),
            };
        }
        self.resume(Resume::Pause, p.timeout_ms).await
    }

    #[tool(
        description = "Wait for a running program to stop, exit or terminate, e.g. after \
        a resuming command reported `running`."
    )]
    async fn wait(&self, Parameters(p): Parameters<ResumeParams>) -> ToolResult {
        self.resume(Resume::Wait, p.timeout_ms).await
    }

    #[tool(description = "Terminate the program. The session stays open for another `run`.")]
    async fn kill(&self) -> ToolResult {
        if self.state.try_lock().is_err() {
            let Some(engine) = self.engine() else {
                return no_session();
            };
            return match engine.execute(Command::Kill).await {
                Ok(_) => text("Killed; the pending call reports the exit."),
                Err(e) => fail(e),
            };
        }
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        match st.dbg.kill().await {
            Ok(()) => text("Killed."),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Set a breakpoint at `file`+`line`, or on `function` (optionally \
        scoped to `file`), with an optional adapter-native `condition`. Can be set before the program runs."
    )]
    async fn set_breakpoint(&self, Parameters(p): Parameters<SetBreakpointParams>) -> ToolResult {
        let location = match (p.file, p.line, p.function) {
            (Some(f), Some(l), None) => Location::Source(SourceLocation::new(f, l)),
            (file, None, Some(func)) => {
                Location::Function(FunctionLocation::new(func, file.map(PathBuf::from)))
            }
            _ => {
                return Err(ErrorData::invalid_params(
                    "give `file` and `line`, or `function` (optionally with `file`)",
                    None,
                ));
            }
        };
        let cmd = match p.condition {
            Some(condition) => Command::ConditionalBreak {
                location,
                condition,
            },
            None => Command::Break(location),
        };
        self.command(cmd, |r| match r {
            Reply::BreakpointSet { breakpoint, .. } => to_value(BreakpointDto::from(breakpoint)),
            other => unexpected(other),
        })
        .await
    }

    #[tool(description = "Delete a breakpoint by id.")]
    async fn delete_breakpoint(&self, Parameters(p): Parameters<IdParams>) -> ToolResult {
        let Ok(id) = u32::try_from(p.id) else {
            return Err(ErrorData::invalid_params("invalid breakpoint id", None));
        };
        self.command(
            Command::DeleteBreakpoint(BreakpointId(id)),
            |_| serde_json::json!({ "deleted": id }),
        )
        .await
    }

    #[tool(description = "Change a breakpoint's condition by id. Omit `condition` to clear it.")]
    async fn set_breakpoint_condition(
        &self,
        Parameters(p): Parameters<SetBreakpointConditionParams>,
    ) -> ToolResult {
        self.command(
            Command::Condition {
                id: BreakpointId(p.id),
                condition: p.condition,
            },
            |r| match r {
                Reply::BreakpointSet { breakpoint, .. } => {
                    to_value(BreakpointDto::from(breakpoint))
                }
                other => unexpected(other),
            },
        )
        .await
    }

    #[tool(description = "List breakpoints.")]
    async fn list_breakpoints(&self) -> ToolResult {
        self.command(Command::Breakpoints, |r| match r {
            Reply::Breakpoints(bps) => serde_json::json!({
                "breakpoints": bps.into_iter().map(BreakpointDto::from).collect::<Vec<_>>(),
            }),
            other => unexpected(other),
        })
        .await
    }

    #[tool(
        description = "Add a watch expression. Refreshes at each stop, frame/thread change and assignment. Does not stop execution on writes. Avoid expressions with side effects."
    )]
    async fn add_watch(&self, Parameters(p): Parameters<WatchParams>) -> ToolResult {
        self.command(Command::Watch(p.expression), |r| match r {
            Reply::WatchSet { watch, .. } => to_value(WatchDto::from(watch)),
            other => unexpected(other),
        })
        .await
    }

    #[tool(
        description = "List watch expressions and their current summaries or evaluation errors. Values are unavailable while the program is not stopped."
    )]
    async fn list_watches(&self) -> ToolResult {
        self.command(Command::Watches, |r| match r {
            Reply::Watches(watches) => serde_json::json!({ "watches": watches.into_iter().map(WatchDto::from).collect::<Vec<_>>() }),
            other => unexpected(other),
        }).await
    }

    #[tool(
        description = "Remove a watch expression by id. Watch ids are separate from breakpoint ids."
    )]
    async fn remove_watch(&self, Parameters(p): Parameters<IdParams>) -> ToolResult {
        let Ok(id) = u32::try_from(p.id) else {
            return Err(ErrorData::invalid_params("invalid watch id", None));
        };
        self.command(
            Command::Unwatch(ddbg_core::watch::WatchId(id)),
            |_| serde_json::json!({ "deleted": id }),
        )
        .await
    }

    #[tool(
        description = "Show the call stack of the selected thread. The program must be stopped. \
        Frames without source (framework/library internals) are hidden unless `all` is set; \
        frame indexes stay valid for `select_frame`."
    )]
    async fn backtrace(&self, Parameters(p): Parameters<BacktraceParams>) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        let cmd = Command::Backtrace;
        let reply = match st.dbg.execute(cmd.clone()).await {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let Reply::Backtrace { frames, selected } = reply else {
            return ok(None, unexpected(reply));
        };
        if p.all {
            let summary = st.dbg.renderer_mut().reply(
                &cmd,
                &Reply::Backtrace {
                    frames: frames.clone(),
                    selected,
                },
            );
            let frames: Vec<_> = frames
                .into_iter()
                .enumerate()
                .map(|(i, f)| FrameDto::new(Some(i), f))
                .collect();
            return ok(
                summary,
                serde_json::json!({ "selected": selected, "frames": frames }),
            );
        }
        let total = frames.len();
        let mut summary = String::new();
        let mut shown = Vec::new();
        let mut hidden_run = 0;
        for (i, f) in frames.into_iter().enumerate() {
            if !f.path.as_deref().is_some_and(std::path::Path::exists) && Some(i) != selected {
                hidden_run += 1;
                continue;
            }
            if hidden_run > 0 {
                summary.push_str(&format!(
                    "   ... {hidden_run} frames without local source\n"
                ));
                hidden_run = 0;
            }
            let marker = if Some(i) == selected { '*' } else { ' ' };
            let location = match &f.path {
                Some(path) => format!(" at {}:{}", path.display(), f.line),
                None => String::new(),
            };
            summary.push_str(&format!("{marker}#{i} {}{location}\n", f.name));
            shown.push(FrameDto::new(Some(i), f));
        }
        if hidden_run > 0 {
            summary.push_str(&format!(
                "   ... {hidden_run} frames without local source\n"
            ));
        }
        let hidden = total - shown.len();
        if hidden > 0 {
            summary.push_str(&format!(
                "{hidden} of {total} frames without local source hidden; pass `all: true` to show them."
            ));
        }
        ok(
            Some(summary),
            serde_json::json!({ "selected": selected, "frames": shown, "hidden": hidden }),
        )
    }

    #[tool(description = "List threads.")]
    async fn threads(&self) -> ToolResult {
        self.command(Command::Threads, |r| match r {
            Reply::Threads { threads, selected } => serde_json::json!({
                "selected": selected.map(|t| t.0),
                "threads": threads.into_iter().map(ThreadDto::from).collect::<Vec<_>>(),
            }),
            other => unexpected(other),
        })
        .await
    }

    #[tool(description = "Select a thread by id; later inspection commands use it.")]
    async fn select_thread(&self, Parameters(p): Parameters<IdParams>) -> ToolResult {
        self.command(Command::Thread(ThreadId(p.id)), |r| match r {
            Reply::Frame { index, frame } => to_value(FrameDto::new(Some(index), frame)),
            _ => serde_json::json!({ "thread": p.id }),
        })
        .await
    }

    #[tool(
        description = "Select a stack frame by index, or move `up`/`down`; later \
        evaluation uses it. Without `frame`, shows the current frame."
    )]
    async fn select_frame(&self, Parameters(p): Parameters<FrameParams>) -> ToolResult {
        let selector = match p.frame.as_deref().map(str::trim) {
            None | Some("") => FrameSelector::Current,
            Some("up") => FrameSelector::Up,
            Some("down") => FrameSelector::Down,
            Some(n) => match n.parse() {
                Ok(i) => FrameSelector::Index(i),
                Err(_) => {
                    return Err(ErrorData::invalid_params(
                        "`frame` must be an index, `up` or `down`",
                        None,
                    ));
                }
            },
        };
        self.command(Command::Frame(selector), |r| match r {
            Reply::Frame { index, frame } => to_value(FrameDto::new(Some(index), frame)),
            other => unexpected(other),
        })
        .await
    }

    #[tool(
        description = "Evaluate an expression in the selected frame and return its value \
        and one level of children."
    )]
    async fn evaluate(&self, Parameters(p): Parameters<EvaluateParams>) -> ToolResult {
        let cmd = if p.repl {
            Command::Eval(p.expression)
        } else {
            Command::Print(p.expression)
        };
        self.command(cmd, |r| match r {
            Reply::Value(v, children) => to_value(EvaluationDto::new(v, children)),
            other => unexpected(other),
        })
        .await
    }

    #[tool(description = "Assign a new value to a variable or other l-value expression.")]
    async fn set_value(&self, Parameters(p): Parameters<SetValueParams>) -> ToolResult {
        let cmd = Command::Set {
            target: p.target,
            value: p.value,
        };
        self.command(cmd, |r| match r {
            Reply::Value(v, _) => to_value(EvaluationDto::new(v, Vec::new())),
            other => unexpected(other),
        })
        .await
    }

    #[tool(description = "Show variables of the selected frame, grouped by scope.")]
    async fn locals(&self) -> ToolResult {
        self.command(Command::Locals, |r| match r {
            Reply::Locals(s) => serde_json::json!({
                "scopes": s.into_iter().map(ScopeDto::from).collect::<Vec<_>>(),
            }),
            other => unexpected(other),
        })
        .await
    }

    #[tool(description = "Discover tests in the project (Rust and .NET).")]
    async fn list_tests(&self, Parameters(p): Parameters<ListTestsParams>) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        match st.dbg.tests(p.filter.as_deref()).await {
            Ok(tests) => ok(
                Some(render_list(&tests)),
                serde_json::json!({
                    "tests": tests
                        .into_iter()
                        .enumerate()
                        .map(|(i, t)| TestCaseDto::new(i + 1, t))
                        .collect::<Vec<_>>(),
                }),
            ),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Run a test without the debugger and return its result and output.")]
    async fn run_test(&self, Parameters(p): Parameters<TestParams>) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        match st.dbg.test_run(&p.test).await {
            Ok(t) => text(t),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Debug a test and wait until it stops, exits or the timeout passes.")]
    async fn debug_test(&self, Parameters(p): Parameters<DebugTestParams>) -> ToolResult {
        self.resume(Resume::TestDebug(p.test, p.break_at_start), p.timeout_ms)
            .await
    }

    #[tool(description = "Return program stdout and stderr received since the previous call.")]
    async fn get_output(&self) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        let stdout = st.dbg.stdout()[st.stdout_seen..].to_owned();
        st.stdout_seen += stdout.len();
        let stderr = st.dbg.stderr()[st.stderr_seen..].to_owned();
        st.stderr_seen += stderr.len();
        ok(
            None,
            serde_json::json!({ "stdout": stdout, "stderr": stderr }),
        )
    }

    #[tool(
        description = "Run a raw ddbg REPL command and return its text output. Use for \
        anything the other tools do not cover; `help` lists commands."
    )]
    async fn repl(&self, Parameters(p): Parameters<ReplParams>) -> ToolResult {
        let mut guard = self.state.lock().await;
        let Some(st) = guard.as_mut() else {
            return no_session();
        };
        match st.dbg.exec(&p.line).await {
            Ok(t) => text(t),
            Err(e) => fail(e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DdbgServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("ddbg", env!("CARGO_PKG_VERSION"))
                    .with_title("ddbg debugger")
                    .with_website_url(env!("CARGO_PKG_HOMEPAGE")),
            )
            .with_instructions(
                "ddbg: a language-agnostic debugger (Rust, C/C++, .NET, Python). Call \
             `start_session` with the project directory, set breakpoints, then `run`. \
             Resuming tools return the program state: stopped (with frame), exited, \
             terminated, or running if the timeout passed. While a call waits, `pause`, \
             `kill` and `end_session` act immediately.\n\
             Tools: start_session, end_session, run, continue, next, step, finish, pause, \
              wait, kill, set_breakpoint, set_breakpoint_condition, delete_breakpoint, list_breakpoints, backtrace, \
              threads, select_thread, select_frame, evaluate, set_value, locals, add_watch, list_watches, remove_watch, list_tests, \
             run_test, debug_test, get_output, repl.\n\
             A failed call (e.g. an expression that cannot be evaluated) returns an error \
             result; when scripting several calls, handle each failure separately.",
            )
    }
}
