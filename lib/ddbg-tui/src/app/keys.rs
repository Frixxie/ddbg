//! Top-level key dispatch and keys outside of overlays.

use ddbg_core::command::{Command, FrameSelector};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Focus};

impl App {
    pub(super) fn key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.exec.pause(&self.engine);
            return;
        }
        if self.overlay_key(key) {
            return;
        }
        self.normal_key(key);
    }

    /// Route `key` to the open overlay, if any. Returns whether it was consumed.
    fn overlay_key(&mut self, key: KeyEvent) -> bool {
        if self.edit.is_some() {
            self.edit_key(key);
        } else if self.input.is_some() {
            self.command_key(key);
        } else if self.show_help {
            self.show_help = false;
        } else if self.picker.is_some() {
            self.picker_key(key);
        } else if self.programs.is_some() {
            self.program_key(key);
        } else if self.functions.is_some() {
            self.function_key(key);
        } else if self.files.is_some() {
            self.file_key(key);
        } else if self.value_scroll.is_some() {
            self.value_popup_key(key);
        } else {
            return false;
        }
        true
    }

    /// Keys while the variable value popup is open.
    fn value_popup_key(&mut self, key: KeyEvent) {
        let Some(scroll) = &mut self.value_scroll else {
            return;
        };
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
    }

    fn normal_key(&mut self, key: KeyEvent) {
        if self.ui_key(key) || self.debug_key(key) {
            return;
        }
        self.navigation_key(key);
    }

    /// Panels, pickers and the command line. Returns whether `key` was handled.
    fn ui_key(&mut self, key: KeyEvent) -> bool {
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
            _ => return false,
        }
        true
    }

    /// Execution control. Returns whether `key` was handled.
    fn debug_key(&mut self, key: KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Nothing to run yet: let the user choose first.
            KeyCode::Char('r')
                if self.program.is_none()
                    && self.attached_pid.is_none()
                    && !self.candidates.is_empty() =>
            {
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
            _ => return false,
        }
        true
    }

    /// Cursor movement and actions on the focused pane.
    fn navigation_key(&mut self, key: KeyEvent) {
        match key.code {
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
                self.open_value_popup();
            }
            KeyCode::Char('=') if self.focus == Focus::Locals => self.start_edit(),
            KeyCode::Char('.') => self.jump_to_exec(),
            _ => {}
        }
    }

    fn open_value_popup(&mut self) {
        self.value_scroll = Some(0);
        self.value_children.clear();
        self.value_expr = self
            .selected_local()
            .filter(|v| v.children.is_some())
            .map(|v| v.evaluate_name.clone().unwrap_or_else(|| v.name.clone()));
        if let Some(expr) = self.value_expr.clone() {
            self.execute_silent(Command::Print(expr));
        }
    }

    /// Jump back to the execution point.
    fn jump_to_exec(&mut self) {
        if let Some(frame) = self.selected_frame.and_then(|i| self.stack.get(i)).cloned() {
            self.source = None;
            self.show_frame(&frame);
        }
    }
}
