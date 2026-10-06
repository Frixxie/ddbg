//! Programmatic driver for ddbg: do from code what you do in the REPL.
//!
//! A [`Debugger`] wraps the same [`Session`] the CLI and TUI use, so project
//! detection, adapter selection, program resolution and test commands behave
//! exactly as in `ddbg`. It offers two layers:
//!
//! - **Typed methods** ([`Debugger::break_at`], [`Debugger::run`],
//!   [`Debugger::next`], [`Debugger::print`], ...) that return structured
//!   values. Commands that resume the program wait until it stops again and
//!   return a [`Halt`].
//! - **Text commands** ([`Debugger::exec`]) that take a REPL line such as
//!   `"break src/main.rs:14"` and return the text the REPL would print,
//!   including the events that happened while the command ran.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use ddbg_driver::Debugger;
//!
//! let mut dbg = Debugger::from_cli(["--", "target/debug/my-app"], "/path/to/project")?;
//! dbg.break_at("src/main.rs", 14).await?;
//! let stop = dbg.run().await?.into_stopped()?;
//! assert_eq!(stop.frame.unwrap().line, 14);
//! assert_eq!(dbg.print("point.x").await?.value, "3");
//! dbg.cont().await?.into_exited()?;
//! # Ok(()) }
//! ```

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ddbg_cli::parser::{Input, parse};
use ddbg_cli::render::{Renderer, help};
use ddbg_cli::testing::{Tests, render_list};
use ddbg_cli::{Args, Outcome, Parser, Session};
use ddbg_core::breakpoint::{Breakpoint, BreakpointId, FunctionLocation, SourceLocation};
use ddbg_core::command::{
    Command, Completion, FrameSelector, Location, Reply, ScopeVariables, TestQuery, TestSelector,
};
use ddbg_core::event::{OutputCategory, StopInfo};
use ddbg_core::frame::StackFrame;
use ddbg_core::thread::{Thread, ThreadId};
use ddbg_core::variable::{Evaluation, Variable};
use ddbg_core::watch::{Watch, WatchId};
use ddbg_core::{DebugEvent, EngineHandle, LaunchTarget};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};

pub use ddbg_cli::TestCase;
pub use ddbg_core::session::StopReason;

/// Default time to wait for the program to stop, exit or terminate.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Error returned when the program did not halt within the timeout. The
/// program may still be running; detect it with `anyhow::Error::downcast_ref`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout(pub Duration);

impl std::fmt::Display for Timeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timed out after {:?} waiting for the debugger", self.0)
    }
}

impl std::error::Error for Timeout {}

/// Why the program is no longer running after a resuming command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // short-lived, one per command
pub enum Halt {
    Stopped(StopInfo),
    Exited(i64),
    /// The session ended without an exit code.
    Terminated,
}

impl Halt {
    pub fn into_stopped(self) -> Result<StopInfo> {
        match self {
            Self::Stopped(info) => Ok(info),
            other => bail!("expected the program to stop, got {other:?}"),
        }
    }

    /// The exit code; fails unless the program exited.
    pub fn into_exited(self) -> Result<i64> {
        match self {
            Self::Exited(code) => Ok(code),
            other => bail!("expected the program to exit, got {other:?}"),
        }
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped(_))
    }
}

/// A debug session driven from code.
pub struct Debugger {
    session: Session,
    events: broadcast::Receiver<DebugEvent>,
    renderer: Renderer,
    /// Rendered event text not yet returned by [`Debugger::exec`].
    pending: Vec<String>,
    stdout: String,
    stderr: String,
    timeout: Duration,
}

