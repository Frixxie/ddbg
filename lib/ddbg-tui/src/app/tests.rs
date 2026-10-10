use super::*;
use ddbg_cli::Outcome;
use ddbg_core::DebugEvent;
use ddbg_core::adapter::LldbDapAdapter;
use ddbg_core::command::Reply;
use ddbg_core::engine::{self, EngineConfig};
use ddbg_core::session::StopReason;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

#[tokio::test]
async fn select_and_inspect_locals_across_scopes() {
    let cwd = PathBuf::from(".");
    let engine = engine::spawn(EngineConfig {
        adapter: Arc::new(LldbDapAdapter::default()),
        cwd: cwd.clone(),
        target: None,
    });
    let (tx, _rx) = mpsc::unbounded_channel();
    let exec = Executor {
        session: Arc::new(Mutex::new(ddbg_cli::Session {
            engine: engine.clone(),
            cwd: cwd.clone(),
            candidates: Vec::new(),
            tests: ddbg_cli::testing::Tests::new(None, ""),
        })),
        tx,
    };
    let mut app = App::new(exec, engine, cwd, false, Vec::new(), None);
    let key = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
    app.apply_reply(Reply::Locals(
        (0..2)
            .map(|i| ScopeVariables {
                scope: format!("scope {i}"),
                reference: VarRef(i + 1),
                variables: vec![Variable {
                    name: format!("var{i}"),
                    value: format!("first line\n{}\nlast line", "long value ".repeat(300)),
                    type_name: Some("String".into()),
                    children: None,
                    evaluate_name: None,
                }],
            })
            .collect(),
    ));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Locals);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected_local().unwrap().name, "var1");
    key(&mut app, KeyCode::Down);
    assert_eq!(app.locals_cursor, 1);
    key(&mut app, KeyCode::Enter);
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let screen = format!("{:?}", terminal.backend().buffer());
    assert!(screen.contains("var1: String"));
    assert!(screen.contains("first line"));
    key(&mut app, KeyCode::Char('G'));
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(app.value_scroll.unwrap() > 0);
    assert!(format!("{:?}", terminal.backend().buffer()).contains("last line"));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.value_scroll, None);
    key(&mut app, KeyCode::Enter);
    app.debug_event(DebugEvent::SessionContinued);
    assert_eq!(app.value_scroll, None);
    assert!(app.selected_local().is_none());
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.value_scroll, None);
}

fn test_app() -> App {
    let cwd = PathBuf::from(".");
    let engine = engine::spawn(EngineConfig {
        adapter: Arc::new(LldbDapAdapter::default()),
        cwd: cwd.clone(),
        target: None,
    });
    let (tx, _rx) = mpsc::unbounded_channel();
    let exec = Executor {
        session: Arc::new(Mutex::new(ddbg_cli::Session {
            engine: engine.clone(),
            cwd: cwd.clone(),
            candidates: Vec::new(),
            tests: ddbg_cli::testing::Tests::new(None, ""),
        })),
        tx,
    };
    App::new(exec, engine, cwd, false, Vec::new(), None)
}

#[tokio::test]
async fn attach_reply_replaces_program_identity() {
    let mut app = test_app();
    app.program = Some("old-program".into());
    app.outcome(
        Outcome::Reply(
            Command::Attach(ddbg_core::AttachTarget { pid: 42 }),
            Reply::Attached(42),
        ),
        false,
    );
    assert_eq!(app.attached_pid, Some(42));
    assert!(app.program.is_none());
    app.outcome(
        Outcome::Reply(Command::Run(None), Reply::Launched("new-program".into())),
        false,
    );
    assert_eq!(app.attached_pid, None);
    assert_eq!(app.program, Some("new-program".into()));
}

