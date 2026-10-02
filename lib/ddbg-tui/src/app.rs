//! TUI state and input handling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ddbg_cli::Outcome;
use ddbg_cli::commands::Suggestion;
use ddbg_cli::parser::{Input, parse};
use ddbg_cli::render::{Renderer, help};
use ddbg_cli::testing::render_list;
use ddbg_core::breakpoint::{Breakpoint, FunctionLocation, Location, SourceLocation, path_matches};
use ddbg_core::command::{Command, FrameSelector, Reply, ScopeVariables, TestQuery, TestSelector};
use ddbg_core::event::ExceptionInfo;
use ddbg_core::frame::StackFrame;
use ddbg_core::variable::{VarRef, Variable};
use ddbg_core::{DebugEvent, EngineHandle, LaunchTarget};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::functions::Function;
use crate::highlight::{self, StyledLine};
use crate::picker::{FilePicker, FunctionPicker, Program, ProgramPicker, TestPicker};
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

#[cfg(test)]
mod tests {
    use super::*;
    use ddbg_core::adapter::LldbDapAdapter;
    use ddbg_core::engine::{self, EngineConfig};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;
    use tokio::sync::{Mutex, mpsc};

    #[tokio::test]
    async fn select_and_inspect_locals_across_scopes() {
        let cwd = PathBuf::from(".");
        let engine = engine::spawn(EngineConfig {
            adapter: Arc::new(LldbDapAdapter::default()),
            cwd: cwd.clone(),
            target: None,
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let exec = Executor {
            session: Arc::new(Mutex::new(ddbg_cli::Session {
                engine: engine.clone(),
                cwd: cwd.clone(),
                candidates: Vec::new(),
                tests: ddbg_cli::testing::Tests::new(None, ""),
            })),
            tx,
        };
        let mut app = App::new(exec, engine, cwd, false, Vec::new(), None);
        let key = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
        app.apply_reply(Reply::Locals(
            (0..2)
                .map(|i| ScopeVariables {
                    scope: format!("scope {i}"),
                    reference: VarRef(i + 1),
                    variables: vec![Variable {
                        name: format!("var{i}"),
                        value: format!("first line\n{}\nlast line", "long value ".repeat(300)),
                        type_name: Some("String".into()),
                        children: None,
                    }],
                })
                .collect(),
        ));
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Locals);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected_local().unwrap().name, "var1");
        key(&mut app, KeyCode::Down);
        assert_eq!(app.locals_cursor, 1);
        key(&mut app, KeyCode::Enter);
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let screen = format!("{:?}", terminal.backend().buffer());
        assert!(screen.contains("var1: String"));
        assert!(screen.contains("first line"));
        key(&mut app, KeyCode::Char('G'));
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(app.value_scroll.unwrap() > 0);
        assert!(format!("{:?}", terminal.backend().buffer()).contains("last line"));
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.value_scroll, None);
        key(&mut app, KeyCode::Enter);
        app.debug_event(DebugEvent::SessionContinued);
        assert_eq!(app.value_scroll, None);
        assert!(app.selected_local().is_none());
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.value_scroll, None);
    }

    fn test_app() -> App {
        let cwd = PathBuf::from(".");
        let engine = engine::spawn(EngineConfig {
            adapter: Arc::new(LldbDapAdapter::default()),
            cwd: cwd.clone(),
            target: None,
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let exec = Executor {
            session: Arc::new(Mutex::new(ddbg_cli::Session {
                engine: engine.clone(),
                cwd: cwd.clone(),
                candidates: Vec::new(),
                tests: ddbg_cli::testing::Tests::new(None, ""),
            })),
            tx,
        };
        App::new(exec, engine, cwd, false, Vec::new(), None)
    }

    #[tokio::test]
    async fn edit_local_value() {
        let mut app = test_app();
        let key = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
        app.apply_reply(Reply::Locals(vec![ScopeVariables {
            scope: "Locals".into(),
            reference: VarRef(7),
            variables: vec![Variable {
                name: "x".into(),
                value: "1".into(),
                type_name: None,
                children: None,
            }],
        }]));
        app.focus = Focus::Locals;
        key(&mut app, KeyCode::Char('='));
        assert_eq!(app.edit.as_ref().unwrap().value, "1");
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Char('4'));
        key(&mut app, KeyCode::Char('2'));
        assert_eq!(
            app.edit_command(),
            Some(Command::SetVariable {
                scope: VarRef(7),
                name: "x".into(),
                value: "42".into(),
            })
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.edit.is_none());
    }

    #[tokio::test]
    async fn command_line_completion_applies_and_cycles() {
        use ddbg_cli::commands::Span;
        let mut app = test_app();
        let key = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
        key(&mut app, KeyCode::Char(':'));
        for c in "p po".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        let item = |v: &str| Suggestion {
            value: v.into(),
            span: Span::new(2, 4),
            ..Default::default()
        };
        app.handle(Msg::Completions {
            line: "p po".into(),
            items: vec![item("point")],
        });
        assert_eq!(app.input.as_deref(), Some("p point"));
        assert!(app.completions.is_none());

        app.input = Some("p po".into());
        app.handle(Msg::Completions {
            line: "p po".into(),
            items: vec![item("point"), item("pos")],
        });
        assert_eq!(app.input.as_deref(), Some("p point"));
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.input.as_deref(), Some("p pos"));
        key(&mut app, KeyCode::Char('x'));
        assert!(app.completions.is_none());
        assert_eq!(app.input.as_deref(), Some("p posx"));
        // Stale results for an edited line are ignored.
        app.handle(Msg::Completions {
            line: "p po".into(),
            items: vec![item("point")],
        });
        assert_eq!(app.input.as_deref(), Some("p posx"));
    }
}

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
    /// Exception the debuggee is stopped on, if any.
    pub exception: Option<ExceptionInfo>,
    pub breakpoints: Vec<Breakpoint>,

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
            exception: None,
            breakpoints: Vec::new(),
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
        self.locals_cursor = 0;
        self.value_scroll = None;
        self.exception = None;
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
            Msg::Functions(fns) => {
                if let Some(picker) = &mut self.functions {
                    picker.set_items(fns);
                }
            }
            Msg::Completions { line, items } => self.show_completions(line, items),
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
                self.exception = info.exception.clone();
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
            DebugEvent::FrameChanged => {
                self.locals.clear();
                self.locals_cursor = 0;
                self.value_scroll = None;
                self.refresh_stopped();
            }
            DebugEvent::VariablesChanged => self.execute_silent(Command::Locals),
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
            Outcome::Tests(tests) => match &mut self.picker {
                Some(picker) if picker.items.is_none() => picker.set_items(tests),
                _ if silent => {}
                _ => self.log(render_list(&tests)),
            },
            Outcome::Text(text) => self.log(text),
            // Background refreshes fail routinely (e.g. not stopped).
            Outcome::Error(_) if silent => {}
            Outcome::Error(e) => {
                // A failed discovery should not leave the picker loading.
                if self.picker.as_ref().is_some_and(|p| p.items.is_none()) {
                    self.picker = None;
                }
                self.log(format!("error: {e}"));
            }
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
            Reply::Locals(scopes) => {
                self.locals = scopes;
                self.locals_cursor = self.locals_cursor.min(self.locals_len().saturating_sub(1));
                self.value_scroll = None;
            }
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
        let (lines, styled) = self
            .sources
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                let lines: Option<Vec<String>> = std::fs::read_to_string(path)
                    .ok()
                    .map(|s| s.lines().map(|l| l.replace('\t', "    ")).collect());
                let styled = lines
                    .as_deref()
                    .map(|l| highlight::highlight(path, l))
                    .unwrap_or_default();
                (lines, styled)
            })
            .clone();
        self.source = Some(SourceView {
            path: path.to_path_buf(),
            lines,
            styled,
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

    fn locals_len(&self) -> usize {
        self.locals.iter().map(|s| s.variables.len()).sum()
    }

    pub fn selected_local(&self) -> Option<&Variable> {
        self.locals
            .iter()
            .flat_map(|s| &s.variables)
            .nth(self.locals_cursor)
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
            Focus::Locals => {
                let max = self.locals_len().saturating_sub(1) as i64;
                self.locals_cursor = (self.locals_cursor as i64 + delta).clamp(0, max) as usize;
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
        if self.edit.is_some() {
            self.edit_key(key);
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
        if self.picker.is_some() {
            self.picker_key(key);
            return;
        }
        if self.programs.is_some() {
            self.program_key(key);
            return;
        }
        if self.functions.is_some() {
            self.function_key(key);
            return;
        }
        if self.files.is_some() {
            self.file_key(key);
            return;
        }
        if let Some(scroll) = &mut self.value_scroll {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.value_scroll = None,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                KeyCode::PageDown => *scroll = scroll.saturating_add(20),
                KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
                KeyCode::End | KeyCode::Char('G') => *scroll = u16::MAX,
                _ => {}
            }
            return;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('q') => self.execute(Command::Quit),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('t') => self.open_picker(),
            KeyCode::Char('e') => self.open_program_picker(),
            KeyCode::Char('F') => self.open_function_picker(),
            KeyCode::Char('o') => self.open_file_picker(),
            KeyCode::Char(':') => {
                self.input = Some(String::new());
                self.history_pos = None;
            }
            KeyCode::Tab => self.focus = self.focus.next(),

            // Nothing to run yet: let the user choose first.
            KeyCode::Char('r') if self.program.is_none() && !self.candidates.is_empty() => {
                self.open_program_picker()
            }
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
            KeyCode::Enter if self.focus == Focus::Locals && self.selected_local().is_some() => {
                self.value_scroll = Some(0);
            }
            KeyCode::Char('=') if self.focus == Focus::Locals => self.start_edit(),
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

    pub fn open_program_picker(&mut self) {
        if self.candidates.is_empty() {
            self.log("error: no programs detected; use `:run <program>`");
            return;
        }
        let programs = self
            .candidates
            .iter()
            .map(|p| Program::new(p.clone(), &self.cwd))
            .collect();
        let mut picker = ProgramPicker::with_items(programs);
        // Start on the current program so Enter restarts it.
        if let Some(i) = self
            .program
            .as_ref()
            .and_then(|cur| self.candidates.iter().position(|c| c == cur))
        {
            picker.cursor = i;
        }
        self.programs = Some(picker);
    }

    /// Keys while the program picker is open.
    fn program_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.programs.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let stop_on_entry = match key.code {
            KeyCode::Esc => {
                self.programs = None;
                return;
            }
            KeyCode::Up => return picker.move_cursor(-1),
            KeyCode::Down => return picker.move_cursor(1),
            KeyCode::PageUp => return picker.move_cursor(-10),
            KeyCode::PageDown => return picker.move_cursor(10),
            KeyCode::Char('p') if ctrl => return picker.move_cursor(-1),
            KeyCode::Char('n') if ctrl => return picker.move_cursor(1),
            KeyCode::Backspace => return picker.pop(),
            KeyCode::Enter => false,
            KeyCode::Char('b') if ctrl => true,
            KeyCode::Char(c) if !ctrl => return picker.push(c),
            _ => return,
        };
        let Some(path) = picker.selected().map(|p| p.path.clone()) else {
            return;
        };
        self.programs = None;
        self.log_scroll = 0;
        let mut target = LaunchTarget::new(path.clone(), Vec::new());
        target.stop_on_entry = stop_on_entry;
        self.program = Some(path);
        self.execute(Command::Run(Some(target)));
    }

    fn open_function_picker(&mut self) {
        self.functions = Some(FunctionPicker::default());
        // Rescan on every open so the list reflects current sources.
        self.exec.discover_functions(self.cwd.clone());
    }

    /// Function breakpoint location for `f`, scoped to its file so that
    /// same-named functions elsewhere do not stop.
    fn function_location(&self, f: &Function) -> FunctionLocation {
        let file = f.path.strip_prefix(&self.cwd).unwrap_or(&f.path);
        FunctionLocation::new(f.name.clone(), Some(file.to_path_buf()))
    }

    /// Existing breakpoint on function `f`, if any.
    pub fn function_breakpoint(&self, f: &Function) -> Option<&Breakpoint> {
        let loc = Location::Function(self.function_location(f));
        self.breakpoints.iter().find(|bp| bp.requested == loc)
    }

    /// Keys while the function picker is open.
    fn function_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.functions.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.functions = None,
            KeyCode::Up => picker.move_cursor(-1),
            KeyCode::Down => picker.move_cursor(1),
            KeyCode::PageUp => picker.move_cursor(-10),
            KeyCode::PageDown => picker.move_cursor(10),
            KeyCode::Char('p') if ctrl => picker.move_cursor(-1),
            KeyCode::Char('n') if ctrl => picker.move_cursor(1),
            KeyCode::Backspace => picker.pop(),
            // Toggle a breakpoint; stay open to pick several.
            KeyCode::Enter => {
                let Some(f) = picker.selected().cloned() else {
                    return;
                };
                let cmd = match self.function_breakpoint(&f) {
                    Some(bp) => Command::DeleteBreakpoint(bp.id),
                    None => Command::Break(Location::Function(self.function_location(&f))),
                };
                self.log_scroll = 0;
                self.execute(cmd);
            }
            // Show the declaration in the source pane.
            KeyCode::Char('o') if ctrl => {
                let Some(f) = picker.selected().cloned() else {
                    return;
                };
                self.functions = None;
                self.open_source(&f.path);
                self.cursor = f.line.max(1);
                self.focus = Focus::Source;
            }
            KeyCode::Char(c) if !ctrl => picker.push(c),
            _ => {}
        }
    }

    fn open_file_picker(&mut self) {
        let files = ddbg_cli::functions::source_files(&self.cwd)
            .into_iter()
            .map(|p| Program::new(p, &self.cwd))
            .collect();
        self.files = Some(FilePicker::with_items(files));
    }

    /// Keys while the source file picker is open.
    fn file_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.files.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.files = None,
            KeyCode::Up => picker.move_cursor(-1),
            KeyCode::Down => picker.move_cursor(1),
            KeyCode::PageUp => picker.move_cursor(-10),
            KeyCode::PageDown => picker.move_cursor(10),
            KeyCode::Char('p') if ctrl => picker.move_cursor(-1),
            KeyCode::Char('n') if ctrl => picker.move_cursor(1),
            KeyCode::Backspace => picker.pop(),
            KeyCode::Enter => {
                let Some(path) = picker.selected().map(|f| f.path.clone()) else {
                    return;
                };
                self.files = None;
                self.open_source(&path);
                self.cursor = 1;
                self.focus = Focus::Source;
            }
            KeyCode::Char(c) if !ctrl => picker.push(c),
            _ => {}
        }
    }

    fn open_picker(&mut self) {
        self.picker = Some(TestPicker::default());
        // Discover on every open so the list reflects current sources.
        self.execute(Command::Tests(TestQuery::default()));
    }

    /// Keys while the test picker is open. Typing filters; Ctrl chords act.
    fn picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let act = |p: &TestPicker, f: fn(TestSelector) -> Command| {
            p.selected().map(|t| f(TestSelector::Name(t.name.clone())))
        };
        let cmd = match key.code {
            KeyCode::Esc => {
                self.picker = None;
                return;
            }
            KeyCode::Up => return picker.move_cursor(-1),
            KeyCode::Down => return picker.move_cursor(1),
            KeyCode::PageUp => return picker.move_cursor(-10),
            KeyCode::PageDown => return picker.move_cursor(10),
            KeyCode::Char('p') if ctrl => return picker.move_cursor(-1),
            KeyCode::Char('n') if ctrl => return picker.move_cursor(1),
            KeyCode::Backspace => return picker.pop(),
            KeyCode::Enter => act(picker, |test| Command::TestDebug {
                test,
                break_at_start: false,
            }),
            KeyCode::Char('b') if ctrl => act(picker, |test| Command::TestDebug {
                test,
                break_at_start: true,
            }),
            KeyCode::Char('r') if ctrl => act(picker, Command::TestRun),
            KeyCode::Char(c) if !ctrl => return picker.push(c),
            _ => return,
        };
        if let Some(cmd) = cmd {
            self.picker = None;
            self.log_scroll = 0;
            self.execute(cmd);
        }
    }

    fn command_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            let back = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            match &mut self.completions {
                Some(list) => {
                    let n = list.items.len();
                    list.index = if back {
                        (list.index + n - 1) % n
                    } else {
                        (list.index + 1) % n
                    };
                    self.input = Some(list.applied());
                }
                None => {
                    let line = self.input.clone().unwrap_or_default();
                    self.exec.complete(&self.engine, line);
                }
            }
            return;
        }
        self.completions = None;
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

    /// Apply completion results if the command line has not changed since.
    fn show_completions(&mut self, line: String, items: Vec<Suggestion>) {
        if self.input.as_deref() != Some(line.as_str()) || items.is_empty() {
            return;
        }
        let list = CompletionList {
            base: line,
            items,
            index: 0,
        };
        self.input = Some(list.applied());
        self.completions = (list.items.len() > 1).then_some(list);
    }

    fn start_edit(&mut self) {
        let Some(v) = self.selected_local().cloned() else {
            return;
        };
        let Some(scope) = self
            .locals
            .iter()
            .scan(0, |n, s| {
                *n += s.variables.len();
                Some((*n, s.reference))
            })
            .find(|(end, _)| self.locals_cursor < *end)
            .map(|(_, r)| r)
        else {
            return;
        };
        self.edit = Some(EditVar {
            scope,
            name: v.name,
            value: v.value,
        });
    }

    pub fn edit_command(&self) -> Option<Command> {
        let e = self.edit.as_ref()?;
        Some(Command::SetVariable {
            scope: e.scope,
            name: e.name.clone(),
            value: e.value.trim().to_owned(),
        })
    }

    fn edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.edit = None,
            KeyCode::Enter => {
                let Some(cmd) = self.edit_command() else {
                    return;
                };
                let name = self.edit.take().map(|e| e.name).unwrap_or_default();
                if let Command::SetVariable { value, .. } = &cmd {
                    self.log(format!("ddbg> set {name} = {value}"));
                }
                self.log_scroll = 0;
                self.execute(cmd);
            }
            KeyCode::Backspace => {
                edit.value.pop();
            }
            KeyCode::Char(c) => edit.value.push(c),
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
            Ok(Input::Command(cmd)) => {
                if let Command::Run(Some(t)) = &cmd {
                    self.program = Some(t.program.clone());
                }
                self.execute(cmd)
            }
            Err(e) => self.log(format!("error: {e}")),
        }
    }
}
