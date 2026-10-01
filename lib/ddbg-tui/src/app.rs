//! TUI state and input handling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ddbg_cli::Outcome;
use ddbg_cli::parser::{Input, parse};
use ddbg_cli::render::{Renderer, help};
use ddbg_core::breakpoint::{Breakpoint, Location, SourceLocation, path_matches};
use ddbg_core::command::{Command, FrameSelector, Reply, ScopeVariables};
use ddbg_core::frame::StackFrame;
use ddbg_core::{DebugEvent, EngineHandle};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{Executor, Msg};

const MAX_LOG_LINES: usize = 5000;
const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Source,
    Stack,
    Log,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Source => Self::Stack,
            Self::Stack => Self::Log,
            Self::Log => Self::Source,
        }
    }
}

/// Coarse status for the title bar, derived from events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Running,
    Stopped,
    Exited(i64),
    Terminated,
}

pub struct SourceView {
    pub path: PathBuf,
    /// `None` when the file could not be read.
    pub lines: Option<Vec<String>>,
}

pub struct App {
    exec: Executor,
    engine: EngineHandle,
    pub cwd: PathBuf,
    renderer: Renderer,

    pub status: Status,
    /// Number of commands in flight.
    pub busy: usize,
    pub should_quit: bool,
    pub focus: Focus,
    pub show_help: bool,

    pub source: Option<SourceView>,
    sources: HashMap<PathBuf, Option<Vec<String>>>,
    /// Line the debuggee is stopped at (1-based), in `source`.
    pub exec_line: Option<u32>,
    /// Cursor in the source pane (1-based).
    pub cursor: u32,

    pub stack: Vec<StackFrame>,
    pub selected_frame: Option<usize>,
    /// Highlighted row in the stack pane.
    pub stack_cursor: usize,
    pub locals: Vec<ScopeVariables>,
    pub breakpoints: Vec<Breakpoint>,

    pub log: Vec<String>,
    /// Lines scrolled up from the bottom of the log.
    pub log_scroll: usize,

    /// Command line contents while in command mode.
    pub input: Option<String>,
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl App {
    pub fn new(exec: Executor, engine: EngineHandle, cwd: PathBuf, verbose: bool) -> Self {
        let mut renderer = Renderer::new(cwd.clone());
        renderer.show_console = verbose;
        Self {
            exec,
            engine,
            cwd,
            renderer,
            status: Status::Idle,
            busy: 0,
            should_quit: false,
            focus: Focus::Source,
            show_help: false,
            source: None,
            sources: HashMap::new(),
            exec_line: None,
            cursor: 1,
            stack: Vec::new(),
            selected_frame: None,
            stack_cursor: 0,
            locals: Vec::new(),
            breakpoints: Vec::new(),
            log: Vec::new(),
            log_scroll: 0,
            input: None,
            history: Vec::new(),
            history_pos: None,
        }
    }

    pub fn log(&mut self, text: impl AsRef<str>) {
        self.log
            .extend(text.as_ref().lines().map(|l| l.replace('\t', "    ")));
        let excess = self.log.len().saturating_sub(MAX_LOG_LINES);
        self.log.drain(..excess);
    }

    pub fn execute(&mut self, cmd: Command) {
        self.busy += 1;
        self.exec.spawn(cmd, false);
    }

    fn execute_silent(&mut self, cmd: Command) {
        self.busy += 1;
        self.exec.spawn(cmd, true);
    }

    pub fn refresh_breakpoints(&mut self) {
        self.execute_silent(Command::Breakpoints);
    }

    /// Re-read state that is only valid while stopped.
    fn refresh_stopped(&mut self) {
        self.execute_silent(Command::Backtrace);
        self.execute_silent(Command::Locals);
    }

    fn clear_stopped(&mut self) {
        self.stack.clear();
        self.selected_frame = None;
        self.locals.clear();
        self.exec_line = None;
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => self.key(key),
            Msg::Input(_) => {}
            Msg::Debug(event) => self.debug_event(event),
            Msg::Done { outcome, silent } => {
                self.busy = self.busy.saturating_sub(1);
                self.outcome(outcome, silent);
            }
            Msg::Progress(text) => self.log(text),
            Msg::EventsLagged(n) => self.log(format!("warning: dropped {n} debugger events")),
        }
    }

