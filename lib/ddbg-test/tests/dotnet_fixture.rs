//! End-to-end against `fixtures/hello-dotnet` (requires the .NET SDK;
//! skipped when `dotnet` is not on PATH).

use std::path::PathBuf;

use ddbg_core::DebugTarget;
use ddbg_test::{DotNetTestProvider, TestOutcome, TestProvider, TestQuery};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-dotnet")
}

fn have_dotnet() -> bool {
    std::process::Command::new("dotnet")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn discover_run_and_debug_target() {
    if !have_dotnet() {
        eprintln!("skipping: dotnet not found");
        return;
    }
    let p = DotNetTestProvider::new(fixture());

    let all = p.discover(&TestQuery::default()).await.unwrap();
    let names: Vec<_> = all.iter().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"HelloTests.CalculatorTests.Adds"),
        "{names:?}"
    );
    assert!(names.contains(&"HelloTests.CalculatorTests.Fails"));
    assert_eq!(all.len(), 4);

    let adds = all.iter().find(|t| t.display_name == "Adds").unwrap();
    let fails = all.iter().find(|t| t.display_name == "Fails").unwrap();
    let run = p.run(&[adds.id.clone(), fails.id.clone()]).await.unwrap();
    assert_eq!(run.results[0].outcome, TestOutcome::Passed);
    assert_eq!(run.results[1].outcome, TestOutcome::Failed);
    assert!(run.results[1].output.contains("Assert.Equal() Failure"));

    let DebugTarget::Launch(t) = p.debug_target(&adds.id).await.unwrap() else {
        panic!("expected launch");
    };
    assert!(t.program.exists());
    assert_eq!(
        t.args[..2],
        ["--filter-method", "HelloTests.CalculatorTests.Adds"]
    );
}
