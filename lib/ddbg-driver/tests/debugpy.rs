//! Real pytest debugging through debugpy. Uses `$DDBG_PYTHON` (default
//! `python3`) and skips when it cannot import both `debugpy` and `pytest`.

mod common;

use common::line_of;
use std::path::PathBuf;

use ddbg_driver::{Debugger, StopReason, TestOutcome};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-python")
}

fn python() -> String {
    std::env::var("DDBG_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn have_modules() -> bool {
    std::process::Command::new(python())
        .args(["-c", "import debugpy, pytest"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires debugpy and pytest"]
async fn pytest_test_stops_at_start_and_reports_outcome() {
    if !have_modules() {
        eprintln!(
            "skipping: debugpy or pytest is not installed for {}",
            python()
        );
        return;
    }
    let adapter = format!("{} -m debugpy.adapter", python());
    let dbg = Debugger::from_cli(["--adapter", adapter.as_str()], fixture()).unwrap();
    debug_failing_test(dbg).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires uv"]
async fn pytest_test_debugs_through_uv() {
    if std::process::Command::new("uv")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: uv not found");
        return;
    }
    // A uv project outside the source tree, since `uv run` writes `uv.lock`.
    let dir = std::env::temp_dir().join(format!("ddbg-uv-debug-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    for f in ["main.py", "pyproject.toml", "tests/test_main.py"] {
        std::fs::copy(fixture().join(f), dir.join(f)).unwrap();
    }
    let pyproject = std::fs::read_to_string(dir.join("pyproject.toml")).unwrap();
    std::fs::write(dir.join("pyproject.toml"), pyproject + "\n[tool.uv]\n").unwrap();
    let dbg = Debugger::from_cli(Vec::<String>::new(), &dir).unwrap();
    debug_failing_test(dbg).await;
    let _ = std::fs::remove_dir_all(&dir);
}

async fn debug_failing_test(mut dbg: Debugger) {
    let tests = dbg.tests(Some("test_fails")).await.unwrap();
    assert_eq!(tests.len(), 1);
    let stop = dbg
        .test_debug(tests[0].name(), true)
        .await
        .unwrap()
        .into_stopped()
        .unwrap();
    assert_eq!(
        stop.frame.unwrap().line,
        line_of(fixture().join("tests/test_main.py"), "failing-assert")
    );
    // The assertion escapes user code, so the debugger pauses on it.
    let failure = dbg.cont().await.unwrap().into_stopped().unwrap();
    assert!(matches!(failure.reason, StopReason::Exception(_)));
    assert_eq!(
        failure.frame.unwrap().line,
        line_of(fixture().join("tests/test_main.py"), "failing-assert")
    );
    dbg.cont().await.unwrap().into_exited().unwrap();
    let result = dbg.debug_test_result().unwrap();
    assert_eq!(result.outcome, Some(TestOutcome::Failed));
    assert_eq!(result.counts.unwrap().total, 1);
    dbg.quit().await.unwrap();
}
