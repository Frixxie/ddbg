//! The debug engine: single owner of [`DebugSession`].
//!
//! Frontends send [`Command`]s through an [`EngineHandle`] and observe
//! [`DebugEvent`]s. The engine task multiplexes commands and adapter
//! messages, so all state transitions happen in one place.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ddbg_dap::client::channel_closed;
use ddbg_dap::protocol::{
    self as dap, ConfigurationDoneArguments, ContinueArguments, DapEvent, DisconnectArguments,
    EvaluateArguments, InitializeArguments, LaunchArguments, NextArguments, PauseArguments,
    ScopesArguments, StackTraceArguments, StepInArguments, StepOutArguments, ThreadsArguments,
    VariablesArguments,
};
use ddbg_dap::{AdapterProcess, DapClient, Incoming};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::adapter::DebugAdapter;
use crate::breakpoint::BreakpointId;
use crate::command::{Command, FrameSelector, Location, Reply, ScopeVariables};
use anyhow::{Result, anyhow, bail};

use crate::error::{NO_TARGET, NO_THREAD, NOT_RUNNING, NOT_STOPPED};
use crate::event::{DebugEvent, Output, StopInfo};
use crate::session::{DebugSession, Feature, SessionStatus, StopReason};
use crate::target::{DebugTarget, LaunchTarget};
use crate::thread::ThreadId;
use crate::variable::{Evaluation, VarRef, Variable};

const MAX_CHILDREN: usize = 100;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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
}

/// Start the engine task.
pub fn spawn(config: EngineConfig) -> EngineHandle {
    spawn_with_connector(config, spawn_connector())
}

/// Start the engine task with a custom connector (used by tests).
pub fn spawn_with_connector(config: EngineConfig, connector: Connector) -> EngineHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (events, _) = broadcast::channel(1024);
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
    };
    tokio::spawn(engine.run(cmd_rx));
    EngineHandle { cmd_tx, events }
}

struct ActiveConnection {
    client: DapClient,
    child: Option<Child>,
}

struct Engine {
    session: DebugSession,
    adapter: Arc<dyn DebugAdapter>,
    connector: Connector,
    conn: Option<ActiveConnection>,
    incoming: Option<mpsc::UnboundedReceiver<Incoming>>,
    cwd: PathBuf,
    events: broadcast::Sender<DebugEvent>,
}

