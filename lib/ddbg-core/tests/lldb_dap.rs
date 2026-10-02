//! End-to-end tests against a real `lldb-dap`.
//!
//! Ignored by default: run with `cargo test -p ddbg-core -- --ignored`.

use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::Arc;
use std::time::Duration;

use ddbg_core::adapter::LldbDapAdapter;
use ddbg_core::breakpoint::{BreakpointId, FunctionLocation, SourceLocation};
use ddbg_core::command::{Command, Location, Reply};
use ddbg_core::engine::{self, EngineConfig};
use ddbg_core::session::StopReason;
use ddbg_core::{DebugEvent, LaunchTarget};
use tokio::sync::broadcast;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-rust")
}

fn build_fixture() -> PathBuf {
    let dir = fixture();
    let status = Process::new(env!("CARGO"))
        .args(["build", "--quiet", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .status()
        .expect("cargo build");
    assert!(status.success());
    dir.join("target/debug/hello-rust")
}

async fn wait_for(
    rx: &mut broadcast::Receiver<DebugEvent>,
    pred: impl Fn(&DebugEvent) -> bool,
) -> DebugEvent {
    tokio::time::timeout(Duration::from_secs(30), async {
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap"]
async fn debug_rust_executable() {
    let program = build_fixture();
    let dir = fixture().canonicalize().unwrap();
    let mut target = LaunchTarget::new(&program, vec![]);
    target.cwd = dir.clone();

    let e = engine::spawn(EngineConfig {
        adapter: Arc::new(LldbDapAdapter::for_rust()),
        cwd: dir.clone(),
        target: Some(target),
    });
    let mut rx = e.subscribe();

    e.execute(Command::Break(Location::Source(SourceLocation::new(
        "src/main.rs",
        14,
    ))))
    .await
    .unwrap();
    e.execute(Command::Run(None)).await.unwrap();

    let DebugEvent::SessionStopped(info) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await
    else {
        unreachable!()
    };
    assert_eq!(info.reason, StopReason::Breakpoint(vec![BreakpointId(1)]));
    let frame = info.frame.unwrap();
    assert_eq!(frame.line, 14);
    assert!(frame.name.contains("main"), "{}", frame.name);

    let Reply::Locals(scopes) = e.execute(Command::Locals).await.unwrap() else {
        panic!()
    };
    let names: Vec<_> = scopes[0]
        .variables
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert!(names.contains(&"point"), "{names:?}");
    assert!(names.contains(&"greeting"), "{names:?}");

    let Reply::Value(v, _) = e.execute(Command::Print("point.x".into())).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.value, "3");

    let Reply::Value(v, children) = e.execute(Command::Print("point".into())).await.unwrap() else {
        panic!()
    };
    assert_eq!(children.len(), 2, "{v:?} {children:?}");

    let set = |target: &str, value: &str| Command::Set {
        target: target.into(),
        value: value.into(),
    };
    let Reply::Value(v, _) = e.execute(set("point.x", "42")).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.value, "42");
    let Reply::Value(v, _) = e.execute(Command::Print("point.x".into())).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.value, "42");
    e.execute(set("point.x", "3")).await.unwrap();

    let Reply::Completions(items) = e
        .execute(Command::Complete {
            text: "poi".into(),
            column: 3,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(items.iter().any(|c| c.text == "point"), "{items:?}");

    // Step into `add`, check the backtrace, then step out.
    e.execute(Command::Step).await.unwrap();
    let DebugEvent::SessionStopped(info) =
        wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await
    else {
        unreachable!()
    };
    assert!(info.frame.unwrap().name.contains("add"));
    let Reply::Backtrace { frames, .. } = e.execute(Command::Backtrace).await.unwrap() else {
        panic!()
    };
    assert!(frames[1].name.contains("main"));

    e.execute(Command::Finish).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await;
    e.execute(Command::Next).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await;

    e.execute(Command::Continue).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionExited(0))).await;
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionTerminated)).await;
    e.execute(Command::Quit).await.unwrap();
}

async fn run_with_function_breakpoint(file: Option<&str>) -> DebugEvent {
    let program = build_fixture();
    let dir = fixture().canonicalize().unwrap();
    let mut target = LaunchTarget::new(&program, vec![]);
    target.cwd = dir.clone();
    let e = engine::spawn(EngineConfig {
        adapter: Arc::new(LldbDapAdapter::for_rust()),
        cwd: dir,
        target: Some(target),
    });
    let mut rx = e.subscribe();
    e.execute(Command::Break(Location::Function(FunctionLocation::new(
        "add",
        file.map(Into::into),
    ))))
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
    e.execute(Command::Quit).await.unwrap();
    ev
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap"]
async fn function_breakpoint_scoped_to_file() {
    let DebugEvent::SessionStopped(info) = run_with_function_breakpoint(Some("src/main.rs")).await
    else {
        panic!("expected a stop in `add`")
    };
    assert_eq!(info.reason, StopReason::Breakpoint(vec![BreakpointId(1)]));
    assert!(info.frame.unwrap().name.contains("add"));

    let ev = run_with_function_breakpoint(Some("other.rs")).await;
    assert!(matches!(ev, DebugEvent::SessionTerminated), "{ev:?}");
}
