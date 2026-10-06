//! Real PID attach: verify that detach and quit leave an external process running.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ddbg_core::adapter::LldbDapAdapter;
use ddbg_core::{AttachTarget, Command, EngineConfig, Reply, engine};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap, cc, and OS permission to attach"]
async fn native_attach_detach_and_quit_preserve_external_process() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/attach-native")
        .canonicalize()
        .unwrap();
    let program = dir.join("attach-native");
    assert!(
        std::process::Command::new("cc")
            .args(["-g", "-O0"])
            .arg(dir.join("main.c"))
            .arg("-o")
            .arg(&program)
            .status()
            .unwrap()
            .success()
    );
    let mut child = tokio::process::Command::new(program)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    assert_eq!(output.next_line().await.unwrap().as_deref(), Some("alive"));

    for quit in [false, true] {
        let e = engine::spawn(EngineConfig {
            adapter: Arc::new(LldbDapAdapter::default()),
            cwd: dir.clone(),
            target: None,
        });
        let mut halt = e.subscribe_halt();
        assert_eq!(
            e.execute(Command::Attach(AttachTarget { pid }))
                .await
                .unwrap(),
            Reply::Attached(pid)
        );
        if halt.borrow().is_none() {
            // LLDB may continue automatically after attachment.
            let _ = e.execute(Command::Pause).await;
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if halt.borrow().is_some() {
                    break;
                }
                halt.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            *halt.borrow(),
            Some(ddbg_core::DebugEvent::SessionStopped(_))
        ));
        // Empty the pipe while stopped so the next heartbeat proves resumption.
        while let Ok(line) =
            tokio::time::timeout(Duration::from_millis(100), output.next_line()).await
        {
            assert!(line.unwrap().is_some());
        }
        e.execute(if quit { Command::Quit } else { Command::Detach })
            .await
            .unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "external process was killed"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), output.next_line())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("alive")
        );
        if !quit {
            e.execute(Command::Quit).await.unwrap();
        }
    }
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}
