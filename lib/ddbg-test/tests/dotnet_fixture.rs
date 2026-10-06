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
    assert_eq!(all.len(), 9);

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
        ["--filter-display-name", "HelloTests.CalculatorTests.Adds"]
    );

    for row in all
        .iter()
        .filter(|t| t.name.contains("AddsMany(") || t.name.contains("StringRow("))
    {
        let run = p.run(std::slice::from_ref(&row.id)).await.unwrap();
        assert_eq!(run.results[0].outcome, TestOutcome::Passed);
        assert!(
            run.results[0].output.contains("total: 1"),
            "{}",
            run.results[0].output
        );

        let DebugTarget::Launch(target) = p.debug_target(&row.id).await.unwrap() else {
            panic!("expected launch");
        };
        assert_eq!(
            target.args[..2],
            ["--filter-display-name", row.name.as_str()]
        );
        // Execute exactly the launch arguments the debugger receives. Checking
        // only ddbg's rendered result count would miss method-wide execution.
        let output = tokio::process::Command::new("dotnet")
            .arg(&target.program)
            .args(&target.args)
            .envs(&target.env)
            .current_dir(&target.cwd)
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("total: 1"), "{stdout}");
    }
}
