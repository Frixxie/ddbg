//! Interactive REPL built on reedline.
//!
//! reedline's `read_line` blocks, so it runs on a dedicated thread and
//! hands lines to the async side. All output (command results and async
//! debugger events) goes through reedline's external printer so it never
//! corrupts the prompt.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};

use ddbg_core::command::Command;
use ddbg_core::{EngineHandle, Reply};
use reedline::{
    ColumnarMenu, Emacs, ExternalPrinter, FileBackedHistory, KeyCode, KeyModifiers, MenuBuilder,
    Prompt, PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, Signal,
    default_emacs_keybindings,
};
use tokio::sync::{broadcast, mpsc};

use crate::commands::DdbgCompleter;
use crate::parser::{Input, parse};
use crate::render::{Renderer, help};
use crate::testing::Tests;

const HISTORY_SIZE: usize = 1000;

enum LineEvent {
    Line(String),
    Interrupt,
    Eof,
}

struct DdbgPrompt;

impl Prompt for DdbgPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("ddbg")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }
    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Owned(format!("(search: {}) ", search.term))
    }
}

fn history_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "ddbg")?;
    let dir = dirs.data_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("history.txt"))
}

fn build_editor(printer: ExternalPrinter<String>) -> Reedline {
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".into()),
            ReedlineEvent::MenuNext,
        ]),
    );
    let menu = ColumnarMenu::default().with_name("completion_menu");

    let mut editor = Reedline::create()
        .with_completer(Box::new(DdbgCompleter))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_external_printer(printer);
    if let Some(history) =
        history_path().and_then(|p| FileBackedHistory::with_file(HISTORY_SIZE, p).ok())
    {
        editor = editor.with_history(Box::new(history));
    }
    editor
}

/// Run the interactive loop until `quit` or Ctrl-D.
pub async fn run(
    engine: EngineHandle,
    cwd: PathBuf,
    initial: Vec<Command>,
    verbose: bool,
    candidates: Vec<PathBuf>,
    tests: Tests,
) -> anyhow::Result<()> {
    let printer = ExternalPrinter::<String>::new(4096);
    let out = printer.sender();
    let mut renderer = Renderer::new(cwd.clone());
    renderer.show_console = verbose;
    let renderer = Arc::new(Mutex::new(renderer));

    // Async debugger events → printer.
    let mut events = engine.subscribe();
    let event_out = out.clone();
    let event_renderer = renderer.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let text = event_renderer.lock().unwrap().event(&event);
                    if let Some(text) = text {
                        let _ = event_out.send(text);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let _ = event_out.send(format!("warning: dropped {n} debugger events"));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let _ = out.send(format!(
        "ddbg {}. Type `help` for a list of commands.",
        env!("CARGO_PKG_VERSION")
    ));

    // Line editor thread. Waits for an ack after each line so that command
    // output is queued before the next prompt is drawn.
    let (line_tx, mut line_rx) = mpsc::channel::<LineEvent>(1);
    let (ack_tx, ack_rx) = std_mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut editor = build_editor(printer);
        let prompt = DdbgPrompt;
        loop {
            let event = match editor.read_line(&prompt) {
                Ok(Signal::Success(line)) => LineEvent::Line(line),
                Ok(Signal::CtrlC) => LineEvent::Interrupt,
                Ok(Signal::CtrlD) => LineEvent::Eof,
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("error: {e}");
                    LineEvent::Eof
                }
            };
            if line_tx.blocking_send(event).is_err() || ack_rx.recv().is_err() {
                break;
            }
        }
    });

    let mut repl = Repl {
        engine,
        renderer,
        out,
        last_repeatable: None,
        cwd,
        candidates,
        tests,
    };
    for cmd in initial {
        repl.execute(cmd).await;
    }

    while let Some(event) = line_rx.recv().await {
        let quit = match event {
            LineEvent::Line(line) => repl.line(&line).await,
            LineEvent::Interrupt => {
                // Ctrl-C interrupts a running program; otherwise it just
                // clears the line.
                let _ = repl.engine.execute(Command::Pause).await;
                false
            }
            LineEvent::Eof => {
                repl.execute(Command::Quit).await;
                true
            }
        };
        if quit {
            break;
        }
        let _ = ack_tx.send(());
    }
    Ok(())
}

