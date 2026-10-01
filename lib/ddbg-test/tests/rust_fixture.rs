//! End-to-end against `fixtures/hello-rust` (requires cargo).

use std::path::PathBuf;

use ddbg_core::DebugTarget;
use ddbg_test::{RustTestProvider, TestOutcome, TestProvider, TestQuery};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-rust")
}

#[tokio::test]
async fn discover_run_and_debug_target() {
    let p = RustTestProvider::new(fixture());

    let all = p.discover(&TestQuery::default()).await.unwrap();
    let names: Vec<_> = all.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["tests::adds", "tests::fails"]);

    let only = p.discover(&TestQuery::new("ADDS")).await.unwrap();
    assert_eq!(only.len(), 1);

    let ids: Vec<_> = all.iter().map(|t| t.id.clone()).collect();
    let run = p.run(&ids).await.unwrap();
    assert_eq!(run.results[0].outcome, TestOutcome::Passed);
    assert_eq!(run.results[1].outcome, TestOutcome::Failed);

    let DebugTarget::Launch(t) = p.debug_target(&all[0].id).await.unwrap() else {
        panic!("expected launch");
    };
    assert!(t.program.exists());
    assert_eq!(t.args[0], "tests::adds");
}
