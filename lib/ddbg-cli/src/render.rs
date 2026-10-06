//! Formatting of replies and events for the terminal.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ddbg_core::DebugEvent;
use ddbg_core::breakpoint::{Breakpoint, Location, SourceLocation};
use ddbg_core::command::{Command, Reply};
use ddbg_core::event::{ExceptionInfo, OutputCategory, StopInfo};
use ddbg_core::frame::StackFrame;
use ddbg_core::session::StopReason;
use ddbg_core::variable::Variable;

use crate::parser::COMMANDS;
use ddbg_core::watch::Watch;

/// Number of source lines shown before and after the current line.
const SOURCE_CONTEXT: usize = 2;

pub struct Renderer {
    cwd: PathBuf,
    sources: HashMap<PathBuf, Option<Vec<String>>>,
    highlighted: HashMap<PathBuf, Vec<String>>,
    last_function: Option<String>,
    exited: bool,
    /// Partial output lines per category.
    pending_output: HashMap<u8, String>,
    /// Show adapter console messages (debugger chatter).
    pub show_console: bool,
    /// Syntax-highlight source listings with ANSI escapes.
    pub color: bool,
}

impl Renderer {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            sources: HashMap::new(),
            highlighted: HashMap::new(),
            last_function: None,
            exited: false,
            pending_output: HashMap::new(),
            show_console: false,
            color: false,
        }
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.cwd)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// Renders the current line with `SOURCE_CONTEXT` lines before and after.
    fn source_line(&mut self, path: &Path, line: u32) -> Option<String> {
        let lines = self
            .sources
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                std::fs::read_to_string(path)
                    .ok()
                    .map(|s| s.lines().map(str::to_owned).collect())
            })
            .as_ref()?;
        let current = line.checked_sub(1)? as usize;
        lines.get(current)?;
        let start = current.saturating_sub(SOURCE_CONTEXT);
        let end = (current + SOURCE_CONTEXT + 1).min(lines.len());
        let width = end.to_string().len();
        let lines = if self.color {
            &*self
                .highlighted
                .entry(path.to_path_buf())
                .or_insert_with(|| crate::highlight::highlight(path, lines))
        } else {
            lines
        };
        let out: Vec<String> = (start..end)
            .map(|i| {
                let marker = if i == current { '>' } else { ' ' };
                let text = lines[i].trim_end();
                if self.color {
                    let gutter = format!("{:>width$} {marker}", i + 1);
                    let gutter = if i == current {
                        format!("\x1b[1;33m{gutter}\x1b[0m")
                    } else {
                        format!("\x1b[2m{gutter}\x1b[0m")
                    };
                    format!("{gutter} {text}")
                } else {
                    format!("{:>width$} {marker} {text}", i + 1)
                        .trim_end()
                        .to_owned()
                }
            })
            .collect();
        Some(out.join("\n"))
    }

    fn frame_location(&self, frame: &StackFrame) -> String {
        match (&frame.path, &frame.source_name) {
            (Some(p), _) => format!("{}:{}", self.display_path(p), frame.line),
            (None, Some(n)) => format!("{n}:{}", frame.line),
            (None, None) => "<no source>".into(),
        }
    }

    fn frame_with_source(&mut self, frame: &StackFrame, out: &mut String) {
        if let Some(path) = frame.path.clone()
            && let Some(line) = self.source_line(&path, frame.line)
        {
            let _ = writeln!(out, "\n{line}");
        }
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    pub fn event(&mut self, event: &DebugEvent) -> Option<String> {
        match event {
            DebugEvent::SessionStarted => {
                self.exited = false;
                self.last_function = None;
                None
            }
            DebugEvent::SessionStopped(info) => Some(self.stopped(info)),
            DebugEvent::SessionContinued => None,
            DebugEvent::WatchesChanged(watches) => {
                let lines: Vec<_> = watches
                    .iter()
                    .filter(|w| w.result.is_some())
                    .map(watch)
                    .collect();
                (!lines.is_empty()).then(|| lines.join("\n"))
            }
            DebugEvent::SessionExited(code) => {
                self.exited = true;
                let mut out = self.flush_output();
                if *code == 0 {
                    out.push_str("Process exited normally.");
                } else {
                    let _ = write!(out, "Process exited with code {code}.");
                }
                Some(out)
            }
            DebugEvent::SessionTerminated => {
                let mut out = self.flush_output();
                if !std::mem::replace(&mut self.exited, false) {
                    out.push_str("Debug session ended.");
                }
                (!out.is_empty()).then_some(out)
            }
            DebugEvent::BreakpointChanged(bp) if bp.verified => Some(format!(
                "Breakpoint {} resolved at {}",
                bp.id,
                self.bp_location(bp)
            )),
            DebugEvent::Output(o)
                if o.category == OutputCategory::Console && !self.show_console =>
            {
                None
            }
            DebugEvent::Output(o) => self.output(o.category, &o.text),
            _ => None,
        }
    }

    fn stopped(&mut self, info: &StopInfo) -> String {
        let mut out = self.flush_output();
        let function = info.frame.as_ref().map(|f| f.name.clone());
        let same_function = function.is_some() && function == self.last_function;
        self.last_function = function;

        let header = match &info.reason {
            StopReason::Breakpoint(ids) if !ids.is_empty() => {
                let ids: Vec<_> = ids.iter().map(|i| i.to_string()).collect();
                Some(format!("Breakpoint {}", ids.join(", ")))
            }
            StopReason::Breakpoint(_) => Some("Breakpoint".into()),
            StopReason::Step if same_function => None,
            StopReason::Step => Some("Stepped".into()),
            StopReason::Pause => Some("Paused".into()),
            StopReason::Entry => Some("Stopped at entry".into()),
            StopReason::Exception(text) => Some(format!(
                "Exception{}",
                text.as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default()
            )),
            StopReason::Other(r) => Some(format!("Stopped ({r})")),
        };

        let mut lines = Vec::new();
        match &info.frame {
            Some(frame) => {
                if let Some(h) = header {
                    lines.push(format!(
                        "{h}, {} at {}{}",
                        frame.name,
                        self.frame_location(frame),
                        elapsed_suffix(info.elapsed)
                    ));
                    if let StopReason::Exception(_) = &info.reason {
                        match &info.exception {
                            Some(ex) => exception_lines(ex, &mut lines),
                            None => lines.extend(info.description.clone()),
                        }
                    }
                }
                let src = frame
                    .path
                    .clone()
                    .and_then(|p| self.source_line(&p, frame.line));
                match src {
                    Some(src) => {
                        if !lines.is_empty() {
                            lines.push(String::new());
                        }
                        lines.push(src);
                    }
                    None if lines.is_empty() => {
                        lines.push(format!("{} at {}", frame.name, self.frame_location(frame)));
                    }
                    None => {}
                }
            }
            None => lines.push(format!(
                "{}{}",
                header.unwrap_or_else(|| "Stopped".into()),
                elapsed_suffix(info.elapsed)
            )),
        }
        if !info.watches.is_empty() {
            lines.push(String::new());
            lines.extend(info.watches.iter().map(watch));
        }
        out.push_str(&lines.join("\n"));
        out.trim_end().to_owned()
    }

    fn output(&mut self, category: OutputCategory, text: &str) -> Option<String> {
        let key = category as u8;
        let buf = self.pending_output.entry(key).or_default();
        buf.push_str(text);
        let end = buf.rfind('\n')?;
        let complete: String = buf.drain(..=end).collect();
        Some(complete.trim_end_matches('\n').to_owned())
    }

    /// Emit any partial output lines.
    fn flush_output(&mut self) -> String {
        let mut out = String::new();
        for buf in self.pending_output.values_mut() {
            if !buf.is_empty() {
                out.push_str(buf);
                out.push('\n');
                buf.clear();
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // Replies
    // -----------------------------------------------------------------------

    pub fn reply(&mut self, command: &Command, reply: &Reply) -> Option<String> {
        match reply {
            Reply::Ok => match command {
                Command::Continue => Some("Continuing.".into()),
                Command::Detach => Some("Detached; the process is left running.".into()),
                _ => None,
            },
            Reply::Launched(program) => {
                Some(format!("Starting program: {}", self.display_path(program)))
            }
            Reply::Attached(pid) => Some(format!("Attached to process {pid}.")),
            Reply::BreakpointSet { breakpoint, new } => {
                let mut s = format!(
                    "Breakpoint {} {} {}",
                    breakpoint.id,
                    if *new || matches!(command, Command::Condition { .. }) {
                        "at"
                    } else {
                        "already set at"
                    },
                    self.bp_location(breakpoint)
                );
                if matches!(
                    command,
                    Command::Condition {
                        condition: None,
                        ..
                    }
                ) {
                    s.push_str(" (condition cleared)");
                }
                if let Some(m) = &breakpoint.message {
                    let _ = write!(s, " ({m})");
                }
                Some(s)
            }
            Reply::BreakpointDeleted(id) => Some(format!("Deleted breakpoint {id}")),
            Reply::WatchSet { watch: w, new } => Some(format!(
                "{}{}",
                watch(w),
                if *new { "" } else { " (already set)" }
            )),
            Reply::WatchDeleted(id) => Some(format!("Deleted watch {id}")),
            Reply::Watches(watches) => Some(if watches.is_empty() {
                "No watches.".into()
            } else {
                watches.iter().map(watch).collect::<Vec<_>>().join("\n")
            }),
            Reply::Breakpoints(bps) if bps.is_empty() => Some("No breakpoints.".into()),
            Reply::Breakpoints(bps) => Some(
                bps.iter()
                    .map(|bp| {
                        format!(
                            "{:<3} {} {}",
                            bp.id,
                            self.bp_location(bp),
                            if bp.verified { "" } else { "(pending)" }
                        )
                        .trim_end()
                        .to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Reply::Backtrace { frames, selected } => Some(
                frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let marker = if Some(i) == *selected { '>' } else { ' ' };
                        format!("{marker}#{i:<3} {} at {}", f.name, self.frame_location(f))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Reply::Threads { threads, selected } => Some(
                threads
                    .iter()
                    .map(|t| {
                        let marker = if Some(t.id) == *selected { '*' } else { ' ' };
                        format!("{marker} {:<6} {}", t.id, t.name)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Reply::Frame { index, frame } => {
                let mut s = format!("#{index} {} at {}", frame.name, self.frame_location(frame));
                self.frame_with_source(frame, &mut s);
                Some(s)
            }
            Reply::Value(eval, children) => {
                let mut s = eval.value.clone();
                if !children.is_empty() && needs_expansion(&eval.value, eval.type_name.as_deref()) {
                    s.push_str(" {\n");
                    for v in children {
                        let _ = writeln!(s, "    {}", variable(v));
                    }
                    s.push('}');
                }
                Some(s)
            }
            Reply::Locals(scopes) => {
                let multiple = scopes.len() > 1;
                let mut s = String::new();
                for scope in scopes {
                    if multiple {
                        let _ = writeln!(s, "{}:", scope.scope);
                    }
                    if scope.variables.is_empty() {
                        s.push_str("No locals.\n");
                    }
                    for v in &scope.variables {
                        let _ = writeln!(s, "{}", variable(v));
                    }
                }
                Some(s.trim_end().to_owned())
            }
            Reply::Completions(items) => Some(
                items
                    .iter()
                    .map(|c| c.label.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Reply::Quit => None,
        }
    }

    fn bp_location(&self, bp: &Breakpoint) -> String {
        let line = |loc: &SourceLocation| format!("{}:{}", self.display_path(&loc.path), loc.line);
        let mut location = match (&bp.requested, &bp.resolved) {
            (_, Some(resolved)) if !bp.is_function() => line(resolved),
            (Location::Source(requested), _) => line(requested),
            (Location::Function(f), Some(resolved)) => format!("{f} ({})", line(resolved)),
            (Location::Function(f), None) => f.to_string(),
        };
        if let Some(condition) = &bp.condition {
            let _ = write!(location, " if {condition}");
        }
        location
    }
}

/// Whether a value's summary is uninformative enough that its children
/// should be shown (e.g. `Foo @ 0x1234`, `{...}`, `{MyApp.Foo}`).
/// `Type: message`, then inner exceptions in .NET `ToString()` style.
pub fn exception_lines(ex: &ExceptionInfo, lines: &mut Vec<String>) {
    fn summary(ex: &ExceptionInfo) -> String {
        let ty = ex.type_name.as_deref().unwrap_or(&ex.id);
        match ex.message.as_deref().or(ex.description.as_deref()) {
            Some(m) if !ty.is_empty() => format!("{ty}: {m}"),
            Some(m) => m.to_owned(),
            None => ty.to_owned(),
        }
    }
    fn inner(ex: &ExceptionInfo, depth: usize, lines: &mut Vec<String>) {
        for i in &ex.inner {
            lines.push(format!("{} ---> {}", " ".repeat(depth), summary(i)));
            inner(i, depth + 1, lines);
        }
    }
    let s = summary(ex);
    if !s.is_empty() {
        lines.push(s);
    }
    inner(ex, 0, lines);
}

fn needs_expansion(value: &str, type_name: Option<&str>) -> bool {
    let v = value.trim();
    v.is_empty()
        || v.contains(" @ 0x")
        || v.ends_with("{...}")
        || type_name.is_some_and(|t| v == t || v == format!("{{{t}}}"))
}

fn variable(v: &Variable) -> String {
    format!("{} = {}", v.name, v.value)
}

pub fn watch(w: &Watch) -> String {
    let value = match &w.result {
        Some(Ok(v)) => v.value.clone(),
        Some(Err(e)) => format!("<error: {e}>"),
        None => "<unavailable>".into(),
    };
    format!("Watch {}: {} = {value}", w.id, w.expression)
}

pub fn help(topic: Option<&str>) -> String {
    if let Some(topic) = topic {
        return match crate::parser::lookup(topic) {
            Some(c) => {
                let aliases = if c.aliases.is_empty() {
                    String::new()
                } else {
                    format!("\naliases: {}", c.aliases.join(", "))
                };
                format!("{}\n  {}{aliases}", c.usage, c.help)
            }
            None => format!("unknown command `{topic}`"),
        };
    }
    let mut s = String::from("Commands:\n");
    for c in COMMANDS {
        let aliases = if c.aliases.is_empty() {
            String::new()
        } else {
            format!(" ({})", c.aliases.join(", "))
        };
        let _ = writeln!(s, "  {:<28} {}{aliases}", c.usage, c.help);
    }
    s.trim_end().to_owned()
}

/// ` (+12.3ms)`: time the debuggee ran before this stop.
pub fn elapsed_suffix(elapsed: Option<Duration>) -> String {
    let Some(d) = elapsed else {
        return String::new();
    };
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!(" (+{ms:.1}ms)")
    } else {
        format!(" (+{:.2}s)", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddbg_core::frame::FrameId;

    fn frame(path: &Path, line: u32, name: &str) -> StackFrame {
        StackFrame {
            id: FrameId(1),
            name: name.into(),
            path: Some(path.to_path_buf()),
            source_name: None,
            line,
            column: 1,
        }
    }

    #[test]
    fn breakpoint_stop_shows_header_and_source() {
        let dir = std::env::temp_dir().join("ddbg-render-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();

        let mut r = Renderer::new(dir.clone());
        let out = r
            .event(&DebugEvent::SessionStopped(Box::new(StopInfo {
                reason: StopReason::Breakpoint(vec![ddbg_core::breakpoint::BreakpointId(1)]),
                thread: None,
                description: None,
                frame: Some(frame(&file, 2, "app::main")),
                exception: None,
                elapsed: Some(Duration::from_micros(12_345)),
                watches: Vec::new(),
            })))
            .unwrap();
        assert_eq!(
            out,
            "Breakpoint 1, app::main at main.rs:2 (+12.3ms)\n\n1   fn main() {\n2 >     let x = 1;\n3   }"
        );

        // stepping within the same function only shows the line
        let out = r
            .event(&DebugEvent::SessionStopped(Box::new(StopInfo {
                reason: StopReason::Step,
                thread: None,
                description: None,
                frame: Some(frame(&file, 3, "app::main")),
                exception: None,
                elapsed: None,
                watches: Vec::new(),
            })))
            .unwrap();
        assert_eq!(out, "1   fn main() {\n2       let x = 1;\n3 > }");
    }

    #[test]
    fn exception_stop_shows_type_message_and_inner() {
        let mut r = Renderer::new(PathBuf::from("/src"));
        let ex = |ty: &str, msg: &str, inner: Vec<ExceptionInfo>| ExceptionInfo {
            id: ty.into(),
            type_name: Some(ty.into()),
            message: Some(msg.into()),
            inner,
            ..Default::default()
        };
        let out = r
            .event(&DebugEvent::SessionStopped(Box::new(StopInfo {
                reason: StopReason::Exception(Some("Unhandled".into())),
                thread: None,
                description: Some("Outer failed".into()),
                frame: Some(frame(Path::new("/src/missing.cs"), 7, "App.Main")),
                exception: Some(ex(
                    "System.InvalidOperationException",
                    "Outer failed",
                    vec![ex(
                        "System.ArgumentException",
                        "Bad arg",
                        vec![ex("System.FormatException", "Bad format", vec![])],
                    )],
                )),
                elapsed: None,
                watches: Vec::new(),
            })))
            .unwrap();
        assert_eq!(
            out,
            "Exception: Unhandled, App.Main at missing.cs:7\n\
             System.InvalidOperationException: Outer failed\n\
             \x20---> System.ArgumentException: Bad arg\n\
             \x20 ---> System.FormatException: Bad format"
        );
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(elapsed_suffix(None), "");
        assert_eq!(
            elapsed_suffix(Some(Duration::from_micros(450))),
            " (+0.5ms)"
        );
        assert_eq!(
            elapsed_suffix(Some(Duration::from_millis(2_500))),
            " (+2.50s)"
        );
    }

    #[test]
    fn expansion_heuristic() {
        assert!(needs_expansion("Point @ 0x1000", None));
        assert!(needs_expansion("{MyApp.User}", Some("MyApp.User")));
        assert!(!needs_expansion("\"hello\"", Some("String")));
        assert!(!needs_expansion("{x:3, y:4}", Some("Point")));
    }

    #[test]
    fn breakpoint_replies_show_conditions_and_edits() {
        use ddbg_core::breakpoint::{BreakpointId, BreakpointStore};
        let mut store = BreakpointStore::default();
        let (id, _) = store.add(
            Location::Source(SourceLocation::new("main.rs", 42)),
            Path::new("/src"),
        );
        store.set_condition(id, Some("count > 10".into()));
        let bp = store.get(id).unwrap().clone();
        let mut r = Renderer::new("/src".into());
        let edit = Command::Condition {
            id: BreakpointId(1),
            condition: bp.condition.clone(),
        };
        assert_eq!(
            r.reply(
                &edit,
                &Reply::BreakpointSet {
                    breakpoint: bp.clone(),
                    new: false
                }
            )
            .unwrap(),
            "Breakpoint 1 at main.rs:42 if count > 10"
        );
        assert!(
            r.reply(&Command::Breakpoints, &Reply::Breakpoints(vec![bp]))
                .unwrap()
                .contains("main.rs:42 if count > 10")
        );
        store.set_condition(id, None);
        assert_eq!(
            r.reply(
                &Command::Condition {
                    id,
                    condition: None
                },
                &Reply::BreakpointSet {
                    breakpoint: store.get(id).unwrap().clone(),
                    new: false
                }
            )
            .unwrap(),
            "Breakpoint 1 at main.rs:42 (condition cleared)"
        );
    }

    #[test]
    fn output_is_line_buffered() {
        let mut r = Renderer::new(PathBuf::new());
        assert_eq!(r.output(OutputCategory::Stdout, "hel"), None);
        assert_eq!(
            r.output(OutputCategory::Stdout, "lo\nwor"),
            Some("hello".into())
        );
        assert_eq!(
            r.event(&DebugEvent::SessionExited(0)).unwrap(),
            "wor\nProcess exited normally."
        );
        assert_eq!(r.event(&DebugEvent::SessionTerminated), None);
    }

    #[test]
    fn watch_values_errors_and_unavailable_states_are_rendered() {
        use ddbg_core::watch::{WatchId, WatchValue};
        let mut w = Watch {
            id: WatchId(1),
            expression: "x".into(),
            result: None,
        };
        assert_eq!(watch(&w), "Watch 1: x = <unavailable>");
        let mut r = Renderer::new("/src".into());
        assert!(
            r.event(&DebugEvent::WatchesChanged(vec![w.clone()]))
                .is_none()
        );
        w.result = Some(Ok(WatchValue {
            value: "42".into(),
            type_name: Some("int".into()),
            has_children: false,
        }));
        let out = r
            .event(&DebugEvent::SessionStopped(Box::new(StopInfo {
                reason: StopReason::Pause,
                thread: None,
                description: None,
                frame: None,
                exception: None,
                elapsed: None,
                watches: vec![w.clone()],
            })))
            .unwrap();
        assert_eq!(out, "Paused\n\nWatch 1: x = 42");
        assert_eq!(
            r.reply(&Command::Watches, &Reply::Watches(vec![w.clone()]))
                .unwrap(),
            "Watch 1: x = 42"
        );
        w.result = Some(Err("out of scope".into()));
        assert_eq!(watch(&w), "Watch 1: x = <error: out of scope>");
    }
}
