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
    SetExceptionBreakpointsArguments, SetExpressionArguments, SetVariableArguments,
    StackTraceArguments, StepInArguments, StepOutArguments, ThreadsArguments, VariablesArguments,
};
use ddbg_dap::{DapClient, Incoming};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::adapter::DebugAdapter;
use crate::breakpoint::{Breakpoint, BreakpointId};
use crate::command::{Command, Completion, FrameSelector, Location, Reply, ScopeVariables};
use anyhow::{Result, anyhow, bail};

use crate::error::{
    ALREADY_STOPPED, NO_CONDITIONAL_BREAKPOINTS, NO_FUNCTION_BREAKPOINTS, NO_TARGET, NO_THREAD,
    NOT_RUNNING, NOT_STOPPED,
};
use crate::event::{DebugEvent, ExceptionInfo, Output, StopInfo};
use crate::session::{DebugSession, Feature, SessionStatus, StopReason};
use crate::target::{AttachTarget, DebugTarget, LaunchTarget};
use crate::thread::{Thread, ThreadId};
use crate::variable::{Evaluation, VarRef, Variable};
use crate::watch::{WatchId, WatchValue};

mod breakpoints;
mod events;
mod inspect;
mod run;
mod watches;

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
    Box::new(|adapter| adapter.connect())
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
        continuing: false,
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
    /// The last resume was `continue`, not a step.
    continuing: bool,
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
            Command::Detach => self.cmd_detach().await,
            Command::Continue => self.cmd_continue().await,
            Command::Pause => self.cmd_pause().await,
            Command::Kill => self.cmd_kill().await,
            Command::Next => self.cmd_next().await,
            Command::Step => self.cmd_step().await,
            Command::Finish => self.cmd_finish().await,
            Command::Break(loc) => self.cmd_break(loc, None).await,
            Command::ConditionalBreak {
                location,
                condition,
            } => self.cmd_break(location, Some(condition)).await,
            Command::Condition { id, condition } => self.cmd_condition(id, condition).await,
            Command::DeleteBreakpoint(id) => self.cmd_delete_breakpoint(id).await,
            Command::Breakpoints => self.cmd_breakpoints(),
            Command::Backtrace => self.cmd_backtrace().await,
            Command::Threads => self.cmd_threads().await,
            Command::Thread(tid) => self.cmd_thread(tid).await,
            Command::Frame(sel) => self.cmd_frame(sel).await,
            Command::Print(expr) => self.cmd_print(expr, "watch").await,
            Command::Watch(expression) => self.cmd_watch(expression).await,
            Command::Unwatch(id) => self.cmd_unwatch(id),
            Command::Watches => self.cmd_watches(),
            Command::Eval(expr) => self.cmd_print(expr, "repl").await,
            Command::Set { target, value } => self.cmd_set(target, value).await,
            Command::SetVariable { scope, name, value } => {
                self.cmd_set_variable(scope, name, value).await
            }
            Command::Complete { text, column } => self.cmd_complete(text, column).await,
            Command::Locals => self.cmd_locals().await,
            Command::Tests(_) => Err(anyhow!("test discovery is not implemented yet")),
            Command::TestRun(_) => Err(anyhow!("test-run is not implemented yet")),
            Command::TestDebug { .. } => Err(anyhow!("test-debug is not implemented yet")),
            Command::Quit => self.cmd_quit().await,
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
}
