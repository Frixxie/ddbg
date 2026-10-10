//! TUI state and input handling.

use std::collections::HashMap;
use std::path::PathBuf;

use ddbg_cli::commands::Suggestion;
use ddbg_cli::render::Renderer;
use ddbg_core::EngineHandle;
use ddbg_core::breakpoint::{Breakpoint, BreakpointId};
use ddbg_core::command::{Command, ScopeVariables};
use ddbg_core::event::ExceptionInfo;
use ddbg_core::frame::StackFrame;
use ddbg_core::variable::{VarRef, Variable};
use ratatui::crossterm::event::{Event, KeyEventKind};

use crate::highlight::StyledLine;
use crate::picker::{FilePicker, FunctionPicker, ProgramPicker, TestPicker};
use crate::{Executor, Msg};

const MAX_LOG_LINES: usize = 5000;
const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Source,
    Stack,
    Locals,
    Log,
}

/// Inline editor for a local variable's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditVar {
    pub scope: VarRef,
    pub name: String,
    pub value: String,
}

/// Open completion list for the command line.
#[derive(Debug, Clone)]
pub struct CompletionList {
    /// Command line the completions were computed for.
    pub base: String,
    pub items: Vec<Suggestion>,
    pub index: usize,
}

impl CompletionList {
    /// `base` with the selected completion applied.
    fn applied(&self) -> String {
        let s = &self.items[self.index];
        let (start, end) = (
            s.span.start.min(self.base.len()),
            s.span.end.min(self.base.len()),
        );
        let mut line = format!("{}{}{}", &self.base[..start], s.value, &self.base[end..]);
        if s.append_whitespace {
            line.push(' ');
        }
        line
    }
}

mod command_line;
mod events;
mod keys;
mod pickers;
#[cfg(test)]
mod tests;
mod view;

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Source => Self::Stack,
            Self::Stack => Self::Locals,
            Self::Locals => Self::Log,
            Self::Log => Self::Source,
        }
    }
}

/// Debuggee run time between two consecutive breakpoint hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BpInterval {
    pub from: BreakpointId,
    pub to: BreakpointId,
    pub time: std::time::Duration,
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
    /// Syntax-highlighted `lines`; empty when the file could not be read.
    pub styled: Vec<StyledLine>,
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
    sources: HashMap<PathBuf, (Option<Vec<String>>, Vec<StyledLine>)>,
    /// Line the debuggee is stopped at (1-based), in `source`.
    pub exec_line: Option<u32>,
    /// Cursor in the source pane (1-based).
    pub cursor: u32,

    pub stack: Vec<StackFrame>,
    pub selected_frame: Option<usize>,
    /// Highlighted row in the stack pane.
    pub stack_cursor: usize,
    pub locals: Vec<ScopeVariables>,
    pub locals_cursor: usize,
    /// Scroll offset in the open variable value popup.
    pub value_scroll: Option<u16>,
    /// Expression being expanded in the value popup.
    value_expr: Option<String>,
    /// Members of the variable shown in the value popup.
    pub value_children: Vec<Variable>,
    /// Exception the debuggee is stopped on, if any.
    pub exception: Option<ExceptionInfo>,
    pub elapsed: Option<std::time::Duration>,
    /// Last breakpoint hit and debuggee run time accumulated since.
    last_hit: Option<(BreakpointId, std::time::Duration)>,
    /// Run time between the previous and the current breakpoint hit.
    pub bp_interval: Option<BpInterval>,
    pub breakpoints: Vec<Breakpoint>,
    pub watches: Vec<ddbg_core::watch::Watch>,

    pub log: Vec<String>,
    /// Lines scrolled up from the bottom of the log.
    pub log_scroll: usize,

    /// Command line contents while in command mode.
    pub input: Option<String>,
    /// Open completion list for `input`.
    pub completions: Option<CompletionList>,
    /// Inline value editor for a local variable.
    pub edit: Option<EditVar>,
    history: Vec<String>,
    history_pos: Option<usize>,

    /// Open test picker.
    pub picker: Option<TestPicker>,
    /// Open program picker.
    pub programs: Option<ProgramPicker>,
    /// Open function picker.
    pub functions: Option<FunctionPicker>,
    /// Open source file picker.
    pub files: Option<FilePicker>,
    /// Binaries found by project detection.
    pub candidates: Vec<PathBuf>,
    /// Program the next `run` launches, if known.
    pub program: Option<PathBuf>,
    pub attached_pid: Option<u32>,
}

impl App {
    pub fn new(
        exec: Executor,
        engine: EngineHandle,
        cwd: PathBuf,
        verbose: bool,
        candidates: Vec<PathBuf>,
        program: Option<PathBuf>,
    ) -> Self {
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
            locals_cursor: 0,
            value_scroll: None,
            value_expr: None,
            value_children: Vec::new(),
            exception: None,
            elapsed: None,
            last_hit: None,
            bp_interval: None,
            breakpoints: Vec::new(),
            watches: Vec::new(),
            log: Vec::new(),
            log_scroll: 0,
            input: None,
            completions: None,
            edit: None,
            history: Vec::new(),
            history_pos: None,
            picker: None,
            programs: None,
            functions: None,
            files: None,
            candidates,
            program,
            attached_pid: None,
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
            Msg::Functions(fns) => {
                if let Some(picker) = &mut self.functions {
                    picker.set_items(fns);
                }
            }
            Msg::Completions { line, items } => self.show_completions(line, items),
            Msg::EventsLagged(n) => self.log(format!("warning: dropped {n} debugger events")),
        }
    }
}
