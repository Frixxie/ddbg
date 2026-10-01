//! Engine tests against a scripted in-memory fake adapter.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ddbg_core::adapter::LldbDapAdapter;
use ddbg_core::breakpoint::{BreakpointId, SourceLocation};
use ddbg_core::command::{Command, FrameSelector, Location, Reply};
use ddbg_core::engine::{Connection, EngineConfig, spawn_with_connector};
use ddbg_core::session::StopReason;
use ddbg_core::{DebugEvent, EngineHandle, LaunchTarget};
use ddbg_dap::DapClient;
use ddbg_dap::codec::{Decoder, encode};
use ddbg_dap::protocol::{Event, Message, Request, Response};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::broadcast;

type Log = Arc<Mutex<Vec<String>>>;

struct Fake {
    io: DuplexStream,
    decoder: Decoder,
    seq: i64,
    log: Log,
    bp_id: i64,
}

impl Fake {
    async fn send(&mut self, msg: Message) {
        self.io.write_all(&encode(&msg).unwrap()).await.unwrap();
    }

    async fn event(&mut self, name: &str, body: Value) {
        self.seq += 1;
        let msg = Message::Event(Event {
            seq: self.seq,
            event: name.into(),
            body: Some(body),
        });
        self.send(msg).await;
    }

    async fn respond(&mut self, req: &Request, body: Value) {
        self.seq += 1;
        let msg = Message::Response(Response {
            seq: self.seq,
            request_seq: req.seq,
            success: true,
            command: req.command.clone(),
            message: None,
            body: Some(body),
        });
        self.send(msg).await;
    }

    async fn run(mut self) {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let req = loop {
                if let Some(Message::Request(r)) = self.decoder.decode().unwrap() {
                    break r;
                }
                match self.io.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => self.decoder.extend(&buf[..n]),
                }
            };
            self.log.lock().unwrap().push(req.command.clone());
            let args = req.arguments.clone().unwrap_or(Value::Null);
            match req.command.as_str() {
                "initialize" => {
                    self.respond(
                        &req,
                        json!({"supportsConfigurationDoneRequest": true, "supportTerminateDebuggee": true}),
                    )
                    .await
                }
                "launch" => {
                    self.respond(&req, json!({})).await;
                    self.event("initialized", json!({})).await;
                }
                "setBreakpoints" => {
                    let bps: Vec<Value> = args["breakpoints"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|b| {
                            self.bp_id += 1;
                            json!({"id": self.bp_id, "verified": true, "line": b["line"]})
                        })
                        .collect();
                    self.respond(&req, json!({ "breakpoints": bps })).await;
                }
                "configurationDone" => {
                    self.respond(&req, json!({})).await;
                    self.event(
                        "stopped",
                        json!({"reason": "breakpoint", "threadId": 1, "hitBreakpointIds": [1]}),
                    )
                    .await;
                }
                "threads" => {
                    self.respond(&req, json!({"threads": [{"id": 1, "name": "main"}]}))
                        .await
                }
                "stackTrace" => {
                    self.respond(
                        &req,
                        json!({"stackFrames": [
                            {"id": 10, "name": "app::main", "source": {"path": "/src/main.rs", "name": "main.rs"}, "line": 5, "column": 1},
                            {"id": 11, "name": "std::rt::lang_start", "line": 0, "column": 0}
                        ]}),
                    )
                    .await
                }
                "scopes" => {
                    let r = if args["frameId"] == 10 { 100 } else { 200 };
                    self.respond(
                        &req,
                        json!({"scopes": [{"name": "Locals", "variablesReference": r, "presentationHint": "locals", "expensive": false}]}),
                    )
                    .await
                }
                "variables" => {
                    self.respond(
                        &req,
                        json!({"variables": [{"name": "x", "value": "42", "type": "i32", "variablesReference": 0}]}),
                    )
                    .await
                }
                "evaluate" => {
                    self.respond(
                        &req,
                        json!({"result": "Point", "type": "Point", "variablesReference": 300}),
                    )
                    .await
                }
                "next" => {
                    self.respond(&req, json!({})).await;
                    self.event("stopped", json!({"reason": "step", "threadId": 1}))
                        .await;
                }
                "continue" => {
                    self.respond(&req, json!({"allThreadsContinued": true}))
                        .await;
                    self.event("continued", json!({"threadId": 1})).await;
                    self.event("output", json!({"category": "stdout", "output": "done\n"}))
                        .await;
                    self.event("exited", json!({"exitCode": 0})).await;
                    self.event("terminated", json!({})).await;
                }
                _ => self.respond(&req, json!({})).await,
            }
        }
    }
}

fn engine() -> (EngineHandle, Log) {
    let log: Log = Arc::default();
    let log2 = log.clone();
    let config = EngineConfig {
        adapter: Arc::new(LldbDapAdapter::default()),
        cwd: PathBuf::from("/"),
        target: Some(LaunchTarget::new("/bin/app", vec![])),
    };
    let handle = spawn_with_connector(
        config,
        Box::new(move |_| {
            let (a, b) = tokio::io::duplex(64 * 1024);
            let (r, w) = tokio::io::split(a);
            let (client, incoming) = DapClient::connect(r, w);
            tokio::spawn(
                Fake {
                    io: b,
                    decoder: Decoder::new(),
                    seq: 0,
                    log: log2.clone(),
                    bp_id: 0,
                }
                .run(),
            );
            Ok(Connection {
                client,
                incoming,
                child: None,
            })
        }),
    );
    (handle, log)
}