struct Repl {
    engine: EngineHandle,
    renderer: Arc<Mutex<Renderer>>,
    out: std_mpsc::SyncSender<String>,
    /// Command repeated by an empty line (like gdb).
    last_repeatable: Option<Command>,
    cwd: PathBuf,
    /// Binaries found by project detection, used to resolve `run <name>`.
    candidates: Vec<PathBuf>,
    tests: Tests,
}

impl Repl {
    fn print(&self, text: String) {
        let _ = self.out.send(text);
    }

    /// Returns `true` when the REPL should exit.
    async fn line(&mut self, line: &str) -> bool {
        match parse(line) {
            Ok(Input::Empty) => match self.last_repeatable.clone() {
                Some(cmd) => self.execute(cmd).await,
                None => false,
            },
            Ok(Input::Help(topic)) => {
                self.print(help(topic.as_deref()));
                false
            }
            Ok(Input::Command(cmd)) => {
                self.last_repeatable = matches!(
                    cmd,
                    Command::Next | Command::Step | Command::Finish | Command::Continue
                )
                .then(|| cmd.clone());
                self.execute(cmd).await
            }
            Err(e) => {
                self.print(format!("error: {e}"));
                false
            }
        }
    }

    async fn execute(&mut self, mut cmd: Command) -> bool {
        // Test targets are complete; only user-typed programs need resolving.
        let from_test = matches!(cmd, Command::TestDebug(_));
        // Test commands are handled by the frontend; only the resulting
        // debug target reaches the engine.
        let result = match &cmd {
            Command::Tests(q) => {
                self.print("discovering tests...".into());
                Some(self.tests.list(q).await)
            }
            Command::TestRun(sel) => {
                self.print("building tests...".into());
                Some(self.tests.run(sel).await)
            }
            Command::TestDebug(sel) => {
                self.print("building tests...".into());
                match self.tests.debug_target(sel).await {
                    Ok(t) => cmd = Command::Run(Some(t)),
                    Err(e) => self.print(format!("error: {e}")),
                }
                if !matches!(cmd, Command::Run(_)) {
                    return false;
                }
                None
            }
            _ => None,
        };
        if let Some(result) = result {
            match result {
                Ok(text) => self.print(text),
                Err(e) => self.print(format!("error: {e:#}")),
            }
            return false;
        }
        if !from_test && let Command::Run(Some(t)) = &mut cmd {
            if let Some(p) = resolve_program(&t.program, &self.cwd, &self.candidates) {
                t.program = p;
            }
            crate::apply_dotnet_launch(t);
        }
        match self.engine.execute(cmd.clone()).await {
            Ok(Reply::Quit) => true,
            Ok(reply) => {
                if let Some(text) = self.renderer.lock().unwrap().reply(&cmd, &reply) {
                    self.print(text);
                }
                false
            }
            Err(e) => {
                self.print(format!("error: {e}"));
                false
            }
        }
    }
}

/// Resolve a bare program name (e.g. `App.dll`, `App` or `my-bin`) against
/// detected project binaries when it does not exist relative to `cwd`.
pub(crate) fn resolve_program(
    program: &Path,
    cwd: &Path,
    candidates: &[PathBuf],
) -> Option<PathBuf> {
    if program.components().count() != 1 || cwd.join(program).exists() {
        return None;
    }
    candidates
        .iter()
        .find(|c| c.file_name() == Some(program.as_os_str()))
        .or_else(|| {
            candidates
                .iter()
                .find(|c| c.file_stem() == Some(program.as_os_str()))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bare_names_against_candidates() {
        let cands = vec![
            PathBuf::from("/p/A/bin/Debug/net8.0/My.A.dll"),
            PathBuf::from("/p/target/debug/tool"),
        ];
        let cwd = Path::new("/nonexistent-ddbg-cwd");
        let r = |s: &str| resolve_program(Path::new(s), cwd, &cands);
        assert_eq!(r("My.A.dll"), Some(cands[0].clone()));
        assert_eq!(r("My.A"), Some(cands[0].clone()));
        assert_eq!(r("tool"), Some(cands[1].clone()));
        assert_eq!(r("other"), None);
        assert_eq!(r("./tool"), None);
    }
}