    fn debug_event(&mut self, event: DebugEvent) {
        if let Some(text) = self.renderer.event(&event) {
            self.log(text);
        }
        match &event {
            DebugEvent::SessionStarted | DebugEvent::SessionContinued => {
                self.status = Status::Running;
                self.clear_stopped();
            }
            DebugEvent::SessionStopped(info) => {
                self.status = Status::Stopped;
                if let Some(frame) = &info.frame {
                    self.show_frame(frame);
                }
                self.refresh_stopped();
            }
            DebugEvent::SessionExited(code) => {
                self.status = Status::Exited(*code);
                self.clear_stopped();
            }
            DebugEvent::SessionTerminated => {
                if !matches!(self.status, Status::Exited(_)) {
                    self.status = Status::Terminated;
                }
                self.clear_stopped();
            }
            DebugEvent::FrameChanged => self.refresh_stopped(),
            DebugEvent::BreakpointChanged(_) => self.refresh_breakpoints(),
            DebugEvent::ThreadsChanged | DebugEvent::Output(_) => {}
        }
    }

    fn outcome(&mut self, outcome: Outcome, silent: bool) {
        match outcome {
            Outcome::Quit => self.should_quit = true,
            Outcome::Reply(cmd, reply) => {
                if !silent && let Some(text) = self.renderer.reply(&cmd, &reply) {
                    self.log(text);
                }
                self.apply_reply(reply);
            }
            Outcome::Text(text) => self.log(text),
            // Background refreshes fail routinely (e.g. not stopped).
            Outcome::Error(_) if silent => {}
            Outcome::Error(e) => self.log(format!("error: {e}")),
        }
    }

    fn apply_reply(&mut self, reply: Reply) {
        match reply {
            Reply::Backtrace { frames, selected } => {
                self.stack = frames;
                self.selected_frame = selected;
                self.stack_cursor = selected.unwrap_or(0);
                if let Some(frame) = selected.and_then(|i| self.stack.get(i)).cloned() {
                    self.show_frame(&frame);
                }
            }
            Reply::Frame { index, frame } => {
                self.selected_frame = Some(index);
                self.stack_cursor = index;
                self.show_frame(&frame);
                self.execute_silent(Command::Locals);
            }
            Reply::Locals(scopes) => self.locals = scopes,
            Reply::Breakpoints(bps) => self.breakpoints = bps,
            Reply::BreakpointSet { .. } | Reply::BreakpointDeleted(_) => self.refresh_breakpoints(),
            _ => {}
        }
    }

    fn show_frame(&mut self, frame: &StackFrame) {
        let Some(path) = &frame.path else {
            self.source = None;
            self.exec_line = None;
            return;
        };
        self.open_source(path);
        self.exec_line = Some(frame.line);
        self.cursor = frame.line.max(1);
    }

