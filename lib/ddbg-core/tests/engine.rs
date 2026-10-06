//! Engine tests against a scripted in-memory fake adapter.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ddbg_core::adapter::LldbDapAdapter;
use ddbg_core::breakpoint::{BreakpointId, FunctionLocation, SourceLocation};
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
    caps: Value,
    watch_value: String,
    args: Arc<Mutex<Vec<(String, Value)>>>,
    thread_queries: u64,
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

    async fn reject(&mut self, req: &Request, message: &str) {
        self.seq += 1;
        self.send(Message::Response(Response {
            seq: self.seq,
            request_seq: req.seq,
            success: false,
            command: req.command.clone(),
            message: Some(message.into()),
            body: None,
        }))
        .await;
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
            self.args
                .lock()
                .unwrap()
                .push((req.command.clone(), args.clone()));
            match req.command.as_str() {
                "setBreakpoints" | "setFunctionBreakpoints"
                    if args["breakpoints"].as_array().unwrap().iter().any(|b| b["condition"] == "reject") =>
                {
                    self.seq += 1;
                    self.send(Message::Response(Response {
                        seq: self.seq,
                        request_seq: req.seq,
                        success: false,
                        command: req.command.clone(),
                        message: Some("invalid condition".into()),
                        body: None,
                    })).await;
                }
                "initialize" => {
                    let mut caps = json!({"supportsConfigurationDoneRequest": true, "supportTerminateDebuggee": true, "supportsFunctionBreakpoints": true, "supportsExceptionInfoRequest": true});
                    if let Value::Object(extra) = &self.caps {
                        caps.as_object_mut().unwrap().extend(extra.clone());
                    }
                    self.respond(&req, caps).await
                }
                "setExpression" => {
                    self.watch_value = args["value"].as_str().unwrap().to_owned();
                    self.respond(&req, json!({"value": args["value"], "type": "i32"}))
                        .await
                }
                "setVariable" => {
                    self.watch_value = args["value"].as_str().unwrap().to_owned();
                    self.respond(
                        &req,
                        json!({"value": args["value"], "type": "i32", "variablesReference": 0}),
                    )
                    .await
                }
                "completions" => {
                    self.respond(
                        &req,
                        json!({"targets": [
                            {"label": "point", "type": "variable"},
                            {"label": "pos", "text": "pos", "start": 3, "length": 2}
                        ]}),
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
                "setFunctionBreakpoints" => {
                    let bps: Vec<Value> = args["breakpoints"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|_| {
                            self.bp_id += 1;
                            json!({"id": self.bp_id, "verified": true, "line": 5, "source": {"path": "/src/main.rs"}})
                        })
                        .collect();
                    self.respond(&req, json!({ "breakpoints": bps })).await;
                }
                "configurationDone" => {
                    self.respond(&req, json!({})).await;
                    if self.caps["testRunningLaunch"] != true {
                        self.event(
                        "stopped",
                        json!({"reason": "breakpoint", "threadId": 1, "hitBreakpointIds": [1]}),
                    )
                    .await;
                    }
                }
                "threads" => {
                    self.thread_queries += 1;
                    if self.caps["testNeverThreads"] == true || self.thread_queries <= self.caps["testEmptyThreadResponses"].as_u64().unwrap_or(0) {
                        self.respond(&req, json!({"threads": []})).await;
                    } else {
                        self.respond(&req, json!({"threads": [{"id": 1, "name": "main"}]})).await;
                    }
                    if self.caps["testExitOnThreads"] == true {
                        self.event("exited", json!({"exitCode": 2})).await;
                        self.event("terminated", json!({})).await;
                    }
                }
                "pause" => {
                    self.respond(&req, json!({})).await;
                    self.event("stopped", json!({"reason": "pause", "threadId": 1})).await;
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
                    if args["expression"] == "missing"
                        || (args["expression"] == "frame_local" && args["frameId"] != 10)
                    {
                        self.reject(&req, "expression is out of scope").await;
                    } else if args["expression"] == "watch_value" {
                        self.respond(&req, json!({"result": self.watch_value, "type": "i32", "variablesReference": 0})).await;
                    } else {
                        self.respond(
                            &req,
                            json!({"result": "Point", "type": "Point", "variablesReference": 300}),
                        ).await;
                    }
                }
                "next" => {
                    self.respond(&req, json!({})).await;
                    self.event("stopped", json!({"reason": "step", "threadId": 1}))
                        .await;
                }
                "stepIn" => {
                    self.respond(&req, json!({})).await;
                    self.event(
                        "stopped",
                        json!({"reason": "exception", "threadId": 1, "text": "Unhandled"}),
                    )
                    .await;
                }
                "exceptionInfo" => {
                    assert_eq!(args["threadId"], 1);
                    self.respond(
                        &req,
                        json!({
                            "exceptionId": "System.InvalidOperationException",
                            "description": "Outer failed",
                            "breakMode": "unhandled",
                            "details": {
                                "message": "Outer failed",
                                "typeName": "InvalidOperationException",
                                "fullTypeName": "System.InvalidOperationException",
                                "innerException": [{"message": "Bad arg", "typeName": "System.ArgumentException"}]
                            }
                        }),
                    )
                    .await
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

type Args = Arc<Mutex<Vec<(String, Value)>>>;

fn engine() -> (EngineHandle, Log) {
    let (e, log, _) = engine_with(json!({}));
    (e, log)
}

fn engine_with(caps: Value) -> (EngineHandle, Log, Args) {
    let log: Log = Arc::default();
    let log2 = log.clone();
    let args: Args = Arc::default();
    let args2 = args.clone();
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
                    caps: caps.clone(),
                    watch_value: "42".into(),
                    args: args2.clone(),
                    thread_queries: 0,
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
    (handle, log, args)
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

#[tokio::test]
async fn pause_retries_until_the_first_thread_is_available() {
    let (e, log, args) = engine_with(json!({
        "testRunningLaunch": true, "testEmptyThreadResponses": 3
    }));
    let mut rx = e.subscribe();
    e.execute(Command::Run(None)).await.unwrap();
    assert_eq!(e.execute(Command::Pause).await.unwrap(), Reply::Ok);
    let ev = wait_for(&mut rx, |ev| matches!(ev, DebugEvent::SessionStopped(_))).await;
    let DebugEvent::SessionStopped(info) = ev else {
        unreachable!()
    };
    assert_eq!(info.reason, StopReason::Pause);
    assert!(requests(&log, "threads") >= 4);
    assert_eq!(last_args(&args, "pause")["threadId"], 1);
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn pending_startup_pause_observes_exit_without_pausing_a_dead_thread() {
    let (e, log, _) = engine_with(json!({
        "testRunningLaunch": true, "testNeverThreads": true, "testExitOnThreads": true
    }));
    let mut rx = e.subscribe();
    e.execute(Command::Run(None)).await.unwrap();
    e.execute(Command::Pause).await.unwrap();
    wait_for(&mut rx, |ev| matches!(ev, DebugEvent::SessionExited(2))).await;
    assert_eq!(requests(&log, "pause"), 0);
    assert_eq!(
        *e.subscribe_halt().borrow(),
        Some(DebugEvent::SessionExited(2))
    );
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn pending_startup_pause_does_not_block_kill_or_restart() {
    let (e, _, _) = engine_with(json!({
        "testRunningLaunch": true, "testNeverThreads": true
    }));
    e.execute(Command::Run(None)).await.unwrap();
    e.execute(Command::Pause).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), e.execute(Command::Kill))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *e.subscribe_halt().borrow(),
        Some(DebugEvent::SessionTerminated)
    );
    e.execute(Command::Run(None)).await.unwrap();
    assert_eq!(*e.subscribe_halt().borrow(), None);
    e.execute(Command::Quit).await.unwrap();
}

async fn start_stopped_at_breakpoint() -> (EngineHandle, Log, broadcast::Receiver<DebugEvent>) {
    let (e, log, rx, _) = start_stopped_with(json!({})).await;
    (e, log, rx)
}

async fn start_stopped_with(
    caps: Value,
) -> (EngineHandle, Log, broadcast::Receiver<DebugEvent>, Args) {
    let (e, log, args) = engine_with(caps);
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
    (e, log, rx, args)
}

fn last_args(args: &Args, command: &str) -> Value {
    args.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(c, _)| c == command)
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| panic!("no {command} request"))
}

fn conditional(location: Location, condition: &str) -> Command {
    Command::ConditionalBreak {
        location,
        condition: condition.into(),
    }
}

fn source(line: u32) -> Location {
    Location::Source(SourceLocation::new("/src/main.rs", line))
}

async fn breakpoints(e: &EngineHandle) -> Vec<ddbg_core::breakpoint::Breakpoint> {
    let Reply::Breakpoints(bps) = e.execute(Command::Breakpoints).await.unwrap() else {
        panic!()
    };
    bps
}

#[tokio::test]
async fn conditional_breakpoints_sync_edit_clear_and_survive_restart() {
    let (e, _, args) = engine_with(json!({"supportsConditionalBreakpoints": true}));
    let mut rx = e.subscribe();
    e.execute(conditional(source(5), "x > 2")).await.unwrap();
    e.execute(Command::Break(source(10))).await.unwrap();
    e.execute(conditional(
        Location::Function(FunctionLocation::new("parse", None)),
        "input != 0",
    ))
    .await
    .unwrap();
    e.execute(Command::Run(None)).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await;
    let bps = last_args(&args, "setBreakpoints")["breakpoints"].clone();
    assert_eq!(bps.as_array().unwrap().len(), 2);
    assert_eq!(bps[0]["condition"], "x > 2");
    assert!(bps[1].get("condition").is_none());
    assert_eq!(
        last_args(&args, "setFunctionBreakpoints")["breakpoints"][0]["condition"],
        "input != 0"
    );

    e.execute(Command::Condition {
        id: BreakpointId(1),
        condition: Some("x == 7".into()),
    })
    .await
    .unwrap();
    let bps = last_args(&args, "setBreakpoints")["breakpoints"].clone();
    assert_eq!(bps.as_array().unwrap().len(), 2);
    assert_eq!(bps[0]["condition"], "x == 7");
    e.execute(Command::Condition {
        id: BreakpointId(3),
        condition: None,
    })
    .await
    .unwrap();
    assert!(
        last_args(&args, "setFunctionBreakpoints")["breakpoints"][0]
            .get("condition")
            .is_none()
    );

    e.execute(Command::Run(None)).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await;
    assert_eq!(
        last_args(&args, "setBreakpoints")["breakpoints"][0]["condition"],
        "x == 7"
    );
    e.execute(Command::Condition {
        id: BreakpointId(1),
        condition: None,
    })
    .await
    .unwrap();
    assert!(
        last_args(&args, "setBreakpoints")["breakpoints"][0]
            .get("condition")
            .is_none()
    );
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn unsupported_conditions_refuse_launch_without_installing_breakpoints() {
    let (e, log) = engine();
    e.execute(conditional(source(5), "x > 2")).await.unwrap();
    let err = e.execute(Command::Run(None)).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("does not support conditional breakpoints"),
        "{err}"
    );
    assert_eq!(requests(&log, "launch"), 0);
    assert_eq!(requests(&log, "setBreakpoints"), 0);
    assert_eq!(breakpoints(&e).await[0].condition.as_deref(), Some("x > 2"));
    e.execute(Command::Condition {
        id: BreakpointId(1),
        condition: None,
    })
    .await
    .unwrap();
    e.execute(Command::Run(None)).await.unwrap();
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn unsupported_condition_edits_do_not_modify_active_breakpoints() {
    let (e, log, _, _) = start_stopped_with(json!({})).await;
    let before = breakpoints(&e).await;
    for cmd in [
        conditional(source(10), "x > 2"),
        Command::Condition {
            id: BreakpointId(1),
            condition: Some("x > 2".into()),
        },
    ] {
        assert!(
            e.execute(cmd)
                .await
                .unwrap_err()
                .to_string()
                .contains("does not support conditional breakpoints")
        );
        assert_eq!(breakpoints(&e).await, before);
    }
    assert_eq!(requests(&log, "setBreakpoints"), 1);
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn conditional_breakpoint_duplicates_require_explicit_edits() {
    let (e, _) = engine();
    let cmd = conditional(source(5), "x > 2");
    e.execute(cmd.clone()).await.unwrap();
    assert!(matches!(
        e.execute(cmd).await.unwrap(),
        Reply::BreakpointSet { new: false, .. }
    ));
    // A plain break never clears an existing condition.
    e.execute(Command::Break(source(5))).await.unwrap();
    assert!(
        e.execute(conditional(source(5), "x > 3"))
            .await
            .unwrap_err()
            .to_string()
            .contains("use `condition 1")
    );
    assert_eq!(breakpoints(&e).await.len(), 1);
    assert_eq!(breakpoints(&e).await[0].condition.as_deref(), Some("x > 2"));
    assert!(e.execute(conditional(source(10), "  ")).await.is_err());
    assert!(
        e.execute(Command::Condition {
            id: BreakpointId(9),
            condition: None
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("no breakpoint 9")
    );
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn failed_condition_sync_restores_desired_breakpoints() {
    let (e, _, _, _) = start_stopped_with(json!({"supportsConditionalBreakpoints": true})).await;
    let before = breakpoints(&e).await;
    for cmd in [
        conditional(source(10), "reject"),
        conditional(
            Location::Function(FunctionLocation::new("parse", None)),
            "reject",
        ),
        Command::Condition {
            id: BreakpointId(1),
            condition: Some("reject".into()),
        },
    ] {
        assert!(
            e.execute(cmd)
                .await
                .unwrap_err()
                .to_string()
                .contains("invalid condition")
        );
        assert_eq!(breakpoints(&e).await, before);
    }
    e.execute(Command::Quit).await.unwrap();
}

async fn watches(e: &EngineHandle) -> Vec<ddbg_core::watch::Watch> {
    let Reply::Watches(watches) = e.execute(Command::Watches).await.unwrap() else {
        panic!()
    };
    watches
}

#[tokio::test]
async fn watches_refresh_at_stops_without_expanding_and_errors_do_not_hide_stops() {
    let (e, log, args) = engine_with(json!({}));
    let mut rx = e.subscribe();
    assert!(e.execute(Command::Watch("  ".into())).await.is_err());
    e.execute(Command::Watch(" point ".into())).await.unwrap();
    e.execute(Command::Watch("missing".into())).await.unwrap();
    assert!(matches!(
        e.execute(Command::Watch("point".into())).await.unwrap(),
        Reply::WatchSet { new: false, .. }
    ));
    assert_eq!(watches(&e).await.len(), 2);
    assert!(watches(&e).await.iter().all(|w| w.result.is_none()));
    assert_eq!(requests(&log, "evaluate"), 0);
    e.execute(Command::Run(None)).await.unwrap();
    let DebugEvent::SessionStopped(info) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await
    else {
        panic!()
    };
    assert_eq!(info.watches.len(), 2);
    let value = info.watches[0].result.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(value.value, "Point");
    assert!(value.has_children);
    assert!(
        info.watches[1]
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .contains("out of scope")
    );
    assert_eq!(requests(&log, "evaluate"), 2);
    assert_eq!(requests(&log, "variables"), 0);
    assert_eq!(last_args(&args, "evaluate")["context"], "watch");
    assert_eq!(last_args(&args, "evaluate")["frameId"], 10);
    assert_eq!(watches(&e).await, info.watches);
    assert_eq!(requests(&log, "evaluate"), 2); // listing never invokes expressions
    e.execute(Command::Next).await.unwrap();
    let DebugEvent::WatchesChanged(cleared) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::WatchesChanged(_))).await
    else {
        panic!()
    };
    assert!(cleared.iter().all(|w| w.result.is_none()));
    let DebugEvent::SessionStopped(info) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await
    else {
        panic!()
    };
    assert!(info.watches[0].result.as_ref().unwrap().is_ok());
    assert_eq!(requests(&log, "evaluate"), 4);
    e.execute(Command::Continue).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionTerminated)).await;
    assert!(watches(&e).await.iter().all(|w| w.result.is_none()));
    e.execute(Command::Run(None)).await.unwrap();
    let DebugEvent::SessionStopped(info) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await
    else {
        panic!()
    };
    assert_eq!(info.watches.len(), 2);
    assert_eq!(info.watches[0].expression, "point");
    assert!(info.watches[0].result.as_ref().unwrap().is_ok());
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn watches_added_while_stopped_refresh_on_frame_thread_and_value_changes() {
    let (e, _, _, args) = start_stopped_with(json!({"supportsSetExpression": true})).await;
    let Reply::WatchSet { watch, new } = e
        .execute(Command::Watch("frame_local".into()))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(new);
    assert!(watch.result.unwrap().is_ok());
    e.execute(Command::Frame(FrameSelector::Index(1)))
        .await
        .unwrap();
    assert_eq!(last_args(&args, "evaluate")["frameId"], 11);
    assert!(watches(&e).await[0].result.as_ref().unwrap().is_err());
    e.execute(Command::Thread(ddbg_core::thread::ThreadId(1)))
        .await
        .unwrap();
    assert_eq!(last_args(&args, "evaluate")["frameId"], 10);
    assert!(watches(&e).await[0].result.as_ref().unwrap().is_ok());
    e.execute(Command::Watch("watch_value".into()))
        .await
        .unwrap();
    e.execute(Command::Set {
        target: "x".into(),
        value: "77".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        watches(&e).await[1]
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .value,
        "77"
    );
    e.execute(Command::Unwatch(ddbg_core::watch::WatchId(1)))
        .await
        .unwrap();
    assert_eq!(watches(&e).await.len(), 1);
    assert!(
        e.execute(Command::Unwatch(ddbg_core::watch::WatchId(1)))
            .await
            .unwrap_err()
            .to_string()
            .contains("no watch 1")
    );
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn watches_refresh_after_set_variable_and_repl_evaluation() {
    let (e, log, _, _) = start_stopped_with(json!({"supportsSetVariable": true})).await;
    e.execute(Command::Watch("watch_value".into()))
        .await
        .unwrap();
    e.execute(Command::Set {
        target: "x".into(),
        value: "12".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        watches(&e).await[0]
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .value,
        "12"
    );
    let before = requests(&log, "evaluate");
    e.execute(Command::Eval("debugger command".into()))
        .await
        .unwrap();
    assert!(requests(&log, "evaluate") > before + 1); // REPL evaluation plus watch refresh
    e.execute(Command::Quit).await.unwrap();
}

fn set(target: &str, value: &str) -> Command {
    Command::Set {
        target: target.into(),
        value: value.into(),
    }
}

#[tokio::test]
async fn set_prefers_set_expression() {
    let (e, log, mut rx, args) =
        start_stopped_with(json!({"supportsSetExpression": true, "supportsSetVariable": true}))
            .await;
    let Reply::Value(v, _) = e.execute(set("p.x", "7")).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.value, "7");
    assert_eq!(
        last_args(&args, "setExpression"),
        json!({"expression": "p.x", "value": "7", "frameId": 10})
    );
    assert_eq!(requests(&log, "setVariable"), 0);
    wait_for(&mut rx, |e| matches!(e, DebugEvent::VariablesChanged)).await;
}

#[tokio::test]
async fn set_falls_back_to_set_variable() {
    let (e, log, _rx, args) = start_stopped_with(json!({"supportsSetVariable": true})).await;
    e.execute(Command::Locals).await.unwrap();
    e.execute(set("x", "5")).await.unwrap();
    assert_eq!(
        last_args(&args, "setVariable"),
        json!({"variablesReference": 100, "name": "x", "value": "5"})
    );
    // The cache is invalidated: locals are fetched again.
    let before = requests(&log, "variables");
    e.execute(Command::Locals).await.unwrap();
    assert_eq!(requests(&log, "variables"), before + 1);

    // Members resolve through the parent's children (`p` → ref 300 → `x`).
    e.execute(set("p.x", "6")).await.unwrap();
    assert_eq!(
        last_args(&args, "setVariable"),
        json!({"variablesReference": 300, "name": "x", "value": "6"})
    );
    let err = e.execute(set("p.nope", "1")).await.unwrap_err();
    assert!(err.to_string().contains("no member"), "{err}");
    let err = e.execute(set("nope", "1")).await.unwrap_err();
    assert!(err.to_string().contains("no variable"), "{err}");
}

#[tokio::test]
async fn set_without_support_fails_and_set_variable_targets_scope() {
    let (e, _log, _rx, _) = start_stopped_with(json!({})).await;
    let err = e.execute(set("x", "1")).await.unwrap_err();
    assert!(err.to_string().contains("does not support"), "{err}");

    let (e, _log, _rx, args) = start_stopped_with(json!({"supportsSetVariable": true})).await;
    let cmd = Command::SetVariable {
        scope: ddbg_core::variable::VarRef(100),
        name: "x".into(),
        value: "9".into(),
    };
    let Reply::Value(v, _) = e.execute(cmd).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.value, "9");
    assert_eq!(last_args(&args, "setVariable")["variablesReference"], 100);
}

#[tokio::test]
async fn eval_uses_repl_context() {
    let (e, _log, _rx, args) = start_stopped_with(json!({})).await;
    e.execute(Command::Eval("x = 1".into())).await.unwrap();
    assert_eq!(last_args(&args, "evaluate")["context"], "repl");
    e.execute(Command::Print("x".into())).await.unwrap();
    assert_eq!(last_args(&args, "evaluate")["context"], "watch");
}

#[tokio::test]
async fn completions_map_ranges_and_fall_back_to_locals() {
    let (e, _log, _rx, args) =
        start_stopped_with(json!({"supportsCompletionsRequest": true})).await;
    let cmd = Command::Complete {
        text: "p.po".into(),
        column: 4,
    };
    let Reply::Completions(items) = e.execute(cmd).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        last_args(&args, "completions"),
        json!({"text": "p.po", "column": 5, "frameId": 10})
    );
    // No range: replace the identifier at the cursor.
    assert_eq!(
        (items[0].text.as_str(), items[0].start, items[0].length),
        ("point", 2, 2)
    );
    // 1-based `start` from the adapter.
    assert_eq!((items[1].start, items[1].length), (2, 2));

    let (e, log, _rx, _) = start_stopped_with(json!({})).await;
    let Reply::Completions(items) = e
        .execute(Command::Complete {
            text: "1 + ".into(),
            column: 4,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(items.len(), 1);
    assert_eq!(
        (items[0].text.as_str(), items[0].start, items[0].length),
        ("x", 4, 0)
    );
    assert_eq!(requests(&log, "completions"), 0);
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

fn function_bp(name: &str, file: Option<&str>) -> Command {
    Command::Break(Location::Function(FunctionLocation::new(
        name,
        file.map(PathBuf::from),
    )))
}

#[tokio::test]
async fn function_breakpoint_in_scope_stops() {
    let (e, log) = engine();
    let mut rx = e.subscribe();
    e.execute(function_bp("main", Some("src/main.rs")))
        .await
        .unwrap();
    e.execute(Command::Run(None)).await.unwrap();
    let DebugEvent::SessionStopped(info) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await
    else {
        unreachable!()
    };
    assert_eq!(info.reason, StopReason::Breakpoint(vec![BreakpointId(1)]));
    assert_eq!(requests(&log, "setFunctionBreakpoints"), 1);
    assert_eq!(requests(&log, "setBreakpoints"), 0);

    let Reply::Breakpoints(bps) = e.execute(Command::Breakpoints).await.unwrap() else {
        panic!()
    };
    assert!(bps[0].verified);
    assert_eq!(bps[0].message, None);

    // Adding another function breakpoint re-sends the whole set.
    e.execute(function_bp("helper", None)).await.unwrap();
    assert_eq!(requests(&log, "setFunctionBreakpoints"), 2);
    e.execute(Command::DeleteBreakpoint(BreakpointId(2)))
        .await
        .unwrap();
    assert_eq!(requests(&log, "setFunctionBreakpoints"), 3);
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test]
async fn function_breakpoint_out_of_scope_is_skipped() {
    let (e, log) = engine();
    let mut rx = e.subscribe();
    e.execute(function_bp("main", Some("other.rs")))
        .await
        .unwrap();
    e.execute(Command::Run(None)).await.unwrap();
    let ev = wait_for(&mut rx, |e| {
        matches!(
            e,
            DebugEvent::SessionStopped(_) | DebugEvent::SessionTerminated
        )
    })
    .await;
    assert!(matches!(ev, DebugEvent::SessionTerminated), "got {ev:?}");
    assert_eq!(requests(&log, "continue"), 1);
}

#[tokio::test]
async fn exception_stop_fetches_exception_info() {
    let (e, log, mut rx) = start_stopped_at_breakpoint().await;
    assert_eq!(requests(&log, "exceptionInfo"), 0);
    e.execute(Command::Step).await.unwrap();
    let ev = wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await;
    let DebugEvent::SessionStopped(info) = ev else {
        unreachable!()
    };
    assert_eq!(info.reason, StopReason::Exception(Some("Unhandled".into())));
    let ex = info.exception.expect("exception info");
    assert_eq!(ex.id, "System.InvalidOperationException");
    assert_eq!(
        ex.type_name.as_deref(),
        Some("System.InvalidOperationException")
    );
    assert_eq!(ex.message.as_deref(), Some("Outer failed"));
    assert_eq!(ex.break_mode.as_deref(), Some("unhandled"));
    assert_eq!(ex.inner.len(), 1);
    assert_eq!(
        ex.inner[0].type_name.as_deref(),
        Some("System.ArgumentException")
    );
    assert_eq!(ex.inner[0].message.as_deref(), Some("Bad arg"));
    assert_eq!(requests(&log, "exceptionInfo"), 1);
}
