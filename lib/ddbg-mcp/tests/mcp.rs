//! End-to-end tests of the MCP server over an in-memory transport.
//!
//! Tests that need a debug adapter are ignored by default: run them with
//! `cargo test -p ddbg-mcp -- --ignored`. The Python test uses
//! `$DDBG_PYTHON` (default `python3`) and skips itself without debugpy.

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use ddbg_mcp::DdbgServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ()>;

async fn connect() -> Client {
    let (server, client) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let service = DdbgServer::new().serve(server).await?;
        service.waiting().await?;
        anyhow::Ok(())
    });
    ().serve(client).await.expect("client connects")
}

async fn call_raw(client: &Client, tool: &str, args: Value) -> CallToolResult {
    let Value::Object(args) = args else {
        panic!("arguments must be an object")
    };
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    assert_ne!(result.is_error, Some(true), "{tool} failed: {result:?}");
    result
}

async fn call(client: &Client, tool: &str, args: Value) -> Value {
    call_raw(client, tool, args)
        .await
        .structured_content
        .unwrap_or(Value::Null)
}

/// The human-readable summary: the first text block.
fn summary(result: &CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map_or("", |t| t.text.as_str())
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[tokio::test]
async fn lists_tools_and_identifies_as_ddbg() {
    let client = connect().await;
    let info = client.peer_info().expect("server info");
    assert_eq!(info.server_info.as_ref().unwrap().name, "ddbg");
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in [
        "start_session",
        "run",
        "continue",
        "set_breakpoint",
        "evaluate",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn tools_require_a_session() {
    let client = connect().await;
    let result = client
        .call_tool(CallToolRequestParams::new("backtrace"))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn start_session_reports_project() {
    let client = connect().await;
    let info = call(
        &client,
        "start_session",
        json!({ "cwd": fixture("hello-python") }),
    )
    .await;
    assert_eq!(info["project"], "Python");
    assert!(!info["notes"].as_array().unwrap().is_empty(), "{info}");
    call(&client, "end_session", json!({})).await;
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap"]
async fn debug_rust_program() {
    let dir = fixture("hello-rust");
    let status = Process::new(env!("CARGO"))
        .args(["build", "--quiet", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .status()
        .unwrap();
    assert!(status.success());

    let client = connect().await;
    let info = call(&client, "start_session", json!({ "cwd": dir })).await;
    assert_eq!(info["project"], "Rust");
    assert!(info["program"].as_str().unwrap().ends_with("hello-rust"));

    let bp = call(
        &client,
        "set_breakpoint",
        json!({ "file": "src/main.rs", "line": 14 }),
    )
    .await;
    assert_eq!(bp["id"], 1);

    let result = call_raw(&client, "run", json!({})).await;
    let stop = result.structured_content.clone().unwrap();
    assert_eq!(stop["state"], "stopped", "{stop}");
    assert_eq!(stop["frame"]["line"], 14);
    assert!(summary(&result).contains("add(point.x"), "{result:?}");

    let result = call_raw(&client, "evaluate", json!({ "expression": "point.x" })).await;
    assert_eq!(result.structured_content.as_ref().unwrap()["value"], "3");
    assert!(summary(&result).contains('3'));

    let bt = call(&client, "backtrace", json!({})).await;
    assert!(bt["frames"][0]["name"].as_str().unwrap().contains("main"));

    let exit = call(&client, "continue", json!({})).await;
    assert_eq!(exit, json!({ "state": "exited", "exit_code": 0 }));

    let output = call(&client, "get_output", json!({})).await;
    assert!(output["stdout"].as_str().unwrap().contains("hello: 7"));
    let output = call(&client, "get_output", json!({})).await;
    assert_eq!(output["stdout"], "");

    call(&client, "end_session", json!({})).await;
    client.cancel().await.unwrap();
}

/// Build a C program that never exits.
fn build_spinner() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ddbg-mcp-spin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("spin.c");
    std::fs::write(
        &src,
        "#include <unistd.h>\nint main(void) { for (;;) sleep(1); }\n",
    )
    .unwrap();
    let bin = dir.join("spin");
    let status = Process::new("cc")
        .args(["-g", "-o"])
        .arg(&bin)
        .arg(&src)
        .status()
        .expect("cc");
    assert!(status.success());
    bin
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap and cc"]
async fn pause_interrupts_a_waiting_call() {
    let bin = build_spinner();
    let client = connect().await;
    call(
        &client,
        "start_session",
        json!({
            "cwd": bin.parent().map(Path::to_path_buf),
            "program": bin,
            "no_detect": true,
            "adapter": "lldb-dap",
        }),
    )
    .await;

    let running = call(&client, "run", json!({ "timeout_ms": 1000 })).await;
    assert_eq!(running["state"], "running", "{running}");

    let wait = call(&client, "wait", json!({ "timeout_ms": 20000 }));
    let pause = async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        call_raw(&client, "pause", json!({})).await
    };
    let (stopped, pause) = tokio::join!(wait, pause);
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    // lldb-dap on macOS reports interrupting a sleeping process as SIGSTOP.
    assert!(
        stopped["reason"] == "pause"
            || stopped["description"]
                .as_str()
                .is_some_and(|d| d.contains("SIGSTOP")),
        "{stopped}"
    );
    assert!(summary(&pause).contains("Pause requested"), "{pause:?}");

    // `end_session` also works while a call waits.
    call(&client, "continue", json!({ "timeout_ms": 500 })).await;
    let waiting = call(&client, "wait", json!({ "timeout_ms": 20000 }));
    let end = async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        call_raw(&client, "end_session", json!({})).await
    };
    let (_, end) = tokio::join!(waiting, end);
    assert!(summary(&end).contains("Session ended"));
    client.cancel().await.unwrap();
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
    let client = connect().await;
    call(
        &client,
        "start_session",
        json!({
            "cwd": fixture("hello-python"),
            "program": "main.py",
            "adapter": format!("{python} -m debugpy.adapter"),
        }),
    )
    .await;
    call(
        &client,
        "set_breakpoint",
        json!({ "file": "main.py", "line": 18 }),
    )
    .await;
    let stop = call(&client, "run", json!({})).await;
    assert_eq!(stop["state"], "stopped", "{stop}");
    assert_eq!(stop["frame"]["line"], 18);

    let value = call(&client, "evaluate", json!({ "expression": "point.x" })).await;
    assert_eq!(value["value"], "3");

    let exit = call(&client, "continue", json!({})).await;
    assert_eq!(exit["state"], "exited", "{exit}");
    let output = call(&client, "get_output", json!({})).await;
    assert!(output["stdout"].as_str().unwrap().contains("hello: 7"));

    call(&client, "end_session", json!({})).await;
    client.cancel().await.unwrap();
}