async fn recv_incoming(rx: &mut Option<mpsc::UnboundedReceiver<Incoming>>) -> Option<Incoming> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl Engine {
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<CommandMsg>) {
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
            }
        }
        self.shutdown().await;
    }

    fn emit(&self, event: DebugEvent) {
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
            Command::Continue => {
                let tid = self.require_stopped()?;
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
                let client = self.client()?;
                let tid = match self.session.selected_thread {
                    Some(t) => t,
                    None => {
                        self.refresh_threads().await?;
                        self.session
                            .threads
                            .first()
                            .map(|t| t.id)
                            .ok_or_else(|| anyhow!(NO_THREAD))?
                    }
                };
                client.request(PauseArguments { thread_id: tid.0 }).await?;
                Ok(Reply::Ok)
            }
            Command::Kill => {
                if !self.session.status.is_active() {
                    bail!(NOT_RUNNING);
                }
                self.shutdown().await;
                Ok(Reply::Ok)
            }
            Command::Next => {
                let tid = self.require_stopped()?;
                self.client()?
                    .request(NextArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(false);
                Ok(Reply::Ok)
            }
            Command::Step => {
                let tid = self.require_stopped()?;
                self.client()?
                    .request(StepInArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(false);
                Ok(Reply::Ok)
            }
            Command::Finish => {
                let tid = self.require_stopped()?;
                self.client()?
                    .request(StepOutArguments { thread_id: tid.0 })
                    .await?;
                self.mark_resumed(false);
                Ok(Reply::Ok)
            }
            Command::Break(Location::Source(loc)) => {
                let (id, new) = self.session.breakpoints.add(loc, &self.cwd);
                let path = self
                    .session
                    .breakpoints
                    .get(id)
                    .unwrap()
                    .requested
                    .path
                    .clone();
                if self.can_set_breakpoints() {
                    self.sync_breakpoints(&path).await?;
                }
                let breakpoint = self.session.breakpoints.get(id).unwrap().clone();
                Ok(Reply::BreakpointSet { breakpoint, new })
            }
            Command::DeleteBreakpoint(id) => {
                let bp = self
                    .session
                    .breakpoints
                    .remove(id)
                    .ok_or_else(|| anyhow!("no breakpoint {id}"))?;
                if self.can_set_breakpoints() {
                    self.sync_breakpoints(&bp.requested.path).await?;
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
                }
                self.frame_reply(index)
            }
            Command::Print(expr) => self.cmd_print(expr).await,
            Command::Locals => self.cmd_locals().await,
            Command::Tests(_) => Err(anyhow!("test discovery is not implemented yet")),
            Command::TestRun(_) => Err(anyhow!("test-run is not implemented yet")),
            Command::TestDebug(_) => Err(anyhow!("test-debug is not implemented yet")),
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
        self.session.on_resume();
        let was_stopped = self.session.status.is_stopped();
        self.session.status = SessionStatus::Running;
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
                return Err(anyhow!("attach is not implemented yet"));
            }
            None => bail!(NO_TARGET),
        };
        if self.conn.is_some() {
            self.shutdown().await;
        }
        match self.launch(&target).await {
            Ok(()) => Ok(Reply::Launched(target.absolute_program())),
            Err(e) => {
                self.shutdown().await;
                Err(e)
            }
        }
    }

    /// DAP lifecycle: initialize → launch → `initialized` → setBreakpoints
    /// → configurationDone → launch response.
    async fn launch(&mut self, target: &LaunchTarget) -> Result<()> {
        let launch_args = self.adapter.build_launch_request(target)?;
        let conn = (self.connector)(self.adapter.as_ref())?;
        let client = conn.client.clone();
        let mut incoming = conn.incoming;
        self.conn = Some(ActiveConnection {
            client: client.clone(),
            child: conn.child,
        });
        self.session.status = SessionStatus::Initializing;
        self.session.exit_code = None;

        let caps = client
            .request(InitializeArguments::new(self.adapter.id()))
            .await?;
        self.session.capabilities = caps;

        let mut launch = client.send(LaunchArguments(launch_args))?;
        let mut launched = false;
        loop {
            tokio::select! {
                r = &mut launch, if !launched => {
                    r?;
                    launched = true;
                }
                msg = incoming.recv() => match msg {
                    Some(Incoming::Event(DapEvent::Initialized)) => break,
                    None => return Err(channel_closed()),
                    other => self.handle_incoming(other).await,
                }
            }
        }

        self.session.status = SessionStatus::Configuring;
        for file in self.session.breakpoints.files() {
            self.sync_breakpoints(&file).await?;
        }
        if self.session.supports(Feature::ConfigurationDone) {
            client.request(ConfigurationDoneArguments {}).await?;
        }
        if !launched {
            launch.await?;
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

    async fn cmd_print(&mut self, expression: String) -> Result<Reply> {
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(EvaluateArguments {
                expression,
                frame_id,
                context: Some("watch".into()),
            })
            .await?;
        let eval = Evaluation::from(resp);
        let children = match eval.children {
            Some(r) => self.variables(r).await?,
            None => Vec::new(),
        };
        Ok(Reply::Value(eval, children))
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
            let variables = self.variables(scope.reference).await?;
            out.push(ScopeVariables {
                scope: scope.name,
                variables,
            });
        }
        Ok(Reply::Locals(out))
    }

    // -----------------------------------------------------------------------
    // State refresh
    // -----------------------------------------------------------------------

    async fn refresh_threads(&mut self) -> Result<()> {
        let resp = self.client()?.request(ThreadsArguments {}).await?;
        self.session.threads = resp.threads.into_iter().map(Into::into).collect();
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
                    self.mark_resumed(true);
                }
            }
            DapEvent::Exited(e) => {
                self.session.exit_code = Some(e.exit_code);
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
        let hit: Vec<BreakpointId> = s
            .hit_breakpoint_ids
            .iter()
            .filter_map(|id| self.session.breakpoints.by_adapter_id(*id))
            .map(|b| b.id)
            .collect();
        let reason = StopReason::from_dap(&s.reason, s.text.clone(), hit);
        self.session.on_resume();
        self.session.status = SessionStatus::Stopped(reason.clone());

        // threads → stackTrace(selected) → top frame → scopes
        self.refresh_threads().await?;
        let exists = |t: ThreadId| self.session.threads.iter().any(|th| th.id == t);
        let tid = s
            .thread_id
            .map(ThreadId)
            .filter(|t| exists(*t))
            .or(self.session.selected_thread.filter(|t| exists(*t)))
            .or(self.session.threads.first().map(|t| t.id));
        self.session.selected_thread = tid;
        if let Some(tid) = tid {
            self.load_stack(tid).await?;
        }

        self.emit(DebugEvent::SessionStopped(StopInfo {
            reason,
            thread: tid,
            description: s.description.or(s.text),
            frame: self.session.stack.first().cloned(),
        }));
        Ok(())
    }

    /// Disconnect from the adapter (terminating the debuggee) and reset state.
    async fn shutdown(&mut self) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        let terminate = self.session.supports(Feature::TerminateDebuggee);
        let _ = tokio::time::timeout(
            SHUTDOWN_TIMEOUT,
            conn.client.request(DisconnectArguments {
                terminate_debuggee: terminate.then_some(true),
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
        let was_active = self.session.status.is_active();
        self.session.on_terminated();
        if was_active {
            self.emit(DebugEvent::SessionTerminated);
        }
    }
}
