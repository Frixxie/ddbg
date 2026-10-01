//! End-to-end driver tests against a real `lldb-dap`.
//!
//! Ignored by default: run with `cargo test -p ddbg-driver -- --ignored`.

use std::path::PathBuf;
use std::process::Command as Process;

use ddbg_driver::{Debugger, StopReason};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-rust")
}

fn build_fixture() {
    let status = Process::new(env!("CARGO"))
        .args(["build", "--quiet", "--manifest-path"])
        .arg(fixture().join("Cargo.toml"))
        .status()
        .expect("cargo build");
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap"]
async fn typed_api() {
    build_fixture();
    let mut dbg = Debugger::from_cli(["--", "target/debug/hello-rust"], fixture()).unwrap();

    let bp = dbg.break_at("src/main.rs", 14).await.unwrap();
    let stop = dbg.run().await.unwrap().into_stopped().unwrap();
    assert_eq!(stop.reason, StopReason::Breakpoint(vec![bp.id]));
    assert_eq!(stop.frame.unwrap().line, 14);

    assert_eq!(dbg.print("point.x").await.unwrap().value, "3");
    let (_, children) = dbg.print_with_children("point").await.unwrap();
    assert_eq!(children.len(), 2);
    assert!(dbg.local("greeting").await.is_ok());

    let stop = dbg.step().await.unwrap().into_stopped().unwrap();
    assert!(stop.frame.unwrap().name.contains("add"));
    assert!(dbg.backtrace().await.unwrap()[1].name.contains("main"));
    dbg.finish().await.unwrap().into_stopped().unwrap();

    assert_eq!(dbg.cont().await.unwrap().into_exited().unwrap(), 0);
    assert_eq!(dbg.stdout().trim_end(), "hello: 7");
    dbg.quit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap"]
async fn text_api_matches_repl() {
    build_fixture();
    let mut dbg = Debugger::from_cli(["--", "target/debug/hello-rust"], fixture()).unwrap();

    let out = dbg.exec("b src/main.rs:add").await.unwrap();
    assert!(out.starts_with("Breakpoint 1"), "{out}");
    let out = dbg.exec("run").await.unwrap();
    assert!(
        out.contains("Breakpoint 1, hello_rust::add") && out.contains("main.rs"),
        "{out}"
    );
    let out = dbg.exec("p a").await.unwrap();
    assert_eq!(out.trim(), "3");
    assert!(dbg.exec("bogus").await.is_err());

    let transcript = dbg.script("delete 1\nc").await.unwrap();
    assert!(
        transcript.contains("Process exited normally."),
        "{transcript}"
    );
    dbg.quit().await.unwrap();
}
