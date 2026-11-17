//! Backtrace view that hides frames without local source.

use std::path::Path;

use ddbg_core::frame::StackFrame;

use crate::dto::FrameDto;

/// Frames worth showing, plus a summary that marks where frames were hidden.
#[derive(Debug)]
pub(crate) struct LocalBacktrace {
    pub summary: String,
    pub shown: Vec<FrameDto>,
    pub hidden: usize,
}

/// Keep frames with local source and the selected frame; collapse runs of
/// other frames into a single summary line.
pub(crate) fn local_backtrace(
    frames: Vec<StackFrame>,
    selected: Option<usize>,
    has_source: impl Fn(&Path) -> bool,
) -> LocalBacktrace {
    let total = frames.len();
    let mut summary = String::new();
    let mut shown = Vec::new();
    let mut hidden_run = 0;
    for (i, f) in frames.into_iter().enumerate() {
        let is_selected = Some(i) == selected;
        if !is_selected && !f.path.as_deref().is_some_and(&has_source) {
            hidden_run += 1;
            continue;
        }
        push_hidden_run(&mut summary, &mut hidden_run);
        let marker = if is_selected { '*' } else { ' ' };
        let location = match &f.path {
            Some(path) => format!(" at {}:{}", path.display(), f.line),
            None => String::new(),
        };
        summary.push_str(&format!("{marker}#{i} {}{location}\n", f.name));
        shown.push(FrameDto::new(Some(i), f));
    }
    push_hidden_run(&mut summary, &mut hidden_run);
    let hidden = total - shown.len();
    if hidden > 0 {
        summary.push_str(&format!(
            "{hidden} of {total} frames without local source hidden; pass `all: true` to show them."
        ));
    }
    LocalBacktrace {
        summary,
        shown,
        hidden,
    }
}

fn push_hidden_run(summary: &mut String, run: &mut usize) {
    if *run > 0 {
        summary.push_str(&format!("   ... {run} frames without local source\n"));
        *run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddbg_core::frame::FrameId;

    fn frame(name: &str, path: Option<&str>) -> StackFrame {
        StackFrame {
            id: FrameId(0),
            name: name.into(),
            path: path.map(Into::into),
            source_name: None,
            line: 7,
            column: 1,
        }
    }

    fn local(p: &Path) -> bool {
        p.starts_with("/src")
    }

    fn indices(bt: &LocalBacktrace) -> Vec<Option<usize>> {
        bt.shown.iter().map(|f| f.index).collect()
    }

    #[test]
    fn all_local_frames_are_shown_without_notes() {
        let frames = vec![frame("a", Some("/src/a.rs")), frame("b", Some("/src/b.rs"))];
        let bt = local_backtrace(frames, Some(0), local);
        assert_eq!(indices(&bt), [Some(0), Some(1)]);
        assert_eq!(bt.hidden, 0);
        assert_eq!(bt.summary, "*#0 a at /src/a.rs:7\n #1 b at /src/b.rs:7\n");
    }

    #[test]
    fn runs_of_foreign_frames_collapse_into_one_line() {
        let frames = vec![
            frame("a", Some("/src/a.rs")),
            frame("libc", Some("/usr/lib/x.c")),
            frame("anon", None),
            frame("main", Some("/src/main.rs")),
            frame("start", None),
        ];
        let bt = local_backtrace(frames, Some(0), local);
        assert_eq!(indices(&bt), [Some(0), Some(3)]);
        assert_eq!(bt.hidden, 3);
        assert_eq!(
            bt.summary,
            "*#0 a at /src/a.rs:7\n\
             \x20  ... 2 frames without local source\n\
             \x20#3 main at /src/main.rs:7\n\
             \x20  ... 1 frames without local source\n\
             3 of 5 frames without local source hidden; pass `all: true` to show them."
        );
    }

    #[test]
    fn selected_frame_is_shown_even_without_source() {
        let frames = vec![frame("a", Some("/src/a.rs")), frame("anon", None)];
        let bt = local_backtrace(frames, Some(1), local);
        assert_eq!(indices(&bt), [Some(0), Some(1)]);
        assert_eq!(bt.hidden, 0);
        assert_eq!(bt.summary, " #0 a at /src/a.rs:7\n*#1 anon\n");
    }

    #[test]
    fn shown_and_hidden_account_for_every_frame() {
        let paths = [
            None,
            Some("/src/a.rs"),
            Some("/lib/b.c"),
            None,
            Some("/src/c.rs"),
        ];
        for selected in [None, Some(0), Some(2), Some(4)] {
            let frames = paths.iter().map(|p| frame("f", *p)).collect();
            let bt = local_backtrace(frames, selected, local);
            assert_eq!(bt.shown.len() + bt.hidden, paths.len(), "{selected:?}");
        }
    }
}
