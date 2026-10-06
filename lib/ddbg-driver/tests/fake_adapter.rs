//! Driver tests against a minimal scripted in-memory adapter.

use std::sync::Arc;

use ddbg_core::LaunchTarget;
use ddbg_core::adapter::LldbDapAdapter;
use ddbg_core::engine::{Connection, EngineConfig, spawn_with_connector};
use ddbg_dap::DapClient;
use ddbg_dap::codec::{Decoder, encode};
use ddbg_dap::protocol::{Event, Message, Response};
use ddbg_driver::{Debugger, Halt, StopReason};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

async fn fake(mut io: DuplexStream, exit_on_continue: bool) {
    fake_with_output(&mut io, exit_on_continue, "done\n").await;
}

async fn fake_with_output(io: &mut DuplexStream, exit_on_continue: bool, output: &str) {
    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 16 * 1024];
    let mut seq = 0;
    let send = async |io: &mut DuplexStream, msg: Message| {
        io.write_all(&encode(&msg).unwrap()).await.unwrap();
    };
    loop {
        let req = loop {
            if let Some(Message::Request(r)) = decoder.decode().unwrap() {
                break r;
            }
            match io.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => decoder.extend(&buf[..n]),
            }
        };
        let args = req.arguments.clone().unwrap_or(Value::Null);
        let body = match req.command.as_str() {
            "initialize" => {
                json!({"supportsConfigurationDoneRequest": true, "supportsConditionalBreakpoints": true})
            }
            "setBreakpoints" => {
                let line = args["breakpoints"][0]["line"].clone();
                json!({"breakpoints": [{"id": 1, "verified": true, "line": line}]})
            }
            "threads" => json!({"threads": [{"id": 1, "name": "main"}]}),
            "stackTrace" => json!({"stackFrames": [
                {"id": 10, "name": "app::main", "source": {"path": "/src/main.rs"}, "line": 5, "column": 1}
            ]}),
            "scopes" => {
                json!({"scopes": [{"name": "Locals", "variablesReference": 100, "expensive": false}]})
            }
            "variables" => {
                json!({"variables": [{"name": "x", "value": "42", "type": "i32", "variablesReference": 0}]})
            }
            "evaluate" => json!({"result": "7", "variablesReference": 0}),
            _ => json!({}),
        };
        seq += 1;
        let resp = Response {
            seq,
            request_seq: req.seq,
            success: true,
            command: req.command.clone(),
            message: None,
            body: Some(body),
        };
        send(io, Message::Response(resp)).await;
        let events: &[(&str, Value)] = match req.command.as_str() {
            "launch" | "attach" => &[("initialized", json!({}))],
            "configurationDone" => &[(
                "stopped",
                json!({"reason": "breakpoint", "threadId": 1, "hitBreakpointIds": [1]}),
            )],
            "next" => &[("stopped", json!({"reason": "step", "threadId": 1}))],
            "continue" if exit_on_continue => &[
                ("output", json!({"category": "stdout", "output": output})),
                ("exited", json!({"exitCode": 0})),
                ("terminated", json!({})),
            ],
            _ => &[],
        };
        for (name, body) in events {
            seq += 1;
            let ev = Event {
                seq,
                event: (*name).into(),
                body: Some(body.clone()),
            };
            send(io, Message::Event(ev)).await;
        }
    }
}

fn debugger() -> Debugger {
    debugger_with_exit(true)
}

fn debugger_with_exit(exit_on_continue: bool) -> Debugger {
    let config = EngineConfig {
        adapter: Arc::new(LldbDapAdapter::default()),
        cwd: "/".into(),
        target: Some(LaunchTarget::new("/bin/app", vec![])),
    };
    let engine = spawn_with_connector(
        config,
        Box::new(move |_| {
            let (a, b) = tokio::io::duplex(64 * 1024);
            let (r, w) = tokio::io::split(a);
            let (client, incoming) = DapClient::connect(r, w);
            tokio::spawn(fake(b, exit_on_continue));
            Ok(Connection {
                client,
                incoming,
                child: None,
            })
        }),
    );
    Debugger::from_engine(engine, "/")
}

fn debugger_with_test_output(output: &'static str) -> Debugger {
    let engine = spawn_with_connector(
        EngineConfig {
            adapter: Arc::new(LldbDapAdapter::default()),
            cwd: "/".into(),
            target: Some(LaunchTarget::new("/bin/test-app", vec![])),
        },
        Box::new(move |_| {
            let (a, mut b) = tokio::io::duplex(64 * 1024);
            let (r, w) = tokio::io::split(a);
            let (client, incoming) = DapClient::connect(r, w);
            tokio::spawn(async move { fake_with_output(&mut b, true, output).await });
            Ok(Connection {
                client,
                incoming,
                child: None,
            })
        }),
    );
    let mut tests = ddbg_cli::testing::Tests::new(None, "fake test provider");
    tests.set_debugged(Some(ddbg_test::TestCase {
        id: ddbg_test::TestId {
            provider: ddbg_test::ProviderId::DotNet,
            name: "Ns.Tests.Selected".into(),
            data: ddbg_test::ProviderData::DotNet {
                assembly: "/bin/test-app".into(),
                project: "/test.csproj".into(),
                framework: ddbg_test::dotnet::Framework::XUnitV3,
            },
        },
        name: "Ns.Tests.Selected".into(),
        display_name: "Selected".into(),
        source: None,
        line: None,
        suite: None,
    }));
    Debugger::from_session(ddbg_cli::Session {
        engine,
        cwd: "/".into(),
        candidates: vec![],
        tests,
    })
}

