//! The debug engine: single owner of [`DebugSession`].
//!
//! Frontends send [`Command`]s through an [`EngineHandle`] and observe
//! [`DebugEvent`]s. The engine task multiplexes commands and adapter
//! messages, so all state transitions happen in one place.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ddbg_dap::client::channel_closed;
use ddbg_dap::protocol::{
    self as dap, AttachArguments, CompletionsArguments, ConfigurationDoneArguments,
    ContinueArguments, DapEvent, DisconnectArguments, EvaluateArguments, ExceptionInfoArguments,
    InitializeArguments, LaunchArguments, NextArguments, PauseArguments, ScopesArguments,
    SetExpressionArguments, SetVariableArguments, StackTraceArguments, StepInArguments,
    StepOutArguments, ThreadsArguments, VariablesArguments,
};
use ddbg_dap::{AdapterProcess, DapClient, Incoming};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::adapter::DebugAdapter;
use crate::breakpoint::{Breakpoint, BreakpointId};
use crate::command::{Command, Completion, FrameSelector, Location, Reply, ScopeVariables};
use anyhow::{Result, anyhow, bail};

use crate::error::{
    NO_CONDITIONAL_BREAKPOINTS, NO_FUNCTION_BREAKPOINTS, NO_TARGET, NO_THREAD, NOT_RUNNING,
    NOT_STOPPED,
};
use crate::event::{DebugEvent, ExceptionInfo, Output, StopInfo};
use crate::session::{DebugSession, Feature, SessionStatus, StopReason};
use crate::target::{AttachTarget, DebugTarget, LaunchTarget};
use crate::thread::{Thread, ThreadId};
use crate::variable::{Evaluation, VarRef, Variable};
use crate::watch::{WatchId, WatchValue};

const MAX_CHILDREN: usize = 100;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// A live adapter connection.
pub struct Connection {
    pub client: DapClient,
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
    /// Adapter process, if we spawned one.
    pub child: Option<Child>,
}

/// Creates adapter connections. Replaceable for tests.
pub type Connector = Box<dyn Fn(&dyn DebugAdapter) -> Result<Connection> + Send + Sync>;

fn spawn_connector() -> Connector {
    Box::new(|adapter| {
        let proc = AdapterProcess::spawn(&adapter.command()?)?;
        Ok(Connection {
            client: proc.client,
            incoming: proc.incoming,
            child: Some(proc.child),
        })
    })
}

pub struct EngineConfig {
    pub adapter: Arc<dyn DebugAdapter>,
    /// Base directory for relative breakpoint paths.
    pub cwd: PathBuf,
    /// Initial target for `run` without arguments.
    pub target: Option<LaunchTarget>,
}

type CommandMsg = (Command, oneshot::Sender<Result<Reply>>);

/// Cheap, cloneable handle used by frontends.
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: mpsc::Sender<CommandMsg>,
    events: broadcast::Sender<DebugEvent>,
    halt: watch::Receiver<Option<DebugEvent>>,
}

impl EngineHandle {
    /// Execute a command and wait for its result.
    pub async fn execute(&self, command: Command) -> Result<Reply> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send((command, tx))
            .await
            .map_err(|_| channel_closed())?;
        rx.await.map_err(|_| channel_closed())?
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DebugEvent> {
        self.events.subscribe()
    }

    /// Persistent halt state, independent of consumption of the event stream.
    /// `None` means no completed halt; launch/resume clears the previous halt.
    pub fn subscribe_halt(&self) -> watch::Receiver<Option<DebugEvent>> {
        self.halt.clone()
    }
}

/// Start the engine task.
pub fn spawn(config: EngineConfig) -> EngineHandle {
    spawn_with_connector(config, spawn_connector())
}

/// Start the engine task with a custom connector (used by tests).
pub fn spawn_with_connector(config: EngineConfig, connector: Connector) -> EngineHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (events, _) = broadcast::channel(1024);
    let (halt_tx, halt) = watch::channel(None);
    let engine = Engine {
        session: DebugSession {
            target: config.target.map(DebugTarget::Launch),
            ..DebugSession::default()
        },
        adapter: config.adapter,
        connector,
        conn: None,
        incoming: None,
        cwd: config.cwd,
        events: events.clone(),
        resumed_at: None,
        halt: halt_tx,
        pause_pending: false,
    };
    tokio::spawn(engine.run(cmd_rx));
    EngineHandle {
        cmd_tx,
        events,
        halt,
    }
}

struct ActiveConnection {
    client: DapClient,
    child: Option<Child>,
    /// Cleanup policy belongs to this connection, not the next desired target.
    attached: bool,
}

struct Engine {
    session: DebugSession,
    adapter: Arc<dyn DebugAdapter>,
    connector: Connector,
    conn: Option<ActiveConnection>,
    incoming: Option<mpsc::UnboundedReceiver<Incoming>>,
    cwd: PathBuf,
    events: broadcast::Sender<DebugEvent>,
    /// When the debuggee last resumed, for timing the next stop.
    resumed_at: Option<Instant>,
    halt: watch::Sender<Option<DebugEvent>>,
    /// A pause requested before the adapter exposes its first live thread.
    pause_pending: bool,
}