async fn wait_for(
    rx: &mut broadcast::Receiver<DebugEvent>,
    pred: impl Fn(&DebugEvent) -> bool,
) -> DebugEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.unwrap();
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

fn requests(log: &Log, name: &str) -> usize {
    log.lock().unwrap().iter().filter(|c| *c == name).count()
}

async fn start_stopped_at_breakpoint() -> (EngineHandle, Log, broadcast::Receiver<DebugEvent>) {
    let (e, log) = engine();
    let mut rx = e.subscribe();
    e.execute(Command::Break(Location::Source(SourceLocation::new(
        "/src/main.rs",
        5,
    ))))
    .await
    .unwrap();
    assert_eq!(
        e.execute(Command::Run(None)).await.unwrap(),
        Reply::Launched("/bin/app".into())
    );
    let ev = wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await;
    let DebugEvent::SessionStopped(info) = ev else {
        unreachable!()
    };
    assert_eq!(info.reason, StopReason::Breakpoint(vec![BreakpointId(1)]));
    assert_eq!(info.frame.unwrap().line, 5);
    (e, log, rx)
}

#[tokio::test]
async fn lifecycle_order_and_breakpoint_hit() {
    let (e, log, _rx) = start_stopped_at_breakpoint().await;
    let order: Vec<String> = log.lock().unwrap().iter().take(4).cloned().collect();
    assert_eq!(
        order,
        [
            "initialize",
            "launch",
            "setBreakpoints",
            "configurationDone"
        ]
    );

    let Reply::Breakpoints(bps) = e.execute(Command::Breakpoints).await.unwrap() else {
        panic!()
    };
    assert!(bps[0].verified);
    assert_eq!(bps[0].adapter_id, Some(1));
}

#[tokio::test]
async fn inspect_step_continue_to_exit() {
    let (e, log, mut rx) = start_stopped_at_breakpoint().await;

    let Reply::Locals(scopes) = e.execute(Command::Locals).await.unwrap() else {
        panic!()
    };
    assert_eq!(scopes[0].variables[0].name, "x");
    e.execute(Command::Locals).await.unwrap();
    assert_eq!(
        requests(&log, "variables"),
        1,
        "variables are cached while stopped"
    );

    let Reply::Value(v, children) = e.execute(Command::Print("p".into())).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.value, "Point");
    assert_eq!(children.len(), 1);

    let Reply::Backtrace { frames, selected } = e.execute(Command::Backtrace).await.unwrap() else {
        panic!()
    };
    assert_eq!(frames.len(), 2);
    assert_eq!(selected, Some(0));

    let Reply::Frame { index, .. } = e.execute(Command::Frame(FrameSelector::Up)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(index, 1);
    assert!(e.execute(Command::Frame(FrameSelector::Up)).await.is_err());

    e.execute(Command::Next).await.unwrap();
    wait_for(
        &mut rx,
        |e| matches!(e, DebugEvent::SessionStopped(i) if i.reason == StopReason::Step),
    )
    .await;
    e.execute(Command::Locals).await.unwrap();
    assert_eq!(
        requests(&log, "variables"),
        3,
        "cache is cleared after resume"
    );

    e.execute(Command::Continue).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionContinued)).await;
    wait_for(
        &mut rx,
        |e| matches!(e, DebugEvent::Output(o) if o.text == "done\n"),
    )
    .await;
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionExited(0))).await;
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionTerminated)).await;

    assert!(e.execute(Command::Backtrace).await.is_err());
    assert_eq!(requests(&log, "disconnect"), 1);
}

#[tokio::test]
async fn commands_require_a_session() {
    let (e, _log) = engine();
    for cmd in [
        Command::Continue,
        Command::Next,
        Command::Backtrace,
        Command::Locals,
    ] {
        assert!(e.execute(cmd).await.is_err());
    }
    // Breakpoints can be set before running.
    let r = e
        .execute(Command::Break(Location::Source(SourceLocation::new(
            "/src/lib.rs",
            3,
        ))))
        .await
        .unwrap();
    assert!(matches!(r, Reply::BreakpointSet { new: true, .. }));
    assert!(
        e.execute(Command::DeleteBreakpoint(BreakpointId(9)))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn breakpoint_added_while_stopped_resends_whole_file() {
    let (e, log, _rx) = start_stopped_at_breakpoint().await;
    let Reply::BreakpointSet { breakpoint, .. } = e
        .execute(Command::Break(Location::Source(SourceLocation::new(
            "/src/main.rs",
            9,
        ))))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(breakpoint.verified);
    assert_eq!(requests(&log, "setBreakpoints"), 2);
    e.execute(Command::Quit).await.unwrap();
}