#[tokio::test]
async fn breakpoint_interval_sums_run_time_across_steps() {
    use ddbg_core::event::StopInfo;
    use std::time::Duration;
    let mut app = test_app();
    let stop = |reason, ms| {
        DebugEvent::SessionStopped(Box::new(StopInfo {
            reason,
            thread: None,
            description: None,
            frame: None,
            exception: None,
            elapsed: Some(Duration::from_millis(ms)),
            watches: Vec::new(),
        }))
    };
    app.debug_event(DebugEvent::SessionStarted);
    app.debug_event(stop(StopReason::Breakpoint(vec![BreakpointId(1)]), 5));
    assert_eq!(app.bp_interval, None);
    app.debug_event(stop(StopReason::Step, 3));
    app.debug_event(stop(StopReason::Breakpoint(vec![BreakpointId(2)]), 7));
    assert_eq!(
        app.bp_interval,
        Some(BpInterval {
            from: BreakpointId(1),
            to: BreakpointId(2),
            time: Duration::from_millis(10),
        })
    );
    app.debug_event(DebugEvent::SessionStarted);
    assert_eq!(app.bp_interval, None);
}

#[tokio::test]
async fn watch_pane_tracks_replies_refreshes_and_resume() {
    use ddbg_core::watch::{Watch, WatchId, WatchValue};
    let mut app = test_app();
    let w = Watch {
        id: WatchId(1),
        expression: "x".into(),
        result: Some(Ok(WatchValue {
            value: "42".into(),
            type_name: None,
            has_children: false,
        })),
    };
    app.apply_reply(Reply::WatchSet {
        watch: w.clone(),
        new: true,
    });
    app.apply_reply(Reply::WatchSet {
        watch: w.clone(),
        new: false,
    });
    assert_eq!(app.watches.len(), 1);
    app.debug_event(DebugEvent::SessionContinued);
    assert!(app.watches[0].result.is_none());
    app.debug_event(DebugEvent::WatchesChanged(vec![w.clone()]));
    assert_eq!(app.watches, vec![w]);
    app.apply_reply(Reply::WatchDeleted(WatchId(1)));
    assert!(app.watches.is_empty());
}

#[tokio::test]
async fn edit_local_value() {
    let mut app = test_app();
    let key = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
    app.apply_reply(Reply::Locals(vec![ScopeVariables {
        scope: "Locals".into(),
        reference: VarRef(7),
        variables: vec![Variable {
            name: "x".into(),
            value: "1".into(),
            type_name: None,
            children: None,
            evaluate_name: None,
        }],
    }]));
    app.focus = Focus::Locals;
    key(&mut app, KeyCode::Char('='));
    assert_eq!(app.edit.as_ref().unwrap().value, "1");
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Char('4'));
    key(&mut app, KeyCode::Char('2'));
    assert_eq!(
        app.edit_command(),
        Some(Command::SetVariable {
            scope: VarRef(7),
            name: "x".into(),
            value: "42".into(),
        })
    );
    key(&mut app, KeyCode::Esc);
    assert!(app.edit.is_none());
}

#[tokio::test]
async fn command_line_completion_applies_and_cycles() {
    use ddbg_cli::commands::Span;
    let mut app = test_app();
    let key = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
    key(&mut app, KeyCode::Char(':'));
    for c in "p po".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    let item = |v: &str| Suggestion {
        value: v.into(),
        span: Span::new(2, 4),
        ..Default::default()
    };
    app.handle(Msg::Completions {
        line: "p po".into(),
        items: vec![item("point")],
    });
    assert_eq!(app.input.as_deref(), Some("p point"));
    assert!(app.completions.is_none());

    app.input = Some("p po".into());
    app.handle(Msg::Completions {
        line: "p po".into(),
        items: vec![item("point"), item("pos")],
    });
    assert_eq!(app.input.as_deref(), Some("p point"));
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.input.as_deref(), Some("p pos"));
    key(&mut app, KeyCode::Char('x'));
    assert!(app.completions.is_none());
    assert_eq!(app.input.as_deref(), Some("p posx"));
    // Stale results for an edited line are ignored.
    app.handle(Msg::Completions {
        line: "p po".into(),
        items: vec![item("point")],
    });
    assert_eq!(app.input.as_deref(), Some("p posx"));
}