async fn recv_incoming(rx: &mut Option<mpsc::UnboundedReceiver<Incoming>>) -> Option<Incoming> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl Engine {
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<CommandMsg>) {
        let mut pause_retry = tokio::time::interval(Duration::from_millis(50));
        pause_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some((cmd, reply_tx)) = cmd else { break };
                    let quit = matches!(cmd, Command::Quit);
                    let result = self.execute(cmd).await;
                    let _ = reply_tx.send(result);
                    if quit {
                        break;
                    }
                }
                msg = recv_incoming(&mut self.incoming) => {
                    self.handle_incoming(msg).await;
                }
                _ = pause_retry.tick(), if self.pause_pending => {
                    if let Err(e) = self.try_pause().await {
                        tracing::warn!("could not pause during startup: {e}");
                    }
                }
            }
        }
        self.shutdown().await;
    }

    fn emit(&self, event: DebugEvent) {
        match &event {
            DebugEvent::SessionContinued => {
                self.halt.send_replace(None);
            }
            DebugEvent::SessionStopped(_) | DebugEvent::SessionExited(_) => {
                self.halt.send_replace(Some(event.clone()));
            }
            DebugEvent::SessionTerminated if self.session.exit_code.is_none() => {
                // Preserve an adapter-reported exit code across disconnect.
                self.halt.send_replace(Some(event.clone()));
            }
            _ => {}
        }
        let _ = self.events.send(event);
    }

    fn client(&self) -> Result<DapClient> {
        self.conn
            .as_ref()
            .map(|c| c.client.clone())
            .ok_or_else(|| anyhow!(NOT_RUNNING))
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    async fn execute(&mut self, cmd: Command) -> Result<Reply> {
        match cmd {
            Command::Run(target) => self.cmd_run(target).await,
            Command::Attach(target) => self.cmd_attach(target).await,
            Command::Detach => {
                if !self.session.status.is_active() {
                    bail!(NOT_RUNNING);
                }
                if !self.conn.as_ref().is_some_and(|c| c.attached)
                    && !self.session.supports(Feature::TerminateDebuggee)
                {
                    bail!("the debug adapter does not support detaching from a launched process");
                }
                self.disconnect(false).await?;
                Ok(Reply::Ok)
            }
            Command::Continue => {
                let tid = self.require_stopped()?;
                self.resumed_at = Some(Instant::now());
                let client = self.client()?;
                client
                    .request(ContinueArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(true);
                Ok(Reply::Ok)
            }
            Command::Pause => {
                if !matches!(self.session.status, SessionStatus::Running) {
                    bail!(NOT_RUNNING);
                }
                self.pause_pending = true;
                if let Err(e) = self.try_pause().await {
                    self.pause_pending = false;
                    return Err(e);
                }
                Ok(Reply::Ok)
            }
            Command::Kill => {
                if !self.session.status.is_active() {
                    bail!(NOT_RUNNING);
                }
                if self.conn.as_ref().is_some_and(|c| c.attached)
                    && !self.session.supports(Feature::TerminateDebuggee)
                {
                    bail!(
                        "the debug adapter does not support terminating an attached process; use `detach`"
                    );
                }
                self.disconnect(true).await?;
                Ok(Reply::Ok)
            }
            Command::Next => {
                let tid = self.require_stopped()?;
                self.resumed_at = Some(Instant::now());
                self.client()?
                    .request(NextArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(false);
                Ok(Reply::Ok)
            }
            Command::Step => {
                let tid = self.require_stopped()?;
                self.resumed_at = Some(Instant::now());
                self.client()?
                    .request(StepInArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(false);
                Ok(Reply::Ok)
            }
            Command::Finish => {
                let tid = self.require_stopped()?;
                self.resumed_at = Some(Instant::now());
                self.client()?
                    .request(StepOutArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(false);
                Ok(Reply::Ok)
            }
            Command::Break(loc) => self.cmd_break(loc, None).await,
            Command::ConditionalBreak {
                location,
                condition,
            } => self.cmd_break(location, Some(condition)).await,
            Command::Condition { id, condition } => self.cmd_condition(id, condition).await,
            Command::DeleteBreakpoint(id) => {
                let bp = self
                    .session
                    .breakpoints
                    .remove(id)
                    .ok_or_else(|| anyhow!("no breakpoint {id}"))?;
                if self.can_set_breakpoints() {
                    self.sync_for(&bp).await?;
                }
                Ok(Reply::BreakpointDeleted(id))
            }
            Command::Breakpoints => Ok(Reply::Breakpoints(
                self.session.breakpoints.iter().cloned().collect(),
            )),
            Command::Backtrace => {
                let tid = self.require_stopped()?;
                if self.session.stack.is_empty() {
                    self.load_stack(tid).await?;
                }
                Ok(Reply::Backtrace {
                    frames: self.session.stack.clone(),
                    selected: self.session.selected_frame,
                })
            }
            Command::Threads => {
                if !self.session.status.is_active() {
                    bail!(NOT_RUNNING);
                }
                self.refresh_threads().await?;
                Ok(Reply::Threads {
                    threads: self.session.threads.clone(),
                    selected: self.session.selected_thread,
                })
            }
            Command::Thread(tid) => {
                self.require_stopped()?;
                if !self.session.threads.iter().any(|t| t.id == tid) {
                    self.refresh_threads().await?;
                }
                if !self.session.threads.iter().any(|t| t.id == tid) {
                    bail!("no thread {tid}");
                }
                self.session.selected_thread = Some(tid);
                self.load_stack(tid).await?;
                self.emit(DebugEvent::FrameChanged);
                self.refresh_watches().await;
                self.frame_reply(0)
            }
            Command::Frame(sel) => {
                let tid = self.require_stopped()?;
                if self.session.stack.is_empty() {
                    self.load_stack(tid).await?;
                }
                let current = self.session.selected_frame.unwrap_or(0);
                let index = match sel {
                    FrameSelector::Current => current,
                    FrameSelector::Index(i) => i,
                    FrameSelector::Up => current + 1,
                    FrameSelector::Down => current
                        .checked_sub(1)
                        .ok_or_else(|| anyhow!("no frame {}", 0))?,
                };
                if index >= self.session.stack.len() {
                    bail!("no frame {index}");
                }
                if index != current || self.session.selected_frame.is_none() {
                    self.select_frame(index).await?;
                    self.emit(DebugEvent::FrameChanged);
                    self.refresh_watches().await;
                }
                self.frame_reply(index)
            }
            Command::Print(expr) => self.cmd_print(expr, "watch").await,
            Command::Watch(expression) => {
                let expression = expression.trim().to_owned();
                if expression.is_empty() {
                    bail!("watch expression must not be empty");
                }
                let (id, new) = self.session.watches.add(expression);
                if new && self.session.status.is_stopped() {
                    self.evaluate_watch(id).await;
                }
                Ok(Reply::WatchSet {
                    watch: self.session.watches.get(id).unwrap().clone(),
                    new,
                })
            }
            Command::Unwatch(id) => {
                self.session
                    .watches
                    .remove(id)
                    .ok_or_else(|| anyhow!("no watch {id}"))?;
                Ok(Reply::WatchDeleted(id))
            }
            Command::Watches => Ok(Reply::Watches(self.session.watches.snapshot())),
            Command::Eval(expr) => self.cmd_print(expr, "repl").await,
            Command::Set { target, value } => self.cmd_set(target, value).await,
            Command::SetVariable { scope, name, value } => {
                self.require_stopped()?;
                let eval = self.set_variable(scope, name, value).await?;
                self.value_reply(eval).await
            }
            Command::Complete { text, column } => self.cmd_complete(text, column).await,
            Command::Locals => self.cmd_locals().await,
            Command::Tests(_) => Err(anyhow!("test discovery is not implemented yet")),
            Command::TestRun(_) => Err(anyhow!("test-run is not implemented yet")),
            Command::TestDebug { .. } => Err(anyhow!("test-debug is not implemented yet")),
            Command::Quit => {
                self.shutdown().await;
                Ok(Reply::Quit)
            }
        }
    }

    fn require_stopped(&self) -> Result<ThreadId> {
        match self.session.status {
            SessionStatus::Stopped(_) => self
                .session
                .selected_thread
                .ok_or_else(|| anyhow!(NO_THREAD)),
            SessionStatus::Running => Err(anyhow!(NOT_STOPPED)),
            _ => Err(anyhow!(NOT_RUNNING)),
        }
    }

    fn can_set_breakpoints(&self) -> bool {
        self.conn.is_some()
            && matches!(
                self.session.status,
                SessionStatus::Configuring | SessionStatus::Running | SessionStatus::Stopped(_)
            )
    }

    fn check_condition(&self, condition: Option<&str>) -> Result<()> {
        if let Some(condition) = condition {
            if condition.trim().is_empty() {
                bail!("breakpoint condition must not be empty; omit it to clear the condition");
            }
            if self.conn.is_some() && !self.session.supports(Feature::ConditionalBreakpoints) {
                bail!(NO_CONDITIONAL_BREAKPOINTS);
            }
        }
        Ok(())
    }

    async fn cmd_break(&mut self, location: Location, condition: Option<String>) -> Result<Reply> {
        self.check_condition(condition.as_deref())?;
        if matches!(location, Location::Function(_))
            && self.conn.is_some()
            && !self.session.supports(Feature::FunctionBreakpoints)
        {
            bail!(NO_FUNCTION_BREAKPOINTS);
        }
        let before = self.session.breakpoints.clone();
        let (id, new) = self.session.breakpoints.add(location, &self.cwd);
        if !new && let Some(condition) = &condition {
            let bp = self.session.breakpoints.get(id).unwrap();
            if bp.condition.as_ref() != Some(condition) {
                bail!(
                    "breakpoint {id} already exists; use `condition {id} <expression>` to change it"
                );
            }
        }
        if new {
            self.session.breakpoints.set_condition(id, condition);
            if self.can_set_breakpoints() {
                let bp = self.session.breakpoints.get(id).unwrap().clone();
                if let Err(e) = self.sync_for(&bp).await {
                    self.session.breakpoints = before;
                    return Err(e);
                }
            }
        }
        let breakpoint = self.session.breakpoints.get(id).unwrap().clone();
        Ok(Reply::BreakpointSet { breakpoint, new })
    }

    async fn cmd_condition(
        &mut self,
        id: BreakpointId,
        condition: Option<String>,
    ) -> Result<Reply> {
        let bp = self
            .session
            .breakpoints
            .get(id)
            .ok_or_else(|| anyhow!("no breakpoint {id}"))?
            .clone();
        self.check_condition(condition.as_deref())?;
        if bp.condition != condition {
            let before = self.session.breakpoints.clone();
            self.session.breakpoints.set_condition(id, condition);
            if self.can_set_breakpoints()
                && let Err(e) = self.sync_for(&bp).await
            {
                self.session.breakpoints = before;
                return Err(e);
            }
        }
        let breakpoint = self.session.breakpoints.get(id).unwrap().clone();
        Ok(Reply::BreakpointSet {
            breakpoint,
            new: false,
        })
    }

    fn frame_reply(&self, index: usize) -> Result<Reply> {
        let frame = self
            .session
            .stack
            .get(index)
            .cloned()
            .ok_or_else(|| anyhow!("no frame {}", index))?;
        Ok(Reply::Frame { index, frame })
    }

    /// Execution resumed: no frame/variable reference survives this.
    fn mark_resumed(&mut self, announce: bool) {
        self.halt.send_replace(None);
        self.session.on_resume();
        let was_stopped = self.session.status.is_stopped();
        self.session.status = SessionStatus::Running;
        let watches = self.session.watches.snapshot();
        if !watches.is_empty() {
            self.emit(DebugEvent::WatchesChanged(watches));
        }
        if announce && was_stopped {
            self.emit(DebugEvent::SessionContinued);
        }
    }

    async fn cmd_run(&mut self, target: Option<LaunchTarget>) -> Result<Reply> {
        if let Some(t) = target {
            self.session.target = Some(DebugTarget::Launch(t));
        }
        let target = match &self.session.target {
            Some(DebugTarget::Launch(t)) => t.clone(),
            Some(DebugTarget::Attach(_)) => {
                bail!("the current target is attached; use `attach <pid>` or `run <program>`")
            }
            None => bail!(NO_TARGET),
        };
        if self.conn.is_some() {
            self.shutdown().await;
        }
        match self
            .start_target(&DebugTarget::Launch(target.clone()))
            .await
        {
            Ok(()) => Ok(Reply::Launched(target.absolute_program())),
            Err(e) => {
                self.shutdown().await;
                Err(e)
            }
        }
    }

    async fn cmd_attach(&mut self, target: AttachTarget) -> Result<Reply> {
        if target.pid == 0 {
            bail!("process ID must be greater than zero");
        }
        // Validate arguments before replacing a live session.
        self.adapter.build_attach_request(&target)?;
        self.shutdown().await;
        self.session.target = Some(DebugTarget::Attach(target.clone()));
        match self
            .start_target(&DebugTarget::Attach(target.clone()))
            .await
        {
            Ok(()) => Ok(Reply::Attached(target.pid)),
            Err(e) => {
                self.shutdown().await;
                Err(e)
            }
        }
    }

    async fn start_target(&mut self, target: &DebugTarget) -> Result<()> {
        tokio::time::timeout(STARTUP_TIMEOUT, self.start(target))
            .await
            .map_err(|_| anyhow!("debug adapter startup timed out after {STARTUP_TIMEOUT:?}"))?
    }

    /// Shared DAP lifecycle: initialize → launch/attach → `initialized`
    /// → breakpoints → configurationDone → launch/attach response.
    async fn start(&mut self, target: &DebugTarget) -> Result<()> {
        let (args, attached) = match target {
            DebugTarget::Launch(t) => (
                self.adapter.build_launch_request(&with_color_env(t))?,
                false,
            ),
            DebugTarget::Attach(t) => (self.adapter.build_attach_request(t)?, true),
        };
        let conn = (self.connector)(self.adapter.as_ref())?;
        let client = conn.client.clone();
        let mut incoming = conn.incoming;
        self.conn = Some(ActiveConnection {
            client: client.clone(),
            child: conn.child,
            attached,
        });
        self.session.status = SessionStatus::Initializing;
        self.session.exit_code = None;
        self.halt.send_replace(None);

        let caps = client
            .request(InitializeArguments::new(self.adapter.id()))
            .await?;
        self.session.capabilities = caps;

        // Conditions can be configured before capabilities are known. Refuse
        // startup rather than silently installing unconditional breakpoints.
        if self
            .session
            .breakpoints
            .iter()
            .any(|bp| bp.condition.is_some())
            && !self.session.supports(Feature::ConditionalBreakpoints)
        {
            bail!(NO_CONDITIONAL_BREAKPOINTS);
        }

        let mut start_response = if attached {
            client.send(AttachArguments(args))?
        } else {
            client.send(LaunchArguments(args))?
        };
        let mut started = false;
        loop {
            tokio::select! {
                r = &mut start_response, if !started => {
                    r?;
                    started = true;
                }
                msg = incoming.recv() => match msg {
                    Some(Incoming::Event(DapEvent::Initialized)) => break,
                    None => return Err(channel_closed()),
                    other => self.handle_incoming(other).await,
                }
            }
        }

        if matches!(self.session.status, SessionStatus::Initializing) {
            self.session.status = SessionStatus::Configuring;
        }
        for file in self.session.breakpoints.files() {
            self.sync_breakpoints(&file).await?;
        }
        if self.session.breakpoints.has_functions() {
            self.sync_function_breakpoints().await?;
        }
        self.resumed_at = Some(Instant::now());
        if self.session.supports(Feature::ConfigurationDone) {
            client.request(ConfigurationDoneArguments {}).await?;
        }
        if !started {
            start_response.await?;
        }
        self.incoming = Some(incoming);
        // A `stopped` event (e.g. stop-on-entry) may already have arrived.
        if matches!(self.session.status, SessionStatus::Configuring) {
            self.session.status = SessionStatus::Running;
        }
        self.emit(DebugEvent::SessionStarted);
        Ok(())
    }

    async fn sync_breakpoints(&mut self, path: &Path) -> Result<()> {
        let args = self.session.breakpoints.request_for(path);
        let resp = self.client()?.request(args).await?;
        self.session
            .breakpoints
            .apply_results(path, &resp.breakpoints);
        Ok(())
    }

    /// Re-send whichever breakpoint set `bp` belongs to.
    async fn sync_for(&mut self, bp: &Breakpoint) -> Result<()> {
        match &bp.requested {
            Location::Source(s) => self.sync_breakpoints(&s.path).await,
            Location::Function(_) => self.sync_function_breakpoints().await,
        }
    }

    async fn sync_function_breakpoints(&mut self) -> Result<()> {
        if !self.session.supports(Feature::FunctionBreakpoints) {
            self.session
                .breakpoints
                .reject_functions(NO_FUNCTION_BREAKPOINTS);
            return Ok(());
        }
        let args = self.session.breakpoints.function_request();
        let resp = self.client()?.request(args).await?;
        self.session
            .breakpoints
            .apply_function_results(&resp.breakpoints);
        Ok(())
    }

    async fn cmd_print(&mut self, expression: String, context: &str) -> Result<Reply> {
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(EvaluateArguments {
                expression: expression.clone(),
                frame_id,
                context: Some(context.into()),
            })
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if let Some(eval) = self.array_length(&expression, frame_id).await {
                    return self.value_reply(eval).await;
                }
                let null = if context == "watch" {
                    self.null_prefix(&expression, frame_id).await
                } else {
                    None
                };
                return Err(match null {
                    Some(p) => anyhow!(
                        "cannot evaluate `{expression}`: `{p}` is null ({})",
                        e.to_string().trim()
                    ),
                    None => evaluate_error(&expression, e),
                });
            }
        };
        let eval = Evaluation::from(resp);
        if context == "repl" {
            // REPL evaluation may have side effects, so it is not repeated
            // to describe the value.
            self.invalidate_variables().await;
            return self.value_reply(eval).await;
        }
        self.eval_reply(&expression, eval).await
    }

    /// The longest member-access prefix of a failed expression that
    /// evaluates to null, e.g. `a.b` for `a.b.c` when `a.b` is null.
    /// Adapters often report such failures as a missing name instead.
    async fn null_prefix(&self, expression: &str, frame_id: Option<i64>) -> Option<String> {
        let client = self.client().ok()?;
        for prefix in member_prefixes(expression).into_iter().rev() {
            let Ok(resp) = client
                .request(EvaluateArguments {
                    expression: prefix.to_owned(),
                    frame_id,
                    context: Some("watch".into()),
                })
                .await
            else {
                continue;
            };
            return matches!(resp.result.trim(), "null" | "None" | "nullptr" | "NULL")
                .then(|| prefix.to_owned());
        }
        None
    }

    /// `arr.Length` (or `LongLength`/`Count`) computed from the array's
    /// value, e.g. `{int[3]}`. netcoredbg cannot evaluate members of arrays.
    async fn array_length(&self, expression: &str, frame_id: Option<i64>) -> Option<Evaluation> {
        let (receiver, member) = expression.trim().rsplit_once('.')?;
        let type_name = match member.trim() {
            "Length" | "Count" => "int",
            "LongLength" => "long",
            _ => return None,
        };
        let receiver = receiver.trim();
        if receiver.is_empty() {
            return None;
        }
        let resp = self
            .client()
            .ok()?
            .request(EvaluateArguments {
                expression: receiver.to_owned(),
                frame_id,
                context: Some("watch".into()),
            })
            .await
            .ok()?;
        let len = array_length_of(&resp.result)?;
        Some(Evaluation {
            value: len.to_string(),
            type_name: Some(type_name.into()),
            children: None,
        })
    }

    /// Build a value reply, replacing an opaque top-level value with its
    /// `ToString()` like `describe` does for children.
    async fn eval_reply(&mut self, expression: &str, mut eval: Evaluation) -> Result<Reply> {
        let mut top = [Variable {
            name: expression.to_owned(),
            value: eval.value.clone(),
            type_name: eval.type_name.clone(),
            children: eval.children,
            evaluate_name: Some(expression.to_owned()),
        }];
        self.describe(&mut top).await;
        eval.value = std::mem::take(&mut top[0].value);
        self.value_reply(eval).await
    }

    async fn value_reply(&mut self, eval: Evaluation) -> Result<Reply> {
        let mut children = match eval.children {
            Some(r) => self.variables(r).await?,
            None => Vec::new(),
        };
        self.describe(&mut children).await;
        Ok(Reply::Value(eval, children))
    }

    /// Replace values that only name their type (`{System.Guid}`) with
    /// the result of `ToString()`, when the adapter can evaluate it.
    async fn describe(&mut self, vars: &mut [Variable]) {
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        for v in vars.iter_mut().filter(|v| v.is_opaque()) {
            let Some(expr) = &v.evaluate_name else {
                continue;
            };
            let Ok(client) = self.client() else {
                return;
            };
            let Ok(resp) = client
                .request(EvaluateArguments {
                    expression: format!("({expr}).ToString()"),
                    frame_id,
                    context: Some("watch".into()),
                })
                .await
            else {
                continue;
            };
            let s = resp.result.trim();
            let s = s
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(s);
            if !s.is_empty() && Some(s) != v.type_name.as_deref() && s != v.value.trim() {
                v.value = s.to_owned();
            }
        }
    }

    async fn invalidate_variables(&mut self) {
        self.session.variable_cache.clear();
        self.emit(DebugEvent::VariablesChanged);
        self.refresh_watches().await;
    }

    /// Assign to an l-value: `setExpression` when supported, otherwise
    /// `setVariable` on the variable's container.
    async fn cmd_set(&mut self, target: String, value: String) -> Result<Reply> {
        self.require_stopped()?;
        let target = target.trim().to_owned();
        if self.session.supports(Feature::SetExpression) {
            let eval = self.set_expression(target, value).await?;
            return self.value_reply(eval).await;
        }
        if !self.session.supports(Feature::SetVariable) {
            bail!("the debug adapter does not support assigning values");
        }
        let (container, name) = self.resolve_lvalue(&target).await?;
        let eval = self.set_variable(container, name, value).await?;
        self.value_reply(eval).await
    }

    async fn set_expression(&mut self, expression: String, value: String) -> Result<Evaluation> {
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(SetExpressionArguments {
                expression,
                value,
                frame_id,
            })
            .await?;
        self.invalidate_variables().await;
        Ok(Evaluation {
            value: resp.value,
            type_name: resp.type_.filter(|t| !t.is_empty()),
            children: VarRef::from_raw(resp.variables_reference),
        })
    }

    /// Find the container reference and child name for `target`.
    async fn resolve_lvalue(&mut self, target: &str) -> Result<(VarRef, String)> {
        match split_lvalue(target) {
            None => {
                if self.session.scopes.is_empty()
                    && let Some(i) = self.session.selected_frame
                {
                    self.select_frame(i).await?;
                }
                let scopes = self.session.scopes.clone();
                for scope in scopes.iter().filter(|s| !s.expensive) {
                    if self
                        .variables(scope.reference)
                        .await?
                        .iter()
                        .any(|v| v.name == target)
                    {
                        return Ok((scope.reference, target.to_owned()));
                    }
                }
                bail!("no variable named `{target}` in the current frame")
            }
            Some((parent, child)) => {
                let frame_id = self.session.current_frame().map(|f| f.id.0);
                let resp = self
                    .client()?
                    .request(EvaluateArguments {
                        expression: parent.to_owned(),
                        frame_id,
                        context: Some("watch".into()),
                    })
                    .await?;
                let Some(r) = VarRef::from_raw(resp.variables_reference) else {
                    bail!("`{parent}` has no members");
                };
                let children = self.variables(r).await?;
                let bracketed = format!("[{child}]");
                let name = children
                    .iter()
                    .map(|v| &v.name)
                    .find(|n| *n == child || **n == bracketed)
                    .ok_or_else(|| anyhow!("`{parent}` has no member `{child}`"))?
                    .clone();
                Ok((r, name))
            }
        }
    }

    async fn set_variable(
        &mut self,
        container: VarRef,
        name: String,
        value: String,
    ) -> Result<Evaluation> {
        if !self.session.supports(Feature::SetVariable) {
            if self.session.supports(Feature::SetExpression) {
                return self.set_expression(name, value).await;
            }
            bail!("the debug adapter does not support assigning values");
        }
        let resp = self
            .client()?
            .request(SetVariableArguments {
                variables_reference: container.0,
                name,
                value,
            })
            .await?;
        self.invalidate_variables().await;
        Ok(Evaluation {
            value: resp.value,
            type_name: resp.type_.filter(|t| !t.is_empty()),
            children: VarRef::from_raw(resp.variables_reference),
        })
    }

    async fn cmd_complete(&mut self, text: String, column: usize) -> Result<Reply> {
        let column = column.min(text.chars().count());
        let token = token_start(&text, column);
        if !self.session.supports(Feature::Completions) {
            // Fall back to local variable names.
            if !self.session.status.is_stopped() {
                return Ok(Reply::Completions(Vec::new()));
            }
            let Reply::Locals(scopes) = self.cmd_locals().await? else {
                unreachable!()
            };
            let prefix: String = text.chars().skip(token).take(column - token).collect();
            let items = scopes
                .into_iter()
                .flat_map(|s| s.variables)
                .filter(|v| v.name.starts_with(&prefix))
                .map(|v| Completion {
                    label: v.name.clone(),
                    text: v.name,
                    kind: v.type_name,
                    start: token,
                    length: column - token,
                })
                .collect();
            return Ok(Reply::Completions(items));
        }
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(CompletionsArguments {
                column: utf16_len(&text, column) as i64 + 1,
                text: text.clone(),
                frame_id,
            })
            .await?;
        let items = resp
            .targets
            .into_iter()
            .map(|t| {
                let (start, length) = match (t.start, t.length) {
                    (Some(s), l) => {
                        let s = char_offset(&text, (s - 1).max(0) as usize).min(column);
                        let l = l.map_or(0, |l| {
                            char_offset(&text, utf16_len(&text, s) + l.max(0) as usize) - s
                        });
                        (s, l)
                    }
                    (None, Some(l)) => {
                        let s = char_offset(
                            &text,
                            utf16_len(&text, column).saturating_sub(l.max(0) as usize),
                        );
                        (s, column - s)
                    }
                    (None, None) => (token, column - token),
                };
                Completion {
                    text: t.text.unwrap_or_else(|| t.label.clone()),
                    label: t.label,
                    kind: t.type_.or(t.detail),
                    start,
                    length,
                }
            })
            .collect();
        Ok(Reply::Completions(items))
    }

    async fn cmd_locals(&mut self) -> Result<Reply> {
        self.require_stopped()?;
        if self.session.scopes.is_empty()
            && let Some(i) = self.session.selected_frame
        {
            self.select_frame(i).await?;
        }
        let mut scopes: Vec<_> = self
            .session
            .scopes
            .iter()
            .filter(|s| s.is_locals)
            .cloned()
            .collect();
        if scopes.is_empty() {
            scopes.extend(self.session.scopes.iter().find(|s| !s.expensive).cloned());
        }
        let mut out = Vec::new();
        for scope in scopes {
            let mut variables = self.variables(scope.reference).await?;
            self.describe(&mut variables).await;
            out.push(ScopeVariables {
                scope: scope.name,
                reference: scope.reference,
                variables,
            });
        }
        Ok(Reply::Locals(out))
    }

    // -----------------------------------------------------------------------
    // State refresh
    // -----------------------------------------------------------------------

    /// Evaluate only the summary, without expanding children or invoking
    /// ToString(). Each failure belongs to its watch, not to the debugger stop.
    async fn evaluate_watch(&mut self, id: WatchId) {
        let expression = self.session.watches.get(id).unwrap().expression.clone();
        let result = match self.session.current_frame().map(|f| f.id.0) {
            Some(frame_id) => match self.client() {
                Ok(client) => client
                    .request(EvaluateArguments {
                        expression: expression.clone(),
                        frame_id: Some(frame_id),
                        context: Some("watch".into()),
                    })
                    .await
                    .map(|r| WatchValue::from(Evaluation::from(r)))
                    .map_err(|e| evaluate_error(&expression, e).to_string()),
                Err(e) => Err(e.to_string()),
            },
            None => Err("no frame selected".into()),
        };
        self.session.watches.set_result(id, result);
    }

    async fn evaluate_watches(&mut self) {
        if !self.session.status.is_stopped() {
            self.session.watches.invalidate();
            return;
        }
        for w in self.session.watches.snapshot() {
            self.evaluate_watch(w.id).await;
        }
    }

    async fn refresh_watches(&mut self) {
        self.evaluate_watches().await;
        let watches = self.session.watches.snapshot();
        if !watches.is_empty() {
            self.emit(DebugEvent::WatchesChanged(watches));
        }
    }

    async fn refresh_threads(&mut self) -> Result<()> {
        let resp = self.client()?.request(ThreadsArguments {}).await?;
        self.session.threads = resp.threads.into_iter().map(Into::into).collect();
        Ok(())
    }

    async fn try_pause(&mut self) -> Result<()> {
        if !matches!(self.session.status, SessionStatus::Running) {
            self.pause_pending = false;
            return Ok(());
        }
        self.refresh_threads().await?;
        let tid = self
            .session
            .selected_thread
            .filter(|id| self.session.threads.iter().any(|t| t.id == *id))
            .or_else(|| self.session.threads.first().map(|t| t.id));
        if let Some(tid) = tid {
            self.client()?
                .request(PauseArguments { thread_id: tid.0 })
                .await?;
            self.pause_pending = false;
        }
        // No threads yet: retry from the main loop, keeping incoming events
        // and interruption commands usable instead of sleeping inside a command.
        Ok(())
    }

    async fn load_stack(&mut self, tid: ThreadId) -> Result<()> {
        let resp = self
            .client()?
            .request(StackTraceArguments {
                thread_id: tid.0,
                start_frame: None,
                levels: None,
            })
            .await?;
        self.session.on_resume();
        self.session.stack = resp.stack_frames.into_iter().map(Into::into).collect();
        if !self.session.stack.is_empty() {
            self.select_frame(0).await?;
        }
        Ok(())
    }

    async fn select_frame(&mut self, index: usize) -> Result<()> {
        let frame = self
            .session
            .stack
            .get(index)
            .ok_or_else(|| anyhow!("no frame {}", index))?;
        let resp = self
            .client()?
            .request(ScopesArguments {
                frame_id: frame.id.0,
            })
            .await?;
        self.session.selected_frame = Some(index);
        self.session.scopes = resp.scopes.into_iter().map(Into::into).collect();
        Ok(())
    }

    async fn variables(&mut self, r: VarRef) -> Result<Vec<Variable>> {
        if let Some(v) = self.session.variable_cache.get(&r) {
            return Ok(v.clone());
        }
        let resp = self
            .client()?
            .request(VariablesArguments {
                variables_reference: r.0,
            })
            .await?;
        let vars: Vec<Variable> = resp
            .variables
            .into_iter()
            .take(MAX_CHILDREN)
            .map(Into::into)
            .collect();
        self.session.variable_cache.insert(r, vars.clone());
        Ok(vars)
    }

    // -----------------------------------------------------------------------
    // Adapter messages
    // -----------------------------------------------------------------------

    async fn handle_incoming(&mut self, msg: Option<Incoming>) {
        match msg {
            Some(Incoming::Event(event)) => {
                if let Err(e) = self.handle_event(event).await {
                    tracing::warn!("error handling adapter event: {e}");
                }
            }
            Some(Incoming::Request(req)) => {
                tracing::debug!("unsupported reverse request {}", req.command);
                if let Some(conn) = &self.conn {
                    let _ = conn.client.respond(
                        &req,
                        false,
                        Some(format!("{} is not supported by ddbg", req.command)),
                        None,
                    );
                }
            }
            None => {
                // Adapter went away.
                self.incoming = None;
                self.shutdown().await;
            }
        }
    }

    async fn handle_event(&mut self, event: DapEvent) -> Result<()> {
        match event {
            DapEvent::Stopped(s) => self.on_stopped(s).await?,
            DapEvent::Continued(_) => {
                if self.session.status.is_stopped() {
                    self.resumed_at = Some(Instant::now());
                    self.mark_resumed(true);
                }
            }
            DapEvent::Exited(e) => {
                self.pause_pending = false;
                self.session.exit_code = Some(e.exit_code);
                self.session.on_resume();
                self.session.status = SessionStatus::Terminated;
                self.emit(DebugEvent::SessionExited(e.exit_code));
            }
            DapEvent::Terminated => self.shutdown().await,
            DapEvent::Output(o) => {
                if o.category.as_deref() != Some("telemetry") {
                    self.emit(DebugEvent::Output(Output::from_dap(
                        o.category.as_deref(),
                        o.output,
                    )));
                }
            }
            DapEvent::Thread(t) => {
                if t.reason == "exited" {
                    self.session.threads.retain(|th| th.id.0 != t.thread_id);
                }
                self.emit(DebugEvent::ThreadsChanged);
            }
            DapEvent::Breakpoint(b) => {
                let before = b
                    .breakpoint
                    .id
                    .and_then(|id| self.session.breakpoints.by_adapter_id(id))
                    .cloned();
                if let Some(id) = self.session.breakpoints.apply_event(&b.breakpoint) {
                    let bp = self.session.breakpoints.get(id).unwrap().clone();
                    if before.as_ref() != Some(&bp) {
                        self.emit(DebugEvent::BreakpointChanged(bp));
                    }
                }
            }
            DapEvent::Initialized => {}
            DapEvent::Other(e) => tracing::trace!("ignoring event {}", e.event),
        }
        Ok(())
    }

    async fn on_stopped(&mut self, s: dap::StoppedEvent) -> Result<()> {
        self.pause_pending = false;
        let hit_bps: Vec<Breakpoint> = s
            .hit_breakpoint_ids
            .iter()
            .filter_map(|id| self.session.breakpoints.by_adapter_id(*id))
            .cloned()
            .collect();
        if s.reason == "breakpoint"
            && !hit_bps.is_empty()
            && hit_bps.iter().all(|b| b.scope().is_some())
            && let Some(tid) = s.thread_id
            && self.skip_out_of_scope(tid, &hit_bps).await?
        {
            return Ok(());
        }
        let elapsed = self.resumed_at.take().map(|t| t.elapsed());
        let hit: Vec<BreakpointId> = hit_bps.iter().map(|b| b.id).collect();
        let reason = StopReason::from_dap(&s.reason, s.text.clone(), hit);
        self.session.on_resume();
        self.session.status = SessionStatus::Stopped(reason.clone());

        // threads → stackTrace(selected) → top frame → scopes
        self.refresh_threads().await?;
        // The stopping thread is authoritative: lldb-dap's `threads` reply
        // can lag behind newly spawned threads (e.g. libtest's test thread).
        if let Some(t) = s.thread_id.map(ThreadId)
            && !self.session.threads.iter().any(|th| th.id == t)
        {
            self.session.threads.push(Thread {
                id: t,
                name: format!("Thread {t}"),
            });
        }
        let exists = |t: ThreadId| self.session.threads.iter().any(|th| th.id == t);
        let tid = s
            .thread_id
            .map(ThreadId)
            .or(self.session.selected_thread.filter(|t| exists(*t)))
            .or(self.session.threads.first().map(|t| t.id));
        self.session.selected_thread = tid;
        if let Some(tid) = tid {
            self.load_stack(tid).await?;
        }
        if s.reason == "breakpoint" {
            let at = self.session.stack.first().and_then(|f| {
                f.path
                    .as_ref()
                    .map(|p| crate::breakpoint::SourceLocation::new(p.clone(), f.line))
            });
            let hit: Vec<BreakpointId> = hit_bps.iter().map(|b| b.id).collect();
            for id in self.session.breakpoints.mark_hit(&hit, at.as_ref()) {
                if let Some(bp) = self.session.breakpoints.get(id).cloned() {
                    self.emit(DebugEvent::BreakpointChanged(bp));
                }
            }
        }
        let exception = match (&reason, tid) {
            (StopReason::Exception(_), Some(tid)) => self.exception_info(tid).await,
            _ => None,
        };

        self.evaluate_watches().await;
        self.emit(DebugEvent::SessionStopped(Box::new(StopInfo {
            reason,
            thread: tid,
            description: s.description.or(s.text),
            frame: self.session.stack.first().cloned(),
            exception,
            elapsed,
            watches: self.session.watches.snapshot(),
        })));
        Ok(())
    }

    /// Best effort: a failing `exceptionInfo` must not hide the stop.
    async fn exception_info(&self, tid: ThreadId) -> Option<ExceptionInfo> {
        if !self.session.supports(Feature::ExceptionInfo) {
            return None;
        }
        let client = self.client().ok()?;
        match client
            .request(ExceptionInfoArguments { thread_id: tid.0 })
            .await
        {
            Ok(resp) => Some(resp.into()),
            Err(e) => {
                tracing::debug!("exceptionInfo failed: {e}");
                None
            }
        }
    }

    /// Scoped function breakpoints: DAP matches by name only, so a stop in
    /// a same-named function of another file is resumed transparently.
    /// Returns `true` if execution was resumed.
    async fn skip_out_of_scope(&mut self, tid: i64, hit: &[Breakpoint]) -> Result<bool> {
        let resp = self
            .client()?
            .request(StackTraceArguments {
                thread_id: tid,
                start_frame: None,
                levels: Some(1),
            })
            .await?;
        let path = resp
            .stack_frames
            .first()
            .and_then(|f| f.source.as_ref())
            .and_then(|s| s.path.as_deref())
            .map(Path::new);
        if hit.iter().any(|b| b.in_scope(path)) {
            return Ok(false);
        }
        tracing::debug!("skipping stop outside breakpoint file scope: {path:?}");
        self.client()?
            .request(ContinueArguments { thread_id: tid })
            .await?;
        self.session.on_resume();
        self.session.status = SessionStatus::Running;
        Ok(true)
    }

    /// End the session: launched processes terminate, attached ones detach.
    async fn shutdown(&mut self) {
        let terminate = self.conn.as_ref().is_some_and(|c| !c.attached);
        if let Err(e) = self.disconnect(terminate).await {
            tracing::warn!("could not disconnect debug adapter: {e}");
        }
    }

    async fn disconnect(&mut self, terminate: bool) -> Result<()> {
        self.pause_pending = false;
        let Some(mut conn) = self.conn.take() else {
            return Ok(());
        };
        let result = tokio::time::timeout(
            SHUTDOWN_TIMEOUT,
            conn.client.request(DisconnectArguments {
                terminate_debuggee: self
                    .session
                    .supports(Feature::TerminateDebuggee)
                    .then_some(terminate),
            }),
        )
        .await;
        if let Some(child) = conn.child.as_mut()
            && tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
        }
        self.incoming = None;
        let was_active = self.session.status.is_active() || self.session.exit_code.is_some();
        self.session.on_terminated();
        if was_active {
            self.emit(DebugEvent::SessionTerminated);
        }
        result.map_err(|_| anyhow!("debug adapter disconnect timed out"))??;
        Ok(())
    }
}

/// Split `a.b`, `a->b` or `a[2]` into parent and member; `None` for a
/// plain name. Only the outermost (last) access is split.
fn split_lvalue(target: &str) -> Option<(&str, &str)> {
    let target = target.trim();
    if let Some(inner) = target.strip_suffix(']') {
        let mut depth = 0;
        for (i, c) in inner.char_indices().rev() {
            match c {
                ']' => depth += 1,
                '[' if depth == 0 => {
                    let parent = inner[..i].trim();
                    return (!parent.is_empty()).then(|| (parent, inner[i + 1..].trim()));
                }
                '[' => depth -= 1,
                _ => {}
            }
        }
        return None;
    }
    let mut depth = 0;
    let bytes = target.as_bytes();
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => depth -= 1,
            b'.' if depth == 0 => {
                let parent = target[..i].strip_suffix('-').unwrap_or(&target[..i]);
                let parent = parent.trim();
                return (!parent.is_empty()).then(|| (parent, target[i + 1..].trim()));
            }
            b'>' if depth == 0 && i > 0 && bytes[i - 1] == b'-' => {
                let parent = target[..i - 1].trim();
                return (!parent.is_empty()).then(|| (parent, target[i + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// Char offset where the identifier ending at `column` starts.
fn token_start(text: &str, column: usize) -> usize {
    let chars: Vec<char> = text.chars().take(column).collect();
    let mut i = chars.len();
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '$') {
        i -= 1;
    }
    i
}

/// UTF-16 length of the first `chars` chars of `text`.
fn utf16_len(text: &str, chars: usize) -> usize {
    text.chars().take(chars).map(char::len_utf16).sum()
}

/// Char offset of UTF-16 offset `units` in `text`.
fn char_offset(text: &str, units: usize) -> usize {
    let mut n = 0;
    for (i, c) in text.chars().enumerate() {
        if n >= units {
            return i;
        }
        n += c.len_utf16();
    }
    text.chars().count()
}

/// The debuggee's output is piped through the adapter, so programs would
/// normally disable colors. Ask common runtimes to emit ANSI colors anyway,
/// unless `NO_COLOR` is set or the variable is already provided.
fn with_color_env(target: &LaunchTarget) -> LaunchTarget {
    let mut t = target.clone();
    if std::env::var_os("NO_COLOR").is_some() || t.env.contains_key("NO_COLOR") {
        return t;
    }
    for (key, value) in [
        ("DOTNET_SYSTEM_CONSOLE_ALLOW_ANSI_COLOR_REDIRECTION", "1"),
        (
            "Logging__Console__FormatterOptions__ColorBehavior",
            "Enabled",
        ),
        ("FORCE_COLOR", "1"),
        ("CLICOLOR_FORCE", "1"),
        ("CARGO_TERM_COLOR", "always"),
    ] {
        if std::env::var_os(key).is_none() {
            t.env
                .entry(key.to_owned())
                .or_insert_with(|| value.to_owned());
        }
    }
    t
}

/// Element count from a .NET array value such as `{int[3]}` or
/// `{string[2, 4]}`.
fn array_length_of(value: &str) -> Option<u64> {
    let inner = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    if !inner.ends_with(']') {
        return None;
    }
    inner.split('[').skip(1).find_map(|group| {
        let dims = group.strip_suffix(']')?;
        dims.split(',')
            .map(|d| d.trim().parse::<u64>().ok())
            .try_fold(1u64, |acc, d| acc.checked_mul(d?))
    })
}

#[cfg(test)]
mod array_length_tests {
    use super::array_length_of;
    use quickcheck_macros::quickcheck;

    fn array_value(dimensions: &[u64]) -> String {
        let dimensions = dimensions
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{int[{dimensions}]}}")
    }

    #[quickcheck]
    fn array_dimensions_have_checked_product(first: u64, rest: Vec<u64>) -> bool {
        let dimensions: Vec<_> = std::iter::once(first).chain(rest).collect();
        let expected = dimensions
            .iter()
            .try_fold(1u64, |product, &n| product.checked_mul(n));
        array_length_of(&array_value(&dimensions)) == expected
    }

    #[quickcheck]
    fn zero_dimension_gives_zero(rest: Vec<u64>) -> bool {
        let dimensions: Vec<_> = std::iter::once(0).chain(rest).collect();
        array_length_of(&array_value(&dimensions)) == Some(0)
    }

    #[quickcheck]
    fn malformed_dimensions_are_rejected(number: u64) -> bool {
        array_length_of(&format!("{{int[{number}, invalid]}}")).is_none()
            && array_length_of(&format!("{{int[-{number}]}}")).is_none()
    }

    #[test]
    fn overflowing_dimensions_are_rejected() {
        assert_eq!(array_length_of("{int[18446744073709551615, 2]}"), None);
    }

    #[test]
    fn parses_array_values() {
        assert_eq!(array_length_of("{int[3]}"), Some(3));
        assert_eq!(array_length_of("{Foo.Bar[0]}"), Some(0));
        assert_eq!(array_length_of("{int[2, 4]}"), Some(8));
        assert_eq!(array_length_of("{int[2][]}"), Some(2));
        assert_eq!(array_length_of("{System.Guid}"), None);
        assert_eq!(array_length_of("{int[]}"), None);
        assert_eq!(array_length_of("3"), None);
    }
}

/// Prefixes of `expr` ending before each top-level `.` or `->`, shortest
/// first: `a[0].b.c` gives `a[0]` and `a[0].b`.
fn member_prefixes(expr: &str) -> Vec<&str> {
    let mut prefixes = Vec::new();
    let mut depth = 0i32;
    let mut quote = None;
    let bytes = expr.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        match (quote, c) {
            (Some(q), _) if c == q && bytes.get(i.wrapping_sub(1)) != Some(&b'\\') => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'(' | b'[' | b'{') => depth += 1,
            (None, b')' | b']' | b'}') => depth -= 1,
            (None, b'.') if depth == 0 && i > 0 => prefixes.push(&expr[..i]),
            (None, b'-') if depth == 0 && i > 0 && bytes.get(i + 1) == Some(&b'>') => {
                prefixes.push(&expr[..i])
            }
            _ => {}
        }
    }
    prefixes.retain(|p| !p.trim().is_empty() && !p.trim().bytes().all(|c| c.is_ascii_digit()));
    prefixes
}

/// Name the failing expression and explain bare HRESULTs from netcoredbg.
fn evaluate_error(expression: &str, e: anyhow::Error) -> anyhow::Error {
    let message = e.to_string();
    let hint = [
        (
            "0x80070057",
            "invalid argument; often a member access on a null value or an expression \
             the evaluator does not support",
        ),
        ("0x80004005", "unspecified failure"),
    ]
    .into_iter()
    .find(|(code, _)| message.to_ascii_lowercase().contains(code))
    .map(|(_, hint)| format!(" ({hint})"))
    .unwrap_or_default();
    anyhow!("cannot evaluate `{expression}`: {message}{hint}")
}

#[cfg(test)]
mod evaluate_error_tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn member_chains_split_at_top_level(accesses: Vec<(u8, u16)>) -> bool {
        let mut expression = String::from("root");
        let mut expected_prefixes = Vec::new();
        let mut expected_lvalue = None;
        for (kind, number) in accesses {
            let parent = expression.clone();
            let member = match kind % 4 {
                0 | 1 => {
                    expected_prefixes.push(parent.clone());
                    let member = format!("member{number}");
                    expression.push_str(if kind % 4 == 0 { "." } else { "->" });
                    expression.push_str(&member);
                    member
                }
                2 => {
                    let index = format!("indices[{number}]");
                    expression.push_str(&format!("[{index}]"));
                    index
                }
                _ => {
                    // Member access inside a call must not create a prefix.
                    let index = format!("lookup(item.member{number})");
                    expression.push_str(&format!("[{index}]"));
                    index
                }
            };
            expected_lvalue = Some((parent, member));
        }
        let actual_prefixes: Vec<_> = member_prefixes(&expression)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let actual_lvalue = split_lvalue(&expression)
            .map(|(parent, member)| (parent.to_owned(), member.to_owned()));
        actual_prefixes == expected_prefixes && actual_lvalue == expected_lvalue
    }

    #[quickcheck]
    fn quoted_member_access_is_not_a_prefix(number: u16, arrow: bool) -> bool {
        let parent = format!("root[\"key.{number}->value\"]");
        let expression = format!("{parent}{}member", if arrow { "->" } else { "." });
        member_prefixes(&expression) == vec![parent.as_str()]
    }

    #[test]
    fn names_expression_and_explains_hresult() {
        let e = evaluate_error("a.b", anyhow!("evaluate failed: error 0x80070057"));
        let s = e.to_string();
        assert!(
            s.starts_with("cannot evaluate `a.b`: evaluate failed"),
            "{s}"
        );
        assert!(s.contains("null"), "{s}");
    }

    #[test]
    fn splits_member_prefixes() {
        assert_eq!(
            member_prefixes("Issue[0].Diagnostics.Length"),
            ["Issue[0]", "Issue[0].Diagnostics"]
        );
        assert_eq!(member_prefixes("f(a.b).c"), ["f(a.b)"]);
        assert_eq!(member_prefixes("p->next->x"), ["p", "p->next"]);
        assert_eq!(member_prefixes("s[\"a.b\"].x"), ["s[\"a.b\"]"]);
        assert!(member_prefixes("x").is_empty());
    }
}

#[cfg(test)]
mod lvalue_tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn character_offsets_roundtrip_through_utf16(text: String, offset: usize) -> bool {
        let offset = offset % (text.chars().count() + 1);
        char_offset(&text, utf16_len(&text, offset)) == offset
    }

    #[quickcheck]
    fn utf16_offsets_round_up_to_character_boundaries(text: String, units: usize) -> bool {
        let total = text.encode_utf16().count();
        [units, units % (total + 2)].into_iter().all(|units| {
            let offset = char_offset(&text, units);
            let boundary = utf16_len(&text, offset);
            offset <= text.chars().count()
                && boundary >= units.min(total)
                && (offset == 0 || utf16_len(&text, offset - 1) < units.min(total))
        })
    }

    #[quickcheck]
    fn token_start_finds_trailing_identifier(text: String, column: usize) -> bool {
        let column = column % (text.chars().count() + 2);
        let prefix: Vec<_> = text.chars().take(column).collect();
        let is_identifier = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
        let trailing = prefix
            .iter()
            .rev()
            .take_while(|&&c| is_identifier(c))
            .count();
        token_start(&text, column) == prefix.len() - trailing
    }

    #[test]
    fn splits_lvalues() {
        assert_eq!(split_lvalue("x"), None);
        assert_eq!(split_lvalue("p.x"), Some(("p", "x")));
        assert_eq!(split_lvalue("a.b.c"), Some(("a.b", "c")));
        assert_eq!(split_lvalue("p->x"), Some(("p", "x")));
        assert_eq!(split_lvalue("v[2]"), Some(("v", "2")));
        assert_eq!(split_lvalue("m[a[1]]"), Some(("m", "a[1]")));
        assert_eq!(split_lvalue("f(a.b)"), None);
    }

    #[test]
    fn completion_offsets() {
        assert_eq!(token_start("p.po", 4), 2);
        assert_eq!(token_start("x + ab", 6), 4);
        assert_eq!(utf16_len("é😀a", 3), 4);
        assert_eq!(char_offset("é😀a", 3), 2);
    }
}
