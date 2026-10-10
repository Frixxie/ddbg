//! Debugger events and command outcomes.

use ddbg_cli::Outcome;
use ddbg_cli::render::elapsed_suffix;
use ddbg_cli::testing::render_list;
use ddbg_core::DebugEvent;
use ddbg_core::command::{Command, Reply};
use ddbg_core::session::StopReason;

use super::{App, BpInterval, Status};

impl App {
    pub(super) fn debug_event(&mut self, event: DebugEvent) {
        if let Some(text) = self.renderer.event(&event) {
            self.log(text);
        }
        match &event {
            DebugEvent::SessionStarted => {
                self.status = Status::Running;
                self.last_hit = None;
                self.bp_interval = None;
                self.clear_stopped();
            }
            DebugEvent::SessionContinued => {
                self.status = Status::Running;
                self.clear_stopped();
            }
            DebugEvent::SessionStopped(info) => {
                self.watches = info.watches.clone();
                self.status = Status::Stopped;
                self.exception = info.exception.clone();
                self.elapsed = info.elapsed;
                self.track_breakpoint_interval(&info.reason, info.elapsed);
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
                for w in &mut self.watches {
                    w.result = None;
                }
                self.locals.clear();
                self.locals_cursor = 0;
                self.value_scroll = None;
                self.refresh_stopped();
            }
            DebugEvent::VariablesChanged => self.execute_silent(Command::Locals),
            DebugEvent::WatchesChanged(watches) => self.watches = watches.clone(),
            DebugEvent::BreakpointChanged(_) => self.refresh_breakpoints(),
            DebugEvent::ThreadsChanged | DebugEvent::Output(_) => {}
        }
    }

    /// Sum run time across stops so that stepping between two breakpoints
    /// does not reset the measurement or count time spent paused.
    pub(super) fn track_breakpoint_interval(
        &mut self,
        reason: &StopReason,
        elapsed: Option<std::time::Duration>,
    ) {
        if let Some((_, total)) = &mut self.last_hit {
            *total += elapsed.unwrap_or_default();
        }
        let StopReason::Breakpoint(hit) = reason else {
            return;
        };
        let Some(&to) = hit.first() else {
            return;
        };
        self.bp_interval = self
            .last_hit
            .map(|(from, time)| BpInterval { from, to, time });
        if let Some(i) = self.bp_interval {
            self.log(format!(
                "breakpoint {} → {}{}",
                i.from,
                i.to,
                elapsed_suffix(Some(i.time))
            ));
        }
        self.last_hit = Some((to, std::time::Duration::ZERO));
    }

    pub(super) fn outcome(&mut self, outcome: Outcome, silent: bool) {
        match outcome {
            Outcome::Quit => self.should_quit = true,
            Outcome::Reply(cmd, reply) => {
                if !silent && let Some(text) = self.renderer.reply(&cmd, &reply) {
                    self.log(text);
                }
                if let (Command::Print(expr), Reply::Value(_, children)) = (&cmd, &reply)
                    && self.value_scroll.is_some()
                    && self.value_expr.as_ref() == Some(expr)
                {
                    self.value_children = children.clone();
                }
                // The engine relaunches its last target (e.g. a debugged
                // test) on `r`, so don't fall back to the program picker.
                match &reply {
                    Reply::Launched(path) => {
                        self.program = Some(path.clone());
                        self.attached_pid = None;
                    }
                    Reply::Attached(pid) => {
                        self.program = None;
                        self.attached_pid = Some(*pid);
                    }
                    _ => {}
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

    pub(super) fn apply_reply(&mut self, reply: Reply) {
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
            Reply::Watches(watches) => self.watches = watches,
            Reply::WatchSet { watch, .. } => {
                if let Some(w) = self.watches.iter_mut().find(|w| w.id == watch.id) {
                    *w = watch;
                } else {
                    self.watches.push(watch);
                }
            }
            Reply::WatchDeleted(id) => self.watches.retain(|w| w.id != id),
            Reply::BreakpointSet { .. } | Reply::BreakpointDeleted(_) => self.refresh_breakpoints(),
            _ => {}
        }
    }
}
