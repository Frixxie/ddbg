//! End-to-end against `fixtures/hello-python` (requires pytest for
//! `$DDBG_PYTHON`, default `python3`; skipped when it cannot be imported).

use std::path::PathBuf;

use ddbg_core::DebugTarget;
use ddbg_test::{PythonTestProvider, TestOutcome, TestProvider, TestQuery};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-python")
}

fn python() -> String {
    std::env::var("DDBG_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn have_pytest() -> bool {
    std::process::Command::new(python())
        .args(["-c", "import pytest"])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn discover_run_and_debug_target() {
    if !have_pytest() {
        eprintln!("skipping: pytest is not installed for {}", python());
        return;
    }
    let p = PythonTestProvider::new(fixture()).with_python(python());

    let all = p.discover(&TestQuery::default()).await.unwrap();
    let names: Vec<_> = all.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "tests/test_main.py::test_adds",
            "tests/test_main.py::test_fails"
        ]
    );
    assert_eq!(all[0].line, Some(5));

    let only = p.discover(&TestQuery::new("ADDS")).await.unwrap();
    assert_eq!(only.len(), 1);

    let ids: Vec<_> = all.iter().map(|t| t.id.clone()).collect();
    let run = p.run(&ids).await.unwrap();
    assert_eq!(run.results[0].outcome, TestOutcome::Passed);
    assert_eq!(run.results[1].outcome, TestOutcome::Failed);

    let DebugTarget::Launch(t) = p.debug_target(&all[0].id).await.unwrap() else {
        panic!("expected launch");
    };
    assert_eq!(t.args[..2], ["pytest", "tests/test_main.py::test_adds"]);
}

fn have_uv() -> bool {
    std::process::Command::new("uv")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Copy of the fixture as a uv project, so `uv run` writes `uv.lock` and
/// `.venv` outside the source tree.
fn uv_fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ddbg-uv-fixture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    for f in ["main.py", "pyproject.toml", "tests/test_main.py"] {
        std::fs::copy(fixture().join(f), dir.join(f)).unwrap();
    }
    dir
}

#[tokio::test]
async fn runs_through_uv() {
    if !have_uv() {
        eprintln!("skipping: uv not found");
        return;
    }
    let dir = uv_fixture();
    let p = PythonTestProvider::new(&dir).with_uv();
    let all = p.discover(&TestQuery::default()).await.unwrap();
    assert_eq!(all.len(), 2);
    let ids: Vec<_> = all.iter().map(|t| t.id.clone()).collect();
    let run = p.run(&ids).await.unwrap();
    assert_eq!(run.results[0].outcome, TestOutcome::Passed);
    assert_eq!(run.results[1].outcome, TestOutcome::Failed);
    let _ = std::fs::remove_dir_all(&dir);
}
