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
        "attach",
        "detach",
        "continue",
        "set_breakpoint",
        "set_breakpoint_condition",
        "add_watch",
        "list_watches",
        "remove_watch",
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
async fn attach_validates_pid_and_detach_requires_an_active_process() {
    let client = connect().await;
    call(&client, "start_session", json!({"no_detect": true})).await;
    for (tool, args) in [("attach", json!({"pid": 0})), ("detach", json!({}))] {
        let result = client
            .call_tool(
                CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true), "{result:?}");
    }
    call(&client, "end_session", json!({})).await;
    client.cancel().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires lldb-dap, cc, and OS permission to attach"]
async fn detach_interrupts_wait_and_end_session_preserves_attached_process() {
    use std::time::Duration;
    let dir = std::env::temp_dir().join(format!("ddbg-mcp-attach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("attach-native");
    assert!(
        Process::new("cc")
            .args(["-g", "-O0"])
            .arg(fixture("attach-native").join("main.c"))
            .arg("-o")
            .arg(&bin)
            .status()
            .unwrap()
            .success()
    );
    let mut child = tokio::process::Command::new(&bin)
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let client = connect().await;
    call(
        &client,
        "start_session",
        json!({
            "cwd": dir, "no_detect": true, "adapter": "lldb-dap"
        }),
    )
    .await;
    let attached = call(
        &client,
        "attach",
        json!({"pid": child.id().unwrap(), "timeout_ms": 100}),
    )
    .await;
    if attached["state"] == "stopped" {
        call(&client, "continue", json!({"timeout_ms": 100})).await;
    } else {
        assert_eq!(attached["state"], "running", "{attached}");
    }
    let waiting = call(&client, "wait", json!({"timeout_ms": 10000}));
    let detach = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        call_raw(&client, "detach", json!({})).await
    };
    let (halt, detached) = tokio::join!(waiting, detach);
    assert_eq!(halt["state"], "terminated", "{halt}");
    assert!(summary(&detached).contains("left running"));
    assert!(child.try_wait().unwrap().is_none());
    call(
        &client,
        "attach",
        json!({"pid": child.id().unwrap(), "timeout_ms": 100}),
    )
    .await;
    call(&client, "end_session", json!({})).await;
    assert!(child.try_wait().unwrap().is_none());
    child.kill().await.unwrap();
    child.wait().await.unwrap();
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

#[tokio::test]
async fn list_results_are_objects() {
    let client = connect().await;
    call(
        &client,
        "start_session",
        json!({ "cwd": fixture("hello-python") }),
    )
    .await;
    call(
        &client,
        "set_breakpoint",
        json!({ "file": "main.py", "line": 18 }),
    )
    .await;
    let bps = call(&client, "list_breakpoints", json!({})).await;
    assert_eq!(bps["breakpoints"][0]["id"], 1, "{bps}");
    call(&client, "end_session", json!({})).await;
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn conditional_breakpoints_can_be_configured_and_edited_before_launch() {
    let client = connect().await;
    call(
        &client,
        "start_session",
        json!({ "cwd": fixture("hello-python") }),
    )
    .await;
    let result = call_raw(
        &client,
        "set_breakpoint",
        json!({ "file": "main.py", "line": 18, "condition": "count > 2" }),
    )
    .await;
    assert_eq!(
        result.structured_content.as_ref().unwrap()["condition"],
        "count > 2"
    );
    assert!(summary(&result).contains("if count > 2"));
    let edited = call(
        &client,
        "set_breakpoint_condition",
        json!({ "id": 1, "condition": "count == 7" }),
    )
    .await;
    assert_eq!(edited["condition"], "count == 7");
    let bps = call(&client, "list_breakpoints", json!({})).await;
    assert_eq!(bps["breakpoints"][0]["condition"], "count == 7");
    let cleared = call(&client, "set_breakpoint_condition", json!({ "id": 1 })).await;
    assert!(cleared["condition"].is_null());
    let function = call(
        &client,
        "set_breakpoint",
        json!({ "function": "add", "condition": "a == 3" }),
    )
    .await;
    assert_eq!(function["kind"], "function");
    assert_eq!(function["condition"], "a == 3");
    call(&client, "end_session", json!({})).await;
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn watches_can_be_managed_before_launch() {
    let client = connect().await;
    call(
        &client,
        "start_session",
        json!({"cwd": fixture("hello-python")}),
    )
    .await;
    let w = call(&client, "add_watch", json!({"expression": "x + 1"})).await;
    assert_eq!(w["id"], 1);
    assert_eq!(w["expression"], "x + 1");
    assert!(w["value"].is_null());
    assert!(w["error"].is_null());
    call(&client, "add_watch", json!({"expression": "x + 1"})).await;
    let watches = call(&client, "list_watches", json!({})).await;
    assert_eq!(watches["watches"].as_array().unwrap().len(), 1);
    call(&client, "remove_watch", json!({"id": 1})).await;
    let watches = call(&client, "list_watches", json!({})).await;
    assert!(watches["watches"].as_array().unwrap().is_empty());
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

    call(&client, "add_watch", json!({"expression": "point.x"})).await;
    call(
        &client,
        "add_watch",
        json!({"expression": "missing_variable"}),
    )
    .await;

    let result = call_raw(&client, "run", json!({})).await;
    let stop = result.structured_content.clone().unwrap();
    assert_eq!(stop["state"], "stopped", "{stop}");
    assert_eq!(stop["frame"]["line"], 14);
    assert_eq!(stop["watches"][0]["value"], "3");
    assert!(stop["watches"][1]["error"].is_string());
    assert!(summary(&result).contains("add(point.x"), "{result:?}");
    assert!(summary(&result).contains("Watch 1: point.x = 3"));

    let result = call_raw(&client, "evaluate", json!({ "expression": "point.x" })).await;
    assert_eq!(result.structured_content.as_ref().unwrap()["value"], "3");
    assert!(summary(&result).contains('3'));

    let bt = call(&client, "backtrace", json!({})).await;
    assert!(bt["frames"][0]["name"].as_str().unwrap().contains("main"));

    let locals = call(&client, "locals", json!({})).await;
    assert!(locals["scopes"].is_array(), "{locals}");

    let exit = call(&client, "continue", json!({})).await;
    assert_eq!(exit, json!({ "state": "exited", "exit_code": 0 }));

    let output = call(&client, "get_output", json!({})).await;
    assert!(output["stdout"].as_str().unwrap().contains("hello: 7"));
    let output = call(&client, "get_output", json!({})).await;
    assert_eq!(output["stdout"], "");
    let repeated_exit = call(&client, "wait", json!({"timeout_ms": 0})).await;
    assert_eq!(repeated_exit, exit);

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

    call(&client, "kill", json!({})).await;
    for _ in 0..2 {
        let terminal = call(&client, "wait", json!({"timeout_ms": 0})).await;
        assert_eq!(terminal["state"], "terminated", "{terminal}");
    }
    let restarted = call(&client, "run", json!({"timeout_ms": 0})).await;
    assert_eq!(restarted["state"], "running", "{restarted}");
    let stopped = call(&client, "pause", json!({"timeout_ms": 5000})).await;
    assert_eq!(stopped["state"], "stopped", "{stopped}");

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