impl Debugger {
    /// Start like `ddbg <args>` run from `cwd`, e.g.
    /// `["--adapter", "lldb-dap", "--", "target/debug/app"]`. Logging is not
    /// initialized and subcommands are not supported.
    pub fn from_cli<I, T>(args: I, cwd: impl Into<PathBuf>) -> Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let argv = std::iter::once(OsString::from("ddbg")).chain(args.into_iter().map(Into::into));
        let args = Args::try_parse_from(argv)?;
        Self::from_args(&args, cwd)
    }

    /// Start from parsed CLI arguments as if `ddbg` was run in `cwd`.
    ///
    /// Commands implied by the arguments (`--run`, `--stop-on-entry`) are
    /// queued; use [`Debugger::wait`] to observe their effect.
    pub fn from_args(args: &Args, cwd: impl Into<PathBuf>) -> Result<Self> {
        if args.command.is_some() {
            bail!("subcommands are not supported by the driver");
        }
        let cwd = cwd.into();
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("working directory {}", cwd.display()))?;
        let prepared = ddbg_cli::prepare(args, cwd)?;
        let mut dbg = Self::from_session(prepared.session);
        dbg.renderer.show_console = prepared.verbose;
        for cmd in prepared.initial {
            let engine = dbg.session.engine.clone();
            tokio::spawn(async move { engine.execute(cmd).await });
        }
        Ok(dbg)
    }

    /// Drive an existing session (as built by [`ddbg_cli::prepare`]).
    pub fn from_session(session: Session) -> Self {
        Self {
            events: session.engine.subscribe(),
            renderer: Renderer::new(session.cwd.clone()),
            session,
            pending: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Drive an engine directly, without project detection or tests. Useful
    /// with [`ddbg_core::engine::spawn_with_connector`] and a fake adapter.
    pub fn from_engine(engine: EngineHandle, cwd: impl Into<PathBuf>) -> Self {
        Self::from_session(Session {
            engine,
            cwd: cwd.into(),
            candidates: Vec::new(),
            tests: Tests::new(None, "no project; cannot discover tests"),
        })
    }

    /// How long resuming commands wait for the program to halt.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Change how long resuming commands wait for the program to halt.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn engine(&self) -> &EngineHandle {
        &self.session.engine
    }

    pub fn cwd(&self) -> &Path {
        &self.session.cwd
    }

    // -----------------------------------------------------------------------
    // Text commands
    // -----------------------------------------------------------------------

    /// Execute a REPL line and return what the REPL would print: the command
    /// result followed by events (stops, output, exit) that happened since
    /// the previous call. Resuming commands wait until the program halts.
    /// Command errors are returned as `Err`.
    pub async fn exec(&mut self, line: &str) -> Result<String> {
        let cmd = match parse(line).map_err(anyhow::Error::msg)? {
            Input::Empty => return Ok(self.take_pending()),
            Input::Help(topic) => return Ok(help(topic.as_deref())),
            Input::Command(cmd) => cmd,
        };
        let resumes = resumes(&cmd);
        if resumes {
            self.drain();
        }
        let mut text = match self.session.execute(cmd, &mut |_| {}).await {
            Outcome::Reply(cmd, reply) => self.renderer.reply(&cmd, &reply),
            Outcome::Tests(tests) => Some(render_list(&tests)),
            Outcome::Text(t) => Some(t),
            Outcome::Error(e) => bail!(e),
            Outcome::Quit => None,
        };
        if resumes {
            self.wait().await?;
        } else {
            self.drain();
        }
        let events = self.take_pending();
        if !events.is_empty() {
            let t = text.get_or_insert_default();
            if !t.is_empty() {
                t.push('\n');
            }
            t.push_str(&events);
        }
        Ok(text.unwrap_or_default())
    }

    /// Execute several REPL lines, failing on the first error, and return
    /// the combined transcript with each line echoed as `ddbg> <line>`.
    pub async fn script(&mut self, lines: &str) -> Result<String> {
        let mut out = String::new();
        for line in lines.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let text = self.exec(line).await.with_context(|| format!("`{line}`"))?;
            out.push_str("ddbg> ");
            out.push_str(line);
            out.push('\n');
            if !text.is_empty() {
                out.push_str(&text);
                out.push('\n');
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Typed commands
    // -----------------------------------------------------------------------

    /// Send a command to the engine and return its raw reply. Does not wait
    /// for the program to halt; see [`Debugger::wait`].
    pub async fn execute(&mut self, cmd: Command) -> Result<Reply> {
        match self.session.execute(cmd, &mut |_| {}).await {
            Outcome::Reply(_, reply) => Ok(reply),
            Outcome::Quit => Ok(Reply::Quit),
            Outcome::Error(e) => Err(anyhow!(e)),
            other => bail!("unexpected outcome {other:?}"),
        }
    }

    /// Execute a resuming command and wait until the program halts.
    async fn resume(&mut self, cmd: Command) -> Result<Halt> {
        self.drain();
        self.execute(cmd).await?;
        self.wait().await
    }

    /// `run`: start (or restart) the current program.
    pub async fn run(&mut self) -> Result<Halt> {
        self.resume(Command::Run(None)).await
    }

    /// `run <program> [args...]`. Bare names resolve against detected
    /// project binaries, like in the REPL.
    pub async fn run_program<I, S>(&mut self, program: impl Into<PathBuf>, args: I) -> Result<Halt>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let target = LaunchTarget::new(program, args.into_iter().map(Into::into).collect());
        self.resume(Command::Run(Some(target))).await
    }

    /// `continue`
    pub async fn cont(&mut self) -> Result<Halt> {
        self.resume(Command::Continue).await
    }

    /// `next` (step over)
    pub async fn next(&mut self) -> Result<Halt> {
        self.resume(Command::Next).await
    }

    /// `step` (step into)
    pub async fn step(&mut self) -> Result<Halt> {
        self.resume(Command::Step).await
    }

    /// `finish` (step out)
    pub async fn finish(&mut self) -> Result<Halt> {
        self.resume(Command::Finish).await
    }

    /// `pause`
    pub async fn pause(&mut self) -> Result<Halt> {
        self.resume(Command::Pause).await
    }

    /// `kill`: terminate the program.
    pub async fn kill(&mut self) -> Result<()> {
        self.execute(Command::Kill).await.map(drop)
    }

    /// `quit`: end the session and shut down the adapter.
    pub async fn quit(mut self) -> Result<()> {
        self.execute(Command::Quit).await.map(drop)
    }

    /// `break <file>:<line>`
    pub async fn break_at(&mut self, file: impl Into<PathBuf>, line: u32) -> Result<Breakpoint> {
        self.set_breakpoint(Location::Source(SourceLocation::new(file, line)))
            .await
    }

    /// `break <function>`
    pub async fn break_function(&mut self, name: &str) -> Result<Breakpoint> {
        self.set_breakpoint(Location::Function(FunctionLocation::new(name, None)))
            .await
    }

    /// `break <file>:<function>`
    pub async fn break_function_in(
        &mut self,
        file: impl Into<PathBuf>,
        name: &str,
    ) -> Result<Breakpoint> {
        self.set_breakpoint(Location::Function(FunctionLocation::new(
            name,
            Some(file.into()),
        )))
        .await
    }

    async fn set_breakpoint(&mut self, location: Location) -> Result<Breakpoint> {
        match self.execute(Command::Break(location)).await? {
            Reply::BreakpointSet { breakpoint, .. } => Ok(breakpoint),
            other => unexpected(other),
        }
    }

    /// Set a source or function breakpoint with an adapter-native condition.
    pub async fn break_if(
        &mut self,
        location: Location,
        condition: impl Into<String>,
    ) -> Result<Breakpoint> {
        match self
            .execute(Command::ConditionalBreak {
                location,
                condition: condition.into(),
            })
            .await?
        {
            Reply::BreakpointSet { breakpoint, .. } => Ok(breakpoint),
            other => unexpected(other),
        }
    }

    /// Change a breakpoint's condition; `None` makes it unconditional.
    pub async fn condition(
        &mut self,
        id: BreakpointId,
        condition: Option<String>,
    ) -> Result<Breakpoint> {
        match self.execute(Command::Condition { id, condition }).await? {
            Reply::BreakpointSet { breakpoint, .. } => Ok(breakpoint),
            other => unexpected(other),
        }
    }

    /// `delete <id>`
    ///
    /// Watch expressions are managed separately with `unwatch`.
    pub async fn delete(&mut self, id: BreakpointId) -> Result<()> {
        self.execute(Command::DeleteBreakpoint(id)).await.map(drop)
    }

    /// `breakpoints`
    pub async fn breakpoints(&mut self) -> Result<Vec<Breakpoint>> {
        match self.execute(Command::Breakpoints).await? {
            Reply::Breakpoints(bps) => Ok(bps),
            other => unexpected(other),
        }
    }

    /// `watch <expression>`: add an expression, evaluating it if stopped.
    pub async fn watch(&mut self, expression: impl Into<String>) -> Result<Watch> {
        match self.execute(Command::Watch(expression.into())).await? {
            Reply::WatchSet { watch, .. } => Ok(watch),
            other => unexpected(other),
        }
    }

    /// `watches`: current summaries, without re-evaluating expressions.
    pub async fn watches(&mut self) -> Result<Vec<Watch>> {
        match self.execute(Command::Watches).await? {
            Reply::Watches(watches) => Ok(watches),
            other => unexpected(other),
        }
    }

    /// `unwatch <id>`
    pub async fn unwatch(&mut self, id: WatchId) -> Result<()> {
        self.execute(Command::Unwatch(id)).await.map(drop)
    }

    /// `backtrace`
    pub async fn backtrace(&mut self) -> Result<Vec<StackFrame>> {
        match self.execute(Command::Backtrace).await? {
            Reply::Backtrace { frames, .. } => Ok(frames),
            other => unexpected(other),
        }
    }

    /// `threads`
    pub async fn threads(&mut self) -> Result<Vec<Thread>> {
        match self.execute(Command::Threads).await? {
            Reply::Threads { threads, .. } => Ok(threads),
            other => unexpected(other),
        }
    }

    /// `thread <id>`
    pub async fn thread(&mut self, id: ThreadId) -> Result<()> {
        self.execute(Command::Thread(id)).await.map(drop)
    }

    /// `frame [n]`, `up`, `down`: select a frame and return it with its index.
    pub async fn frame(&mut self, selector: FrameSelector) -> Result<(usize, StackFrame)> {
        match self.execute(Command::Frame(selector)).await? {
            Reply::Frame { index, frame } => Ok((index, frame)),
            other => unexpected(other),
        }
    }

    /// `print <expr>`
    pub async fn print(&mut self, expr: &str) -> Result<Evaluation> {
        self.print_with_children(expr).await.map(|(v, _)| v)
    }

    /// `print <expr>`, with one level of children.
    pub async fn print_with_children(&mut self, expr: &str) -> Result<(Evaluation, Vec<Variable>)> {
        match self.execute(Command::Print(expr.to_owned())).await? {
            Reply::Value(v, children) => Ok((v, children)),
            other => unexpected(other),
        }
    }

    /// `eval <expr>`: evaluate in the adapter's REPL context.
    pub async fn eval(&mut self, expr: &str) -> Result<Evaluation> {
        match self.execute(Command::Eval(expr.to_owned())).await? {
            Reply::Value(v, _) => Ok(v),
            other => unexpected(other),
        }
    }

    /// `set <target> = <value>`: assign and return the new value.
    pub async fn set(&mut self, target: &str, value: &str) -> Result<Evaluation> {
        let cmd = Command::Set {
            target: target.to_owned(),
            value: value.to_owned(),
        };
        match self.execute(cmd).await? {
            Reply::Value(v, _) => Ok(v),
            other => unexpected(other),
        }
    }

    /// Complete an expression with the cursor at its end.
    pub async fn complete(&mut self, text: &str) -> Result<Vec<Completion>> {
        let cmd = Command::Complete {
            text: text.to_owned(),
            column: text.chars().count(),
        };
        match self.execute(cmd).await? {
            Reply::Completions(c) => Ok(c),
            other => unexpected(other),
        }
    }

    /// `locals`, grouped by scope.
    pub async fn scopes(&mut self) -> Result<Vec<ScopeVariables>> {
        match self.execute(Command::Locals).await? {
            Reply::Locals(scopes) => Ok(scopes),
            other => unexpected(other),
        }
    }

    /// `locals`: variables of all scopes, flattened.
    pub async fn locals(&mut self) -> Result<Vec<Variable>> {
        Ok(self
            .scopes()
            .await?
            .into_iter()
            .flat_map(|s| s.variables)
            .collect())
    }

    /// A local variable by name.
    pub async fn local(&mut self, name: &str) -> Result<Variable> {
        self.locals()
            .await?
            .into_iter()
            .find(|v| v.name == name)
            .ok_or_else(|| anyhow!("no local variable `{name}`"))
    }

    /// `tests [filter]`
    pub async fn tests(&mut self, filter: Option<&str>) -> Result<Vec<TestCase>> {
        let query = TestQuery {
            filter: filter.map(str::to_owned),
        };
        match self
            .session
            .execute(Command::Tests(query), &mut |_| {})
            .await
        {
            Outcome::Tests(tests) => Ok(tests),
            Outcome::Error(e) => Err(anyhow!(e)),
            other => bail!("unexpected outcome {other:?}"),
        }
    }

    /// `test-run <test>`: returns the rendered result.
    pub async fn test_run(&mut self, test: &str) -> Result<String> {
        match self
            .session
            .execute(Command::TestRun(selector(test)), &mut |_| {})
            .await
        {
            Outcome::Text(t) => Ok(t),
            Outcome::Error(e) => Err(anyhow!(e)),
            other => bail!("unexpected outcome {other:?}"),
        }
    }

    /// `test-debug [-b] <test>`: debug a test and wait until it halts.
    pub async fn test_debug(&mut self, test: &str, break_at_start: bool) -> Result<Halt> {
        self.drain();
        let cmd = Command::TestDebug {
            test: selector(test),
            break_at_start,
        };
        if let Outcome::Error(e) = self.session.execute(cmd, &mut |_| {}).await {
            bail!(e);
        }
        self.wait().await
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    /// Wait until the program stops, exits or terminates.
    pub async fn wait(&mut self) -> Result<Halt> {
        let mut halt = self.engine().subscribe_halt();
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            self.drain();
            // Event consumers (output/transcript/MCP) may already have drained
            // the halt event. The engine retains it until the next resume.
            if let Some(ev) = halt.borrow_and_update().clone() {
                return Ok(match ev {
                    DebugEvent::SessionStopped(info) => Halt::Stopped(*info),
                    DebugEvent::SessionExited(code) => Halt::Exited(code),
                    DebugEvent::SessionTerminated => Halt::Terminated,
                    _ => unreachable!("engine publishes only halt events"),
                });
            }
            tokio::select! {
                result = halt.changed() => {
                    result.map_err(|_| anyhow!("the debug engine has shut down"))?;
                }
                ev = self.events.recv() => match ev {
                    Ok(ev) => self.record(&ev),
                    Err(RecvError::Lagged(_)) => {},
                    Err(RecvError::Closed) => bail!("the debug engine has shut down"),
                },
                _ = tokio::time::sleep_until(deadline) => {
                    // A concurrent halt at the deadline is not a running result.
                    if halt.borrow().is_none() {
                        return Err(Timeout(self.timeout).into());
                    }
                }
            }
        }
    }

    /// Wait for the next event matching `pred`. Other events are recorded
    /// (output, transcript) and skipped.
    pub async fn wait_for(&mut self, pred: impl Fn(&DebugEvent) -> bool) -> Result<DebugEvent> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            let ev = match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Err(_) => return Err(Timeout(self.timeout).into()),
                Ok(Err(RecvError::Closed)) => bail!("the debug engine has shut down"),
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Ok(ev)) => ev,
            };
            self.record(&ev);
            if pred(&ev) {
                return Ok(ev);
            }
        }
    }

    /// Process events that have already arrived.
    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(ev) => self.record(&ev),
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
    }

    fn record(&mut self, ev: &DebugEvent) {
        if let DebugEvent::Output(o) = ev {
            match o.category {
                OutputCategory::Stdout => self.stdout.push_str(&o.text),
                OutputCategory::Stderr => self.stderr.push_str(&o.text),
                _ => {}
            }
        }
        if let Some(text) = self.renderer.event(ev) {
            self.pending.push(text);
        }
    }

    fn take_pending(&mut self) -> String {
        std::mem::take(&mut self.pending).join("\n")
    }

    /// Text the REPL would have printed for events (stops, program output,
    /// exit) since the previous call to this method or [`Debugger::exec`].
    pub fn take_transcript(&mut self) -> String {
        self.drain();
        self.take_pending()
    }

    /// The REPL renderer, for presenting replies from [`Debugger::execute`].
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// Program stdout received so far.
    pub fn stdout(&mut self) -> &str {
        self.drain();
        &self.stdout
    }

    /// Program stderr received so far.
    pub fn stderr(&mut self) -> &str {
        self.drain();
        &self.stderr
    }
}

fn resumes(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Run(_)
            | Command::Continue
            | Command::Next
            | Command::Step
            | Command::Finish
            | Command::Pause
            | Command::TestDebug { .. }
    )
}

fn selector(test: &str) -> TestSelector {
    match test.parse() {
        Ok(n) => TestSelector::Index(n),
        Err(_) => TestSelector::Name(test.to_owned()),
    }
}

fn unexpected<T>(reply: Reply) -> Result<T> {
    bail!("unexpected reply {reply:?}")
}
