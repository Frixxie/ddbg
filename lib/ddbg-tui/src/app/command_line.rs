//! Command line, completion, history and the value editor.

use ddbg_cli::commands::Suggestion;
use ddbg_cli::parser::{Input, parse};
use ddbg_cli::render::help;
use ddbg_core::command::Command;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, CompletionList, EditVar, MAX_HISTORY};

impl App {
    pub(super) fn command_key(&mut self, key: KeyEvent) {
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
    pub(super) fn show_completions(&mut self, line: String, items: Vec<Suggestion>) {
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

    pub(super) fn start_edit(&mut self) {
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

    pub(super) fn edit_key(&mut self, key: KeyEvent) {
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

    pub(super) fn history_step(&mut self, delta: i64) {
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

    pub(super) fn submit(&mut self, line: String) {
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
