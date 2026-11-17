//! Test, program, function and file pickers.

use ddbg_core::LaunchTarget;
use ddbg_core::breakpoint::{Breakpoint, FunctionLocation, Location};
use ddbg_core::command::{Command, TestQuery, TestSelector};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Focus};
use crate::functions::Function;
use crate::picker::{FilePicker, FunctionPicker, Program, ProgramPicker, TestPicker};

impl App {
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
    pub(super) fn program_key(&mut self, key: KeyEvent) {
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

    pub(super) fn open_function_picker(&mut self) {
        self.functions = Some(FunctionPicker::default());
        // Rescan on every open so the list reflects current sources.
        self.exec.discover_functions(self.cwd.clone());
    }

    /// Function breakpoint location for `f`, scoped to its file so that
    /// same-named functions elsewhere do not stop.
    pub(super) fn function_location(&self, f: &Function) -> FunctionLocation {
        let file = f.path.strip_prefix(&self.cwd).unwrap_or(&f.path);
        FunctionLocation::new(f.name.clone(), Some(file.to_path_buf()))
    }

    /// Existing breakpoint on function `f`, if any.
    pub fn function_breakpoint(&self, f: &Function) -> Option<&Breakpoint> {
        let loc = Location::Function(self.function_location(f));
        self.breakpoints.iter().find(|bp| bp.requested == loc)
    }

    /// Keys while the function picker is open.
    pub(super) fn function_key(&mut self, key: KeyEvent) {
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

    pub(super) fn open_file_picker(&mut self) {
        let files = ddbg_cli::functions::source_files(&self.cwd)
            .into_iter()
            .map(|p| Program::new(p, &self.cwd))
            .collect();
        self.files = Some(FilePicker::with_items(files));
    }

    /// Keys while the source file picker is open.
    pub(super) fn file_key(&mut self, key: KeyEvent) {
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

    pub(super) fn open_picker(&mut self) {
        self.picker = Some(TestPicker::default());
        // Discover on every open so the list reflects current sources.
        self.execute(Command::Tests(TestQuery::default()));
    }

    /// Keys while the test picker is open. Typing filters; Ctrl chords act.
    pub(super) fn picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let act = |p: &TestPicker, f: fn(TestSelector) -> Command| {
            p.selected()
                .map(|t| f(TestSelector::Name(t.name().to_owned())))
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
}
