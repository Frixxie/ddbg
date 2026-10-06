//! Real .NET regressions; requires SDK 10 and netcoredbg on PATH.

use std::path::PathBuf;
use std::time::Duration;

use ddbg_driver::{Debugger, Halt, TestOutcome};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-dotnet")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires .NET SDK 10 and netcoredbg"]
async fn selected_string_theory_rows_hit_entry_and_run_exactly_once() {
    let mut dbg =
        Debugger::from_cli(["--adapter", "netcoredbg --interpreter=vscode"], fixture()).unwrap();
    let rows = dbg.tests(Some("StringRow")).await.unwrap();
    assert_eq!(rows.len(), 5);
    for row in rows {
        dbg.test_debug(&row.name, true)
            .await
            .unwrap()
            .into_stopped()
            .unwrap();
        let input = dbg.print("input").await.unwrap().value;
        assert!(
            row.name.contains(&format!("input: {input}")),
            "{}: {input}",
            row.name
        );
        let start = dbg.stdout().len();
        dbg.cont().await.unwrap().into_exited().unwrap();
        let output = &dbg.stdout()[start..];
        assert!(output.contains("total: 1"), "{}: {output}", row.name);
        assert!(output.contains("succeeded: 1"), "{}: {output}", row.name);
        let result = dbg.debug_test_result().unwrap();
        assert_eq!(result.test, row.name);
        assert_eq!(result.outcome, Some(TestOutcome::Passed));
        assert_eq!(result.counts.unwrap().total, 1);
    }
    dbg.quit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires .NET SDK 10 and netcoredbg"]
async fn runner_failure_is_separate_from_adapter_exit_and_resets_on_launch() {
    let mut dbg =
        Debugger::from_cli(["--adapter", "netcoredbg --interpreter=vscode"], fixture()).unwrap();
    dbg.tests(Some("CalculatorTests.Fails")).await.unwrap();
    dbg.test_debug("1", true)
        .await
        .unwrap()
        .into_stopped()
        .unwrap();
    assert_eq!(dbg.debug_test_result().unwrap().outcome, None);
    dbg.cont().await.unwrap().into_exited().unwrap();
    let result = dbg.debug_test_result().unwrap();
    assert_eq!(result.outcome, Some(TestOutcome::Failed));
    assert_eq!(result.counts.unwrap().failed, 1);
    dbg.take_transcript();
    dbg.wait().await.unwrap().into_exited().unwrap();
    assert_eq!(dbg.debug_test_result(), Some(result));

    dbg.run().await.unwrap().into_stopped().unwrap();
    assert_eq!(dbg.debug_test_result().unwrap().outcome, None);
    dbg.kill().await.unwrap();
    dbg.wait().await.unwrap();
    assert_eq!(dbg.debug_test_result().unwrap().outcome, None);

    // A manual launch of the same test binary must not inherit test-debug
    // identity or results, even when it emits a recognizable runner summary.
    let assembly = fixture().join("HelloTests/bin/Debug/net10.0/HelloTests.dll");
    dbg.run_program(
        assembly,
        ["--filter-method", "HelloTests.CalculatorTests.Fails"],
    )
    .await
    .unwrap()
    .into_stopped()
    .unwrap();
    assert_eq!(dbg.debug_test_result(), None);
    dbg.cont().await.unwrap().into_exited().unwrap();
    assert_eq!(dbg.debug_test_result(), None);
    dbg.quit().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires .NET SDK 10 and netcoredbg"]
async fn selected_theory_row_and_wait_after_kill() {
    let mut dbg =
        Debugger::from_cli(["--adapter", "netcoredbg --interpreter=vscode"], fixture()).unwrap();
    let rows = dbg.tests(Some("AddsMany")).await.unwrap();
    assert_eq!(rows.len(), 2);
    let row = rows.iter().find(|row| row.name.contains("a: 2")).unwrap();

    dbg.test_debug(&row.name, true)
        .await
        .unwrap()
        .into_stopped()
        .unwrap();
    assert_eq!(dbg.print("a").await.unwrap().value, "2");
    dbg.cont().await.unwrap().into_exited().unwrap();
    assert!(dbg.stdout().contains("total: 1"), "{}", dbg.stdout());
    dbg.take_transcript();
    dbg.set_timeout(Duration::ZERO);
    assert!(matches!(dbg.wait().await.unwrap(), Halt::Exited(_)));

    dbg.set_timeout(Duration::from_secs(30));
    dbg.test_debug(&row.name, true)
        .await
        .unwrap()
        .into_stopped()
        .unwrap();
    dbg.kill().await.unwrap();
    dbg.take_transcript();
    dbg.set_timeout(Duration::ZERO);
    assert_eq!(dbg.wait().await.unwrap(), Halt::Terminated);
    dbg.set_timeout(Duration::from_secs(30));
    dbg.test_debug(&row.name, true)
        .await
        .unwrap()
        .into_stopped()
        .unwrap();
    dbg.cont().await.unwrap().into_exited().unwrap();
    dbg.quit().await.unwrap();
}