#[tokio::test]
async fn debug_test_outcome_uses_runner_counts_not_zero_adapter_exit() {
    use ddbg_driver::TestOutcome;
    for (output, outcome, total) in [
        (
            "\x1b[32mTest run summary: Passed\x1b[0m\n  total: 1\n  failed: 0\n  succeeded: 1\n  skipped: 0\n",
            Some(TestOutcome::Passed),
            Some(1),
        ),
        (
            "Test run summary: Failed\n  total: 1\n  failed: 1\n  succeeded: 0\n  skipped: 0\n",
            Some(TestOutcome::Failed),
            Some(1),
        ),
        (
            "Test run summary: Zero tests ran\n  total: 0\n  failed: 0\n  succeeded: 0\n  skipped: 0\n",
            Some(TestOutcome::Failed),
            Some(0),
        ),
        (
            "Test run summary: Passed\n  total: 2\n  failed: 0\n  succeeded: 2\n  skipped: 0\n",
            Some(TestOutcome::Failed),
            Some(2),
        ),
        (
            "Test run summary: Skipped\n  total: 1\n  failed: 0\n  succeeded: 0\n  skipped: 1\n",
            Some(TestOutcome::Ignored),
            Some(1),
        ),
        ("Test run summary: incomplete\n  total: 1\n", None, None),
        ("No summary\n", None, None),
    ] {
        let mut dbg = debugger_with_test_output(output);
        dbg.run().await.unwrap().into_stopped().unwrap();
        assert_eq!(dbg.debug_test_result().unwrap().outcome, None);
        assert_eq!(dbg.cont().await.unwrap(), Halt::Exited(0));
        let result = dbg.debug_test_result().unwrap();
        assert_eq!(result.outcome, outcome, "{output}");
        assert_eq!(result.counts.map(|c| c.total), total, "{output}");
        if total != Some(1) {
            assert!(result.diagnostic.is_some(), "{result:?}");
        }
        // Output reads and a retained exit must not discard or duplicate the result.
        dbg.stdout();
        dbg.take_transcript();
        assert_eq!(dbg.wait().await.unwrap(), Halt::Exited(0));
        assert_eq!(dbg.debug_test_result(), Some(result));
        dbg.run().await.unwrap().into_stopped().unwrap();
        assert_eq!(dbg.debug_test_result().unwrap().counts, None);
        dbg.kill().await.unwrap();
        dbg.wait().await.unwrap();
        assert_eq!(dbg.debug_test_result().unwrap().outcome, None);
        dbg.quit().await.unwrap();
    }
}

#[tokio::test]
async fn typed_session() {
    let mut dbg = debugger();
    dbg.break_at("/src/main.rs", 5).await.unwrap();
    let stop = dbg.run().await.unwrap().into_stopped().unwrap();
    assert!(matches!(stop.reason, StopReason::Breakpoint(_)));
    assert_eq!(dbg.local("x").await.unwrap().value, "42");
    assert_eq!(dbg.print("a + b").await.unwrap().value, "7");
    let stop = dbg.next().await.unwrap().into_stopped().unwrap();
    assert_eq!(stop.reason, StopReason::Step);
    assert_eq!(dbg.cont().await.unwrap(), Halt::Exited(0));
    assert_eq!(dbg.stdout(), "done\n");
    assert!(dbg.backtrace().await.is_err());
}

#[tokio::test]
async fn attach_and_detach_through_typed_and_text_commands() {
    let mut dbg = debugger();
    assert!(dbg.attach(42).await.unwrap().is_stopped());
    assert_eq!(dbg.local("x").await.unwrap().value, "42");
    dbg.detach().await.unwrap();
    assert_eq!(dbg.wait().await.unwrap(), Halt::Terminated);
    assert!(dbg.run().await.is_err());
    let transcript = dbg.exec("attach 42").await.unwrap();
    assert!(
        transcript.contains("Attached to process 42"),
        "{transcript}"
    );
    assert!(dbg.exec("detach").await.unwrap().contains("left running"));
    dbg.quit().await.unwrap();
}

