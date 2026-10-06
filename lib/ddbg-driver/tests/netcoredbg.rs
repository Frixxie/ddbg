//! Real .NET regressions; requires SDK 10 and netcoredbg on PATH.

use std::path::PathBuf;
use std::time::Duration;

use ddbg_driver::{Debugger, Halt};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-dotnet")
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
