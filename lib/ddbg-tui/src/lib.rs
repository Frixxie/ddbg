//! Full-screen terminal frontend built on ratatui.
//!
//! A presentation layer over the same [`Session`] and [`Command`]s as the
//! REPL: keys map to commands, `:` opens a command line that accepts the
//! REPL syntax, and panes mirror the debugger state.
//!
//! The UI thread never awaits the engine. Commands run on spawned tasks and
//! report back through the same channel as terminal input and debugger
//! events, so the screen stays responsive while a test builds.

mod ansi;
mod app;
mod functions;
mod highlight;
mod picker;
mod ui;

use std::sync::Arc;

use ddbg_cli::{Outcome, Prepared};
use ddbg_core::DebugEvent;
use ddbg_core::command::Command;
use ratatui::crossterm::event::{self, Event};
use tokio::sync::{Mutex, broadcast, mpsc};

use app::App;

/// Messages processed by the UI loop.
pub(crate) enum Msg {
    Input(Event),
    Debug(DebugEvent),
    /// A command finished. Silent results update panes without logging.
    Done {
        outcome: Outcome,
        silent: bool,
    },
    Progress(String),
    EventsLagged(u64),
    /// Result of function discovery.
    Functions(Vec<functions::Function>),
    /// Completions for the command line `line`.
    Completions {
        line: String,
        items: Vec<ddbg_cli::commands::Suggestion>,
    },
}

/// Spawns commands without blocking the UI.
#[derive(Clone)]
pub(crate) struct Executor {
    session: Arc<Mutex<ddbg_cli::Session>>,
    tx: mpsc::UnboundedSender<Msg>,
}

impl Executor {
    pub(crate) fn spawn(&self, cmd: Command, silent: bool) {
        let session = self.session.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let progress_tx = tx.clone();
            let mut progress = |s: &str| {
                let _ = progress_tx.send(Msg::Progress(s.to_owned()));
            };
            let outcome = session.lock().await.execute(cmd, &mut progress).await;
            let _ = tx.send(Msg::Done { outcome, silent });
        });
    }

    /// Scan sources under `root` for functions off the UI thread.
    pub(crate) fn discover_functions(&self, root: std::path::PathBuf) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::Functions(functions::discover(&root)));
        });
    }

    /// Complete the command line `line` (cursor at its end) off the UI
    /// thread. Expressions query the engine directly so a long-running
    /// command holding the session does not block completion.
    pub(crate) fn complete(&self, engine: &ddbg_core::EngineHandle, line: String) {
        use ddbg_cli::commands::{expression_query, expression_suggestions, suggestions};
        let tx = self.tx.clone();
        let Some((offset, query)) = expression_query(&line, line.len()) else {
            let items = suggestions(&line, line.len());
            let _ = tx.send(Msg::Completions { line, items });
            return;
        };
        let engine = engine.clone();
        tokio::spawn(async move {
            let items = match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                engine.execute(query),
            )
            .await
            {
                Ok(Ok(reply)) => expression_suggestions(&line, offset, reply),
                _ => Vec::new(),
            };
            let _ = tx.send(Msg::Completions { line, items });
        });
    }

    /// Interrupt the debuggee without waiting for the session lock, which
    /// may be held by a long-running command.
    pub(crate) fn pause(&self, engine: &ddbg_core::EngineHandle) {
        let engine = engine.clone();
        tokio::spawn(async move {
            let _ = engine.execute(Command::Pause).await;
        });
    }
}

/// Run the TUI until the user quits.
pub async fn run(prepared: Prepared) -> anyhow::Result<()> {
    let Prepared {
        session,
        initial,
        verbose,
        program,
    } = prepared;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Terminal input. crossterm's `read` blocks, so it gets its own thread.
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(Msg::Input(ev)).is_err() {
                break;
            }
        }
    });

    // Debugger events.
    let mut events = session.engine.subscribe();
    let event_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            let msg = match events.recv().await {
                Ok(e) => Msg::Debug(e),
                Err(broadcast::error::RecvError::Lagged(n)) => Msg::EventsLagged(n),
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if event_tx.send(msg).is_err() {
                break;
            }
        }
    });

    let engine = session.engine.clone();
    let cwd = session.cwd.clone();
    let candidates = session.candidates.clone();
    let exec = Executor {
        session: Arc::new(Mutex::new(session)),
        tx,
    };
    let mut app = App::new(exec, engine, cwd, verbose, candidates, program);
    app.log(format!(
        "ddbg {}. Press ? for keys, : for commands.",
        env!("CARGO_PKG_VERSION")
    ));
    app.refresh_breakpoints();
    for cmd in initial {
        app.execute(cmd);
    }
    // Several binaries and none chosen: defer the choice until the user runs.
    if app.program.is_none() && app.candidates.len() > 1 {
        app.log(format!(
            "{} programs detected. Press r or e to choose one.",
            app.candidates.len()
        ));
    }

    let mut terminal = ratatui::init();
    let result = async {
        loop {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            let Some(msg) = rx.recv().await else { break };
            app.handle(msg);
            // Drain whatever else is queued before redrawing.
            while let Ok(msg) = rx.try_recv() {
                app.handle(msg);
            }
            if app.should_quit {
                break;
            }
        }
        anyhow::Ok(())
    }
    .await;
    ratatui::restore();
    result
}
