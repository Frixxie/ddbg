//! End-to-end against `fixtures/hello-node` (requires `npm install` there;
//! skipped when its runners are not installed).

use std::path::PathBuf;

use ddbg_core::DebugTarget;
use ddbg_test::{NodeRunner, NodeTestProvider, TestOutcome, TestProvider, TestQuery};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-node")
}

fn installed() -> bool {
    let ok = ["vitest", "playwright"]
        .iter()
        .all(|t| fixture().join("node_modules/.bin").join(t).is_file());
    if !ok {
        eprintln!("skipping: run `npm install` in fixtures/hello-node");
    }
    ok
}

#[test]
fn detects_runners_from_package_json() {
    let p = NodeTestProvider::new(fixture());
    assert_eq!(p.runners(), [NodeRunner::Vitest, NodeRunner::Playwright]);
}

#[tokio::test]
async fn discover_run_and_debug_target() {
    if !installed() {
        return;
    }
    let p = NodeTestProvider::new(fixture());

    let all = p.discover(&TestQuery::default()).await.unwrap();
    let names: Vec<_> = all.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "tests/math.test.ts > adds numbers",
            "tests/math.test.ts > points > adds points",
            "tests/math.test.ts > points > fails on purpose",
            "e2e/math.spec.ts > adds in playwright",
            "e2e/math.spec.ts > suite > fails in playwright",
        ]
    );
    // The first statement of the test body, not the `test(` call.
    assert_eq!(all[0].line, Some(5));

    let ids: Vec<_> = all.iter().map(|t| t.id.clone()).collect();
    let run = p.run(&ids).await.unwrap();
    let outcomes: Vec<_> = run.results.iter().map(|r| r.outcome).collect();
    use TestOutcome::{Failed, Passed};
    assert_eq!(outcomes, [Passed, Passed, Failed, Passed, Failed]);
    assert!(run.results[2].output.contains("expected 4 to be 5"));

    let DebugTarget::Launch(t) = p.debug_target(&all[2].id).await.unwrap() else {
        panic!("expected launch");
    };
    assert!(t.program.ends_with("vitest.mjs"), "{t:?}");
    assert_eq!(t.args[..2], ["run", "tests/math.test.ts:14"]);
    assert_eq!(t.cwd, fixture().canonicalize().unwrap());
}
