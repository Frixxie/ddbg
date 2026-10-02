//! End-to-end tests for C (`lldb-dap`) and Python (`debugpy`).
//!
//! Ignored by default: run with `cargo test -p ddbg-core -- --ignored`.
//! The Python test uses `$DDBG_PYTHON` (default `python3`) and skips itself
//! when that interpreter cannot `import debugpy`.

use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::Arc;
use std::time::Duration;

use ddbg_core::adapter::{DebugAdapter, DebugpyAdapter, LldbDapAdapter};
use ddbg_core::breakpoint::{BreakpointId, SourceLocation};
use ddbg_core::command::{Command, Location, Reply};
use ddbg_core::engine::{self, EngineConfig, EngineHandle};
use ddbg_core::session::StopReason;
use ddbg_core::{DebugEvent, LaunchTarget};
use tokio::sync::broadcast;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
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

async fn stopped(rx: &mut broadcast::Receiver<DebugEvent>) -> ddbg_core::event::StopInfo {
    match wait_for(rx, |e| matches!(e, DebugEvent::SessionStopped(_))).await {
        DebugEvent::SessionStopped(info) => *info,
        _ => unreachable!(),
    }
}

/// Break at `file:line`, check locals and `print`, step into `add`, finish.
async fn exercise(
    adapter: Arc<dyn DebugAdapter>,
    dir: PathBuf,
    program: PathBuf,
    file: &str,
    line: u32,
) {
    let mut target = LaunchTarget::new(program, vec![]);
    target.cwd = dir.clone();
    let e: EngineHandle = engine::spawn(EngineConfig {
        adapter,
        cwd: dir,
        target: Some(target),
    });
    let mut rx = e.subscribe();

    e.execute(Command::Break(Location::Source(SourceLocation::new(
        file, line,
    ))))
    .await
    .unwrap();
    e.execute(Command::Run(None)).await.unwrap();

    let info = stopped(&mut rx).await;
    assert_eq!(info.reason, StopReason::Breakpoint(vec![BreakpointId(1)]));
    let frame = info.frame.unwrap();
    assert_eq!(frame.line, line);
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

    e.execute(Command::Step).await.unwrap();
    let info = stopped(&mut rx).await;
    assert!(info.frame.unwrap().name.contains("add"));

    e.execute(Command::Continue).await.unwrap();
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionExited(0))).await;
    wait_for(&mut rx, |e| matches!(e, DebugEvent::SessionTerminated)).await;
    e.execute(Command::Quit).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap and a C compiler"]
async fn debug_c_executable() {
    let dir = fixture("hello-c");
    let status = Process::new("make")
        .args(["-s", "-C"])
        .arg(&dir)
        .status()
        .expect("make");
    assert!(status.success());
    exercise(
        Arc::new(LldbDapAdapter::default()),
        dir.clone(),
        dir.join("hello-c"),
        "main.c",
        16,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires debugpy"]
async fn debug_python_script() {
    let python = std::env::var("DDBG_PYTHON").unwrap_or_else(|_| "python3".into());
    let has_debugpy = Process::new(&python)
        .args(["-c", "import debugpy"])
        .status()
        .is_ok_and(|s| s.success());
    if !has_debugpy {
        eprintln!("skipping: {python} cannot import debugpy");
        return;
    }
    let dir = fixture("hello-python");
    exercise(
        Arc::new(DebugpyAdapter::with_python(python)),
        dir.clone(),
        dir.join("main.py"),
        "main.py",
        18,
    )
    .await;
}