    fn open_source(&mut self, path: &Path) {
        if self.source.as_ref().is_some_and(|s| s.path == path) {
            return;
        }
        let lines = self
            .sources
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                std::fs::read_to_string(path)
                    .ok()
                    .map(|s| s.lines().map(|l| l.replace('\t', "    ")).collect())
            })
            .clone();
        self.source = Some(SourceView {
            path: path.to_path_buf(),
            lines,
        });
    }

    /// Breakpoint on `line` of the open source file, if any.
    pub fn breakpoint_at(&self, line: u32) -> Option<&Breakpoint> {
        let path = &self.source.as_ref()?.path;
        self.breakpoints.iter().find(|bp| {
            let loc = match (&bp.resolved, &bp.requested) {
                (Some(r), _) => r,
                (None, Location::Source(s)) => s,
                (None, Location::Function(_)) => return false,
            };
            loc.line == line && (loc.path == *path || path_matches(path, &loc.path))
        })
    }

    fn toggle_breakpoint(&mut self) {
        let Some(source) = &self.source else {
            self.log("error: no source file open");
            return;
        };
        let cmd = match self.breakpoint_at(self.cursor) {
            Some(bp) => Command::DeleteBreakpoint(bp.id),
            None => Command::Break(Location::Source(SourceLocation::new(
                source.path.clone(),
                self.cursor,
            ))),
        };
        self.execute(cmd);
    }

    fn source_len(&self) -> u32 {
        self.source
            .as_ref()
            .and_then(|s| s.lines.as_ref())
            .map_or(1, |l| l.len().max(1) as u32)
    }

    fn move_cursor(&mut self, delta: i64) {
        match self.focus {
            Focus::Source => {
                let max = self.source_len() as i64;
                self.cursor = (self.cursor as i64 + delta).clamp(1, max) as u32;
            }
            Focus::Stack => {
                let max = self.stack.len().saturating_sub(1) as i64;
                self.stack_cursor = (self.stack_cursor as i64 + delta).clamp(0, max) as usize;
            }
            Focus::Log => {
                let max = self.log.len().saturating_sub(1) as i64;
                self.log_scroll = (self.log_scroll as i64 - delta).clamp(0, max) as usize;
            }
        }
    }

    fn key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.exec.pause(&self.engine);
            return;
        }
        if self.input.is_some() {
            self.command_key(key);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('q') => self.execute(Command::Quit),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(':') => {
                self.input = Some(String::new());
                self.history_pos = None;
            }
            KeyCode::Tab => self.focus = self.focus.next(),

            KeyCode::Char('r') => self.execute(Command::Run(None)),
            KeyCode::Char('c') | KeyCode::F(5) => self.execute(Command::Continue),
            KeyCode::Char('p') => self.exec.pause(&self.engine),
            KeyCode::Char('K') => self.execute(Command::Kill),
            KeyCode::Char('n') | KeyCode::F(10) => self.execute(Command::Next),
            KeyCode::F(11) if shift => self.execute(Command::Finish),
            KeyCode::Char('s') | KeyCode::F(11) => self.execute(Command::Step),
            KeyCode::Char('f') => self.execute(Command::Finish),
            KeyCode::Char('b') | KeyCode::F(9) => self.toggle_breakpoint(),
            KeyCode::Char('u') => self.execute(Command::Frame(FrameSelector::Up)),
            KeyCode::Char('d') => self.execute(Command::Frame(FrameSelector::Down)),

            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-20),
            KeyCode::PageDown => self.move_cursor(20),
            KeyCode::Char('g') => self.move_cursor(i64::MIN / 2),
            KeyCode::Char('G') => self.move_cursor(i64::MAX / 2),
            KeyCode::Enter if self.focus == Focus::Stack && !self.stack.is_empty() => {
                self.execute(Command::Frame(FrameSelector::Index(self.stack_cursor)));
            }
            KeyCode::Char('.') => {
                // Jump back to the execution point.
                if let Some(frame) = self.selected_frame.and_then(|i| self.stack.get(i)).cloned() {
                    self.source = None;
                    self.show_frame(&frame);
                }
            }
            _ => {}
        }
    }

    fn command_key(&mut self, key: KeyEvent) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => {
                let line = self.input.take().unwrap_or_default();
                self.submit(line);
            }
            KeyCode::Backspace => {
                if input.pop().is_none() {
                    self.input = None;
                }
            }
            KeyCode::Up => self.history_step(-1),
            KeyCode::Down => self.history_step(1),
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
    }

    fn history_step(&mut self, delta: i64) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() as i64 - 1;
        let pos = match self.history_pos {
            None if delta < 0 => last,
            None => return,
            Some(p) => p as i64 + delta,
        };
        if pos > last {
            self.history_pos = None;
            self.input = Some(String::new());
            return;
        }
        let pos = pos.clamp(0, last) as usize;
        self.history_pos = Some(pos);
        self.input = Some(self.history[pos].clone());
    }

    fn submit(&mut self, line: String) {
        if !line.trim().is_empty() && self.history.last() != Some(&line) {
            self.history.push(line.clone());
            let excess = self.history.len().saturating_sub(MAX_HISTORY);
            self.history.drain(..excess);
        }
        self.log(format!("ddbg> {line}"));
        self.log_scroll = 0;
        match parse(&line) {
            Ok(Input::Empty) => {}
            Ok(Input::Help(topic)) => self.log(help(topic.as_deref())),
            Ok(Input::Command(cmd)) => self.execute(cmd),
            Err(e) => self.log(format!("error: {e}")),
        }
    }
}
