//! Source view, breakpoints and pane cursors.

use std::path::Path;

use ddbg_core::breakpoint::{Breakpoint, Location, SourceLocation, path_matches};
use ddbg_core::command::Command;
use ddbg_core::frame::StackFrame;
use ddbg_core::variable::Variable;

use super::{App, Focus, SourceView};
use crate::highlight;

impl App {
    /// Re-read state that is only valid while stopped.
    pub(super) fn refresh_stopped(&mut self) {
        self.execute_silent(Command::Backtrace);
        self.execute_silent(Command::Locals);
    }

    pub(super) fn clear_stopped(&mut self) {
        for w in &mut self.watches {
            w.result = None;
        }
        self.stack.clear();
        self.selected_frame = None;
        self.locals.clear();
        self.locals_cursor = 0;
        self.value_scroll = None;
        self.exception = None;
        self.exec_line = None;
    }

    pub(super) fn show_frame(&mut self, frame: &StackFrame) {
        let Some(path) = &frame.path else {
            self.source = None;
            self.exec_line = None;
            return;
        };
        self.open_source(path);
        self.exec_line = Some(frame.line);
        self.cursor = frame.line.max(1);
    }

    pub(super) fn open_source(&mut self, path: &Path) {
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

    pub(super) fn toggle_breakpoint(&mut self) {
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

    pub(super) fn source_len(&self) -> u32 {
        self.source
            .as_ref()
            .and_then(|s| s.lines.as_ref())
            .map_or(1, |l| l.len().max(1) as u32)
    }

    pub(super) fn locals_len(&self) -> usize {
        self.locals.iter().map(|s| s.variables.len()).sum()
    }

    pub fn selected_local(&self) -> Option<&Variable> {
        self.locals
            .iter()
            .flat_map(|s| &s.variables)
            .nth(self.locals_cursor)
    }

    pub(super) fn move_cursor(&mut self, delta: i64) {
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
}