#[tokio::test]
async fn wait_retains_halt_after_kill_and_event_consumption() {
    let mut dbg = debugger();
    let stop = dbg.run().await.unwrap();
    assert!(stop.is_stopped());
    dbg.take_transcript();
    assert_eq!(dbg.wait().await.unwrap(), stop);
    dbg.kill().await.unwrap();
    dbg.take_transcript();
    dbg.set_timeout(std::time::Duration::ZERO);
    assert_eq!(dbg.wait().await.unwrap(), Halt::Terminated);
    assert_eq!(dbg.wait().await.unwrap(), Halt::Terminated);
    dbg.set_timeout(std::time::Duration::from_secs(5));
    dbg.run().await.unwrap().into_stopped().unwrap();
    dbg.next().await.unwrap().into_stopped().unwrap();
    dbg.quit().await.unwrap();
}

#[tokio::test]
async fn wait_observes_exit_that_arrived_between_calls() {
    use ddbg_core::command::Command;
    let mut dbg = debugger();
    dbg.run().await.unwrap().into_stopped().unwrap();
    dbg.execute(Command::Continue).await.unwrap();
    dbg.wait_for(|ev| matches!(ev, ddbg_core::DebugEvent::SessionExited(_)))
        .await
        .unwrap();
    // Same event drain MCP performs before a subsequent wait (or output read).
    dbg.take_transcript();
    dbg.set_timeout(std::time::Duration::ZERO);
    assert_eq!(dbg.wait().await.unwrap(), Halt::Exited(0));
    assert_eq!(dbg.wait().await.unwrap(), Halt::Exited(0));
    dbg.set_timeout(std::time::Duration::from_secs(5));
    dbg.run().await.unwrap().into_stopped().unwrap();
    dbg.quit().await.unwrap();
}

#[tokio::test]
async fn kill_interrupts_a_pending_wait() {
    let mut dbg = debugger_with_exit(false);
    dbg.run().await.unwrap().into_stopped().unwrap();
    dbg.engine()
        .execute(ddbg_core::command::Command::Continue)
        .await
        .unwrap();
    let engine = dbg.engine().clone();
    let (halt, killed) = tokio::join!(dbg.wait(), async {
        tokio::task::yield_now().await;
        engine.execute(ddbg_core::command::Command::Kill).await
    });
    killed.unwrap();
    assert_eq!(halt.unwrap(), Halt::Terminated);
    dbg.quit().await.unwrap();
}

#[tokio::test]
async fn text_session() {
    let mut dbg = debugger();
    let out = dbg
        .script("break /src/main.rs:5\nrun\nprint a + b\ncontinue")
        .await
        .unwrap();
    assert!(out.contains("ddbg> print a + b\n7\n"), "{out}");
    assert!(out.contains("Process exited normally."), "{out}");
    assert!(dbg.exec("next").await.is_err());
    assert!(dbg.exec("nonsense").await.is_err());
}

#[tokio::test]
async fn conditional_breakpoints_via_typed_and_text_commands() {
    use ddbg_core::breakpoint::{Location, SourceLocation};
    let mut dbg = debugger();
    let bp = dbg
        .break_if(
            Location::Source(SourceLocation::new("/src/main.rs", 5)),
            "x == 42",
        )
        .await
        .unwrap();
    assert_eq!(bp.condition.as_deref(), Some("x == 42"));
    dbg.run().await.unwrap().into_stopped().unwrap();
    assert!(
        dbg.exec("condition 1 x > 2")
            .await
            .unwrap()
            .contains("if x > 2")
    );
    assert_eq!(
        dbg.breakpoints().await.unwrap()[0].condition.as_deref(),
        Some("x > 2")
    );
    let cleared = dbg.condition(bp.id, None).await.unwrap();
    assert_eq!(cleared.condition, None);
    assert!(dbg.exec("break /src/main.rs:5 if x > 3").await.is_err());
    dbg.quit().await.unwrap();
}

#[tokio::test]
async fn watch_expressions_via_typed_and_text_commands() {
    let mut dbg = debugger();
    let w = dbg.watch("x + 1").await.unwrap();
    assert_eq!(w.result, None);
    dbg.break_at("/src/main.rs", 5).await.unwrap();
    let stop = dbg.run().await.unwrap().into_stopped().unwrap();
    assert_eq!(
        stop.watches[0]
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .value,
        "7"
    );
    assert!(dbg.take_transcript().contains("Watch 1: x + 1 = 7"));
    assert!(
        dbg.exec("watch x + 1")
            .await
            .unwrap()
            .contains("already set")
    );
    assert_eq!(dbg.watches().await.unwrap().len(), 1);
    let out = dbg.exec("next").await.unwrap();
    assert!(out.contains("Watch 1: x + 1 = 7"), "{out}");
    dbg.cont().await.unwrap();
    assert!(dbg.watches().await.unwrap()[0].result.is_none());
    dbg.unwatch(w.id).await.unwrap();
    assert_eq!(
        dbg.exec("watches").await.unwrap().lines().next(),
        Some("No watches.")
    );
    dbg.quit().await.unwrap();
}
