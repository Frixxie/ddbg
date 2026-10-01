//! Interactive REPL built on reedline.
//!
//! reedline's `read_line` blocks, so it runs on a dedicated thread and
//! hands lines to the async side. All output (command results and async
//! debugger events) goes through reedline's external printer so it never
//! corrupts the prompt.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};

use ddbg_core::command::Command;
use reedline::{
    ColumnarMenu, Emacs, ExternalPrinter, FileBackedHistory, KeyCode, KeyModifiers, MenuBuilder,
    Prompt, PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, Signal,
    default_emacs_keybindings,
};
use tokio::sync::{broadcast, mpsc};

use crate::commands::DdbgCompleter;
use crate::parser::{Input, parse};
use crate::render::{Renderer, help};
use crate::session::{Outcome, Session};

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
pub async fn run(session: Session, initial: Vec<Command>, verbose: bool) -> anyhow::Result<()> {
    let printer = ExternalPrinter::<String>::new(4096);
    let out = printer.sender();
    let mut renderer = Renderer::new(session.cwd.clone());
    renderer.show_console = verbose;
    let renderer = Arc::new(Mutex::new(renderer));

    // Async debugger events → printer.
    let mut events = session.engine.subscribe();
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
        session,
        renderer,
        out,
        last_repeatable: None,
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
                let _ = repl.session.engine.execute(Command::Pause).await;
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
    session: Session,
    renderer: Arc<Mutex<Renderer>>,
    out: std_mpsc::SyncSender<String>,
    /// Command repeated by an empty line (like gdb).
    last_repeatable: Option<Command>,
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

    async fn execute(&mut self, cmd: Command) -> bool {
        let out = self.out.clone();
        let mut progress = |s: &str| {
            let _ = out.send(s.to_owned());
        };
        match self.session.execute(cmd, &mut progress).await {
            Outcome::Quit => return true,
            Outcome::Reply(cmd, reply) => {
                if let Some(text) = self.renderer.lock().unwrap().reply(&cmd, &reply) {
                    self.print(text);
                }
            }
            Outcome::Text(text) => self.print(text),
            Outcome::Error(e) => self.print(format!("error: {e}")),
        }
        false
    }
}
