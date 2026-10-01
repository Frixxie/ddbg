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

async fn fake(mut io: DuplexStream) {
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
            "initialize" => json!({"supportsConfigurationDoneRequest": true}),
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
        send(&mut io, Message::Response(resp)).await;
        let events: &[(&str, Value)] = match req.command.as_str() {
            "launch" => &[("initialized", json!({}))],
            "configurationDone" => &[(
                "stopped",
                json!({"reason": "breakpoint", "threadId": 1, "hitBreakpointIds": [1]}),
            )],
            "next" => &[("stopped", json!({"reason": "step", "threadId": 1}))],
            "continue" => &[
                ("output", json!({"category": "stdout", "output": "done\n"})),
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
            send(&mut io, Message::Event(ev)).await;
        }
    }
}

fn debugger() -> Debugger {
    let config = EngineConfig {
        adapter: Arc::new(LldbDapAdapter::default()),
        cwd: "/".into(),
        target: Some(LaunchTarget::new("/bin/app", vec![])),
    };
    let engine = spawn_with_connector(
        config,
        Box::new(|_| {
            let (a, b) = tokio::io::duplex(64 * 1024);
            let (r, w) = tokio::io::split(a);
            let (client, incoming) = DapClient::connect(r, w);
            tokio::spawn(fake(b));
            Ok(Connection {
                client,
                incoming,
                child: None,
            })
        }),
    );
    Debugger::from_engine(engine, "/")
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
