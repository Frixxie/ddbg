//! Real TypeScript debugging through js-debug, found as by default (e.g.
//! `$DDBG_JS_DEBUG` pointing to the extracted `js-debug-dap` release).
//! Skips when Node.js or js-debug is not available.

mod common;

use common::line_of;
use std::path::PathBuf;

use ddbg_driver::{Debugger, Halt, TestOutcome};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-node")
}

fn have_js_debug() -> bool {
    use ddbg_core::adapter::{DebugAdapter, JsDebugAdapter};
    let node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let found = node && JsDebugAdapter::default().command().is_ok();
    if !found {
        eprintln!("skipping: node or js-debug (DDBG_JS_DEBUG) not found");
    }
    found
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires node and js-debug"]
async fn typescript_breakpoint_locals_and_stepping() {
    if !have_js_debug() {
        return;
    }
    let math = fixture().join("src/math.ts");
    let mut dbg = Debugger::from_cli(["--", "src/main.ts"], fixture()).unwrap();
    let bp = dbg
        .break_at("src/math.ts", line_of(&math, "add"))
        .await
        .unwrap();
    let stop = dbg.run().await.unwrap().into_stopped().unwrap();
    let frame = stop.frame.unwrap();
    assert_eq!(frame.line, line_of(&math, "add"));
    assert!(frame.path.unwrap().ends_with("src/math.ts"));
    assert_eq!(dbg.print("a + b").await.unwrap().value, "1");
    let locals = dbg.locals().await.unwrap();
    assert!(locals.iter().any(|v| v.name == "a" && v.value == "0"));

    let frames = dbg.backtrace().await.unwrap();
    assert!(frames.len() >= 2, "{frames:?}");
    assert!(frames[1].path.as_ref().unwrap().ends_with("src/main.ts"));

    let stop = dbg.cont().await.unwrap().into_stopped().unwrap();
    assert_eq!(stop.frame.unwrap().line, line_of(&math, "add"));
    assert_eq!(dbg.print("a").await.unwrap().value, "1");

    dbg.delete(bp.id).await.unwrap();
    // js-debug does not report exit codes.
    assert_eq!(dbg.cont().await.unwrap(), Halt::Terminated);
    assert!(dbg.stdout().contains("total = 6"), "{}", dbg.stdout());
    dbg.quit().await.unwrap();
}

fn have_node_modules() -> bool {
    let ok = fixture().join("node_modules/.bin/vitest").is_file()
        && fixture().join("node_modules/.bin/playwright").is_file();
    if !ok {
        eprintln!("skipping: run `npm install` in fixtures/hello-node");
    }
    ok
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires node, js-debug and npm install in fixtures/hello-node"]
async fn vitest_test_stops_in_test_and_reports_outcome() {
    if !have_js_debug() || !have_node_modules() {
        return;
    }
    let mut dbg = Debugger::from_cli(Vec::<String>::new(), fixture()).unwrap();
    let tests = dbg.tests(Some("math.test.ts")).await.unwrap();
    assert_eq!(tests.len(), 3, "{tests:?}");
    debug_failing_test(
        dbg,
        "fails on purpose",
        fixture().join("tests/math.test.ts"),
        "vitest-failing-assert",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires node, js-debug and npm install in fixtures/hello-node"]
async fn playwright_test_stops_in_test_and_reports_outcome() {
    if !have_js_debug() || !have_node_modules() {
        return;
    }
    let dbg = Debugger::from_cli(Vec::<String>::new(), fixture()).unwrap();
    debug_failing_test(
        dbg,
        "fails in playwright",
        fixture().join("e2e/math.spec.ts"),
        "playwright-failing-assert",
    )
    .await;
}

async fn debug_failing_test(mut dbg: Debugger, name: &str, file: PathBuf, marker: &str) {
    let tests = dbg.tests(Some(name)).await.unwrap();
    assert_eq!(tests.len(), 1, "{tests:?}");
    let line = line_of(&file, marker);
    dbg.break_at(&file, line).await.unwrap();
    let stop = dbg
        .test_debug(&tests[0].name, false)
        .await
        .unwrap()
        .into_stopped()
        .unwrap();
    assert_eq!(stop.frame.unwrap().line, line);
    assert_eq!(dbg.print("sum").await.unwrap().value, "4");
    assert_eq!(dbg.cont().await.unwrap(), Halt::Terminated);
    let result = dbg.debug_test_result().unwrap();
    assert_eq!(
        result.outcome,
        Some(TestOutcome::Failed),
        "{result:?}\n{}",
        dbg.stdout()
    );
    assert_eq!(result.counts.unwrap().total, 1);
    let diagnostic = result.diagnostic.unwrap_or_default();
    assert!(diagnostic.contains("Received"), "{diagnostic}");
    dbg.quit().await.unwrap();
}
