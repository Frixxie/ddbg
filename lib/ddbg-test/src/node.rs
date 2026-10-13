//! Node.js provider for Vitest and Playwright Test, driven through their
//! command-line interfaces from the project's `node_modules`.
//!
//! - discovery: `vitest list --json --includeTaskLocation` and
//!   `playwright test --list --reporter=json`;
//! - execution: the same runners with `--reporter=json`, selecting tests by
//!   `file:line`;
//! - debugging: a [`LaunchTarget`] running the runner's script with one
//!   worker and no test timeouts, so pausing at a breakpoint does not fail
//!   the test. js-debug attaches to the worker processes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, anyhow, bail};
use ddbg_core::{DebugTarget, LaunchTarget};
use serde_json::Value;
use tokio::process::Command;

use crate::{
    ProviderData, ProviderId, TestCase, TestCounts, TestId, TestOutcome, TestProvider, TestQuery,
    TestResult, TestRunResult,
};

/// Separator between file, suites and test in test names.
const SEP: &str = " > ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeRunner {
    Vitest,
    Playwright,
}

impl NodeRunner {
    const ALL: [Self; 2] = [Self::Vitest, Self::Playwright];

    fn tool(self) -> &'static str {
        match self {
            Self::Vitest => "vitest",
            Self::Playwright => "playwright",
        }
    }

    fn package(self) -> &'static str {
        match self {
            Self::Vitest => "vitest",
            Self::Playwright => "@playwright/test",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeTestProvider {
    /// Package root; runners start here.
    root: PathBuf,
    runners: Vec<NodeRunner>,
}

impl NodeTestProvider {
    /// Uses the runners the package depends on or has installed.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        // Runners report absolute, resolved paths.
        let root = root.canonicalize().unwrap_or(root);
        let runners = NodeRunner::ALL
            .into_iter()
            .filter(|r| {
                ddbg_project::node_has_dependency(&root, r.package())
                    || ddbg_project::node_bin(&root, r.tool()).is_some()
            })
            .collect();
        Self { root, runners }
    }

    pub fn runners(&self) -> &[NodeRunner] {
        &self.runners
    }

    fn bin(&self, runner: NodeRunner) -> anyhow::Result<PathBuf> {
        ddbg_project::node_bin(&self.root, runner.tool()).ok_or_else(|| {
            anyhow!(
                "{} is not installed in {} (run `npm install`)",
                runner.tool(),
                self.root.display()
            )
        })
    }

    fn command(&self, runner: NodeRunner) -> anyhow::Result<Command> {
        let mut cmd = Command::new(self.bin(runner)?);
        cmd.current_dir(&self.root)
            .stdin(Stdio::null())
            .env("FORCE_COLOR", "0")
            .env("NO_COLOR", "1");
        if runner == NodeRunner::Playwright {
            cmd.arg("test");
        }
        Ok(cmd)
    }

    async fn list(&self, runner: NodeRunner) -> anyhow::Result<Vec<TestCase>> {
        let mut cmd = self.command(runner)?;
        match runner {
            NodeRunner::Vitest => cmd.args(["list", "--json", "--includeTaskLocation"]),
            NodeRunner::Playwright => cmd.args(["--list", "--reporter=json"]),
        };
        let out = cmd
            .output()
            .await
            .with_context(|| format!("failed to run {}", runner.tool()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let json = parse_json(&stdout).ok_or_else(|| {
            anyhow!(
                "{} did not list tests:\n{}",
                runner.tool(),
                format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)).trim()
            )
        })?;
        Ok(match runner {
            NodeRunner::Vitest => parse_vitest_list(&self.root, &json),
            NodeRunner::Playwright => parse_playwright(&self.root, &json)
                .into_iter()
                .map(|t| t.case)
                .collect(),
        })
    }

    async fn run_with(
        &self,
        runner: NodeRunner,
        tests: &[&TestId],
    ) -> anyhow::Result<HashMap<String, (TestOutcome, String)>> {
        let mut cmd = self.command(runner)?;
        if runner == NodeRunner::Vitest {
            cmd.arg("run");
        }
        for id in tests {
            cmd.arg(location(id)?);
        }
        cmd.arg("--reporter=json");
        let out = cmd
            .output()
            .await
            .with_context(|| format!("failed to run {}", runner.tool()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let output = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
        let Some(json) = parse_json(&stdout) else {
            // The runner failed before reporting (e.g. a configuration error).
            return Ok(tests
                .iter()
                .map(|id| (id.name.clone(), (TestOutcome::Failed, output.clone())))
                .collect());
        };
        Ok(match runner {
            NodeRunner::Vitest => parse_vitest_run(&self.root, &json),
            NodeRunner::Playwright => parse_playwright(&self.root, &json)
                .into_iter()
                .filter_map(|t| Some((t.case.name, t.result?)))
                .collect(),
        })
    }
}

impl TestProvider for NodeTestProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Node
    }

    async fn discover(&self, query: &TestQuery) -> anyhow::Result<Vec<TestCase>> {
        if self.runners.is_empty() {
            bail!("no supported test runner (vitest or @playwright/test) found in package.json");
        }
        let mut tests = Vec::new();
        for runner in &self.runners {
            tests.extend(self.list(*runner).await?);
        }
        tests.retain(|t| query.matches(&t.name));
        Ok(tests)
    }

    async fn run(&self, tests: &[TestId]) -> anyhow::Result<TestRunResult> {
        let mut run = TestRunResult::default();
        for id in tests {
            node_data(id)?;
        }
        for runner in NodeRunner::ALL {
            let selected: Vec<&TestId> = tests
                .iter()
                .filter(|id| node_data(id).is_ok_and(|(r, ..)| r == runner))
                .collect();
            if selected.is_empty() {
                continue;
            }
            let outcomes = self.run_with(runner, &selected).await?;
            for id in selected {
                // A missing result means the runner failed before reporting it.
                let (outcome, output) = outcomes
                    .get(&id.name)
                    .cloned()
                    .unwrap_or((TestOutcome::Failed, String::new()));
                run.results.push(TestResult {
                    id: id.clone(),
                    outcome,
                    output,
                });
            }
        }
        Ok(run)
    }

    async fn debug_target(&self, test: &TestId) -> anyhow::Result<DebugTarget> {
        let (runner, root, _, _) = node_data(test)?;
        let bin = self.bin(runner)?;
        // The script behind the `.bin` link runs with node; js-debug cannot
        // set its bootloader through a shell wrapper on every platform.
        let script = bin.canonicalize().unwrap_or(bin);
        let at = location(test)?;
        let args: Vec<&str> = match runner {
            NodeRunner::Vitest => vec![
                "run",
                &at,
                "--no-file-parallelism",
                "--testTimeout=0",
                "--hookTimeout=0",
                "--reporter=default",
            ],
            NodeRunner::Playwright => {
                vec!["test", &at, "--workers=1", "--timeout=0", "--reporter=list"]
            }
        };
        Ok(DebugTarget::Launch(LaunchTarget {
            program: script,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: root.to_owned(),
            env: Default::default(),
            stop_on_entry: false,
        }))
    }
}

fn node_data(id: &TestId) -> anyhow::Result<(NodeRunner, &Path, &Path, Option<u32>)> {
    match &id.data {
        ProviderData::Node {
            runner,
            root,
            file,
            line,
        } => Ok((*runner, root, file, *line)),
        _ => bail!("test {} does not belong to the Node.js provider", id.name),
    }
}

/// `file:line`, which both runners accept to select a test.
fn location(id: &TestId) -> anyhow::Result<String> {
    let (_, _, file, line) = node_data(id)?;
    let line = line.ok_or_else(|| anyhow!("the location of test {} is unknown", id.name))?;
    Ok(format!("{}:{line}", file.display()))
}

/// The JSON document in a runner's output, which may be preceded by logs.
fn parse_json(stdout: &str) -> Option<Value> {
    let start = stdout.find(['[', '{'])?;
    serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter::<Value>()
        .next()?
        .ok()
}

fn relative(root: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(root).unwrap_or(file).to_owned()
}

fn test_case(
    root: &Path,
    runner: NodeRunner,
    file: &Path,
    path: &str,
    line: Option<u32>,
) -> TestCase {
    let rel = relative(root, file);
    let name = format!("{}{SEP}{path}", rel.display());
    let (suite, display_name) = match name.rsplit_once(SEP) {
        Some((suite, test)) => (Some(suite.to_owned()), test.to_owned()),
        None => (None, name.clone()),
    };
    let body = line.and_then(|l| {
        std::fs::read_to_string(file)
            .ok()
            .map(|src| first_body_line(&src, l))
    });
    TestCase {
        id: TestId {
            provider: ProviderId::Node,
            name: name.clone(),
            data: ProviderData::Node {
                runner,
                root: root.to_owned(),
                file: rel,
                line,
            },
        },
        name,
        display_name,
        source: Some(file.to_owned()),
        line: body.or(line),
        suite,
    }
}

/// Line of the first statement in the callback of the `test(...)` call on
/// 1-based `line`. A breakpoint on the call itself stops during collection.
pub fn first_body_line(src: &str, line: u32) -> u32 {
    let lines: Vec<&str> = src.lines().collect();
    let start = line as usize - 1;
    let Some(open) = (start..lines.len().min(start + 10)).find(|&i| {
        let code = lines[i].split("//").next().unwrap_or_default().trim_end();
        code.ends_with('{')
    }) else {
        return line;
    };
    (open + 1..lines.len())
        .find(|&i| {
            let t = lines[i].trim();
            !t.is_empty() && !t.starts_with("//")
        })
        .and_then(|i| u32::try_from(i + 1).ok())
        .unwrap_or(line)
}

/// `vitest list --json --includeTaskLocation`.
pub fn parse_vitest_list(root: &Path, json: &Value) -> Vec<TestCase> {
    json.as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let file = Path::new(t["file"].as_str()?);
            let line = t["location"]["line"]
                .as_u64()
                .and_then(|l| u32::try_from(l).ok());
            Some(test_case(root, NodeRunner::Vitest, file, name, line))
        })
        .collect()
}

/// `vitest run --reporter=json`, keyed by test name.
pub fn parse_vitest_run(root: &Path, json: &Value) -> HashMap<String, (TestOutcome, String)> {
    let mut results = HashMap::new();
    for file in json["testResults"].as_array().into_iter().flatten() {
        let Some(path) = file["name"].as_str() else {
            continue;
        };
        let rel = relative(root, Path::new(path));
        for a in file["assertionResults"].as_array().into_iter().flatten() {
            let mut parts: Vec<&str> = a["ancestorTitles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            parts.push(a["title"].as_str().unwrap_or_default());
            let outcome = match a["status"].as_str() {
                Some("passed") => TestOutcome::Passed,
                Some("failed") => TestOutcome::Failed,
                _ => TestOutcome::Ignored,
            };
            let output = a["failureMessages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            let name = format!("{}{SEP}{}", rel.display(), parts.join(SEP));
            results.insert(name, (outcome, output));
        }
    }
    results
}

pub struct PlaywrightTest {
    pub case: TestCase,
    /// Outcome and error output, when the report includes results.
    pub result: Option<(TestOutcome, String)>,
}

/// `playwright test --reporter=json`, with or without `--list`.
pub fn parse_playwright(root: &Path, json: &Value) -> Vec<PlaywrightTest> {
    let base = json["config"]["rootDir"]
        .as_str()
        .map_or_else(|| root.to_owned(), PathBuf::from);
    let mut out = Vec::new();
    for suite in json["suites"].as_array().into_iter().flatten() {
        // Top-level suites are files; their title is the file name.
        playwright_suite(root, &base, suite, &[], &mut out);
    }
    out
}

fn playwright_suite(
    root: &Path,
    base: &Path,
    suite: &Value,
    titles: &[&str],
    out: &mut Vec<PlaywrightTest>,
) {
    for spec in suite["specs"].as_array().into_iter().flatten() {
        let (Some(file), Some(title)) = (spec["file"].as_str(), spec["title"].as_str()) else {
            continue;
        };
        let mut path = titles.to_vec();
        path.push(title);
        let line = spec["line"].as_u64().and_then(|l| u32::try_from(l).ok());
        let case = test_case(
            root,
            NodeRunner::Playwright,
            &base.join(file),
            &path.join(SEP),
            line,
        );
        let tests = spec["tests"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let ran = tests
            .iter()
            .any(|t| t["results"].as_array().is_some_and(|r| !r.is_empty()));
        let result = ran.then(|| {
            let failed = tests
                .iter()
                .any(|t| t["status"].as_str() == Some("unexpected"));
            let skipped = tests
                .iter()
                .all(|t| t["status"].as_str() == Some("skipped"));
            let outcome = if failed {
                TestOutcome::Failed
            } else if skipped {
                TestOutcome::Ignored
            } else {
                TestOutcome::Passed
            };
            let errors = tests
                .iter()
                .flat_map(|t| t["results"].as_array().into_iter().flatten())
                .filter_map(|r| r["error"]["message"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            (outcome, errors)
        });
        out.push(PlaywrightTest { case, result });
    }
    for child in suite["suites"].as_array().into_iter().flatten() {
        let mut nested = titles.to_vec();
        if let Some(title) = child["title"].as_str() {
            nested.push(title);
        }
        playwright_suite(root, base, child, &nested, out);
    }
}

/// Counts from the summary of Vitest's default reporter
/// (`Tests  1 failed | 2 skipped (3)`) or Playwright's list reporter
/// (`1 failed`, `2 passed (1.2s)`).
///
/// Vitest reports tests excluded by the location filter as skipped, so
/// skipped and todo tests are not counted.
pub fn parse_counts(stdout: &str) -> Option<TestCounts> {
    let mut c = TestCounts {
        total: 0,
        passed: 0,
        failed: 0,
        ignored: 0,
    };
    if let Some(summary) = stdout
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("Tests "))
    {
        let summary = summary.rsplit_once(" (").map_or(summary, |(s, _)| s);
        for part in summary.split('|') {
            let (n, label) = part.trim().split_once(' ')?;
            let n: u32 = n.parse().ok()?;
            match label {
                "passed" => c.passed = c.passed.checked_add(n)?,
                "failed" => c.failed = c.failed.checked_add(n)?,
                "skipped" | "todo" => {}
                _ => return None,
            }
        }
    } else {
        let mut found = false;
        for line in stdout.lines() {
            let line = line.trim();
            let line = line.split_once(" (").map_or(line, |(s, _)| s);
            let Some((n, label)) = line.split_once(' ') else {
                continue;
            };
            let Ok(n) = n.parse::<u32>() else { continue };
            let slot = match label {
                "passed" | "flaky" => &mut c.passed,
                "failed" | "interrupted" => &mut c.failed,
                "skipped" | "did not run" => &mut c.ignored,
                _ => continue,
            };
            *slot = slot.checked_add(n)?;
            found = true;
        }
        if !found {
            return None;
        }
    }
    c.total = c.passed.checked_add(c.failed)?.checked_add(c.ignored)?;
    Some(c)
}

/// The first failure message in the output of Vitest's default reporter
/// (after ` FAIL  <test>`) or Playwright's list reporter (after
/// `  1) <test> ───`), without the code frame and stack that follow it.
pub fn parse_failure(stdout: &str) -> Option<String> {
    let mut lines = stdout.lines().skip_while(|l| {
        let l = l.trim_start();
        let playwright = l
            .split_once(") ")
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        !(l.starts_with("FAIL ") || playwright && l.ends_with('─'))
    });
    lines.next()?;
    let mut message = Vec::new();
    for line in lines {
        let t = line.trim();
        let code_frame = t
            .trim_start_matches('>')
            .trim_start()
            .split_once(['|', ' '])
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if t.starts_with('❯') || t.starts_with('⎯') || t.starts_with("at ") || code_frame {
            break;
        }
        message.push(t);
    }
    let message = message.join("\n").trim().to_owned();
    (!message.is_empty()).then_some(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_failure() {
        let vitest = " FAIL  tests/a.test.ts > points > fails\nAssertionError: expected 4 to be 5\n\n- Expected\n+ Received\n\n ❯ tests/a.test.ts:16:17\n     14|   test(\"fails\", () => {\n";
        assert_eq!(
            parse_failure(vitest).unwrap(),
            "AssertionError: expected 4 to be 5\n\n- Expected\n+ Received"
        );
        let playwright = "  ✘  1 e2e/a.spec.ts:9:3 › fails (3ms)\n\n\n  1) e2e/a.spec.ts:9:3 › fails ────\n\n    Error: expect(received).toBe(expected)\n\n    Expected: 5\n    Received: 4\n\n       9 |   test(\"fails\", () => {\n    > 11 |     expect(sum).toBe(5);\n";
        assert_eq!(
            parse_failure(playwright).unwrap(),
            "Error: expect(received).toBe(expected)\n\nExpected: 5\nReceived: 4"
        );
        assert_eq!(parse_failure("  1 passed (1.2s)\n"), None);
    }

    #[test]
    fn parses_vitest_list() {
        let root = Path::new("/p");
        let json = json!([
            {"name": "adds", "file": "/p/tests/a.test.ts", "location": {"line": 4, "column": 1}},
            {"name": "points > adds", "file": "/p/tests/a.test.ts", "location": {"line": 9, "column": 3}},
        ]);
        let cases = parse_vitest_list(root, &json);
        assert_eq!(cases[1].name, "tests/a.test.ts > points > adds");
        assert_eq!(cases[1].display_name, "adds");
        assert_eq!(cases[1].suite.as_deref(), Some("tests/a.test.ts > points"));
        assert_eq!(cases[1].line, Some(9));
        let ProviderData::Node { file, line, .. } = &cases[1].id.data else {
            panic!()
        };
        assert_eq!(
            (file.as_path(), *line),
            (Path::new("tests/a.test.ts"), Some(9))
        );
        assert_eq!(location(&cases[1].id).unwrap(), "tests/a.test.ts:9");
    }

    #[test]
    fn parses_vitest_run() {
        let json = json!({"testResults": [{"name": "/p/tests/a.test.ts", "assertionResults": [
            {"ancestorTitles": [], "title": "adds", "status": "passed", "failureMessages": []},
            {"ancestorTitles": ["points"], "title": "fails", "status": "failed", "failureMessages": ["boom"]},
            {"ancestorTitles": ["points"], "title": "later", "status": "skipped", "failureMessages": []},
        ]}]});
        let r = parse_vitest_run(Path::new("/p"), &json);
        assert_eq!(r["tests/a.test.ts > adds"].0, TestOutcome::Passed);
        assert_eq!(
            r["tests/a.test.ts > points > fails"],
            (TestOutcome::Failed, "boom".into())
        );
        assert_eq!(
            r["tests/a.test.ts > points > later"].0,
            TestOutcome::Ignored
        );
    }

    #[test]
    fn parses_playwright_report() {
        let json = json!({"config": {"rootDir": "/p/e2e"}, "suites": [{
            "title": "a.spec.ts", "file": "a.spec.ts",
            "specs": [{"title": "top", "file": "a.spec.ts", "line": 4, "tests": [
                {"status": "expected", "results": [{"status": "passed"}]}]}],
            "suites": [{"title": "suite", "file": "a.spec.ts", "specs": [
                {"title": "fails", "file": "a.spec.ts", "line": 9, "tests": [
                    {"status": "unexpected", "results": [{"status": "failed", "error": {"message": "boom"}}]}]}]}]
        }]});
        let tests = parse_playwright(Path::new("/p"), &json);
        assert_eq!(tests[0].case.name, "e2e/a.spec.ts > top");
        assert_eq!(tests[0].result, Some((TestOutcome::Passed, String::new())));
        assert_eq!(tests[1].case.name, "e2e/a.spec.ts > suite > fails");
        assert_eq!(location(&tests[1].case.id).unwrap(), "e2e/a.spec.ts:9");
        assert_eq!(tests[1].result, Some((TestOutcome::Failed, "boom".into())));

        let listed = json!({"config": {"rootDir": "/p/e2e"}, "suites": [{"title": "a.spec.ts",
            "specs": [{"title": "top", "file": "a.spec.ts", "line": 4, "tests": [{"status": "skipped", "results": []}]}]}]});
        assert_eq!(parse_playwright(Path::new("/p"), &listed)[0].result, None);
    }

    #[test]
    fn parses_counts() {
        let vitest = " Test Files  1 failed (1)\n      Tests  1 failed | 2 skipped (3)\n   Start at  14:13:22\n";
        let c = parse_counts(vitest).unwrap();
        assert_eq!((c.total, c.failed), (1, 1));
        let c = parse_counts("      Tests  3 passed (3)\n").unwrap();
        assert_eq!((c.total, c.passed), (3, 3));

        let playwright = "Running 1 test using 1 worker\n  1 failed\n    e2e/a.spec.ts:9:3 › suite › fails ───\n  1 passed (1.2s)\n";
        let c = parse_counts(playwright).unwrap();
        assert_eq!((c.total, c.passed, c.failed), (2, 1, 1));
        assert_eq!(parse_counts("Running 1 test using 1 worker\n"), None);
    }

    #[test]
    fn finds_first_body_line() {
        let src = "test(\"a\", () => {\n\n  // note\n  expect(1).toBe(1);\n});\ntest(\"b\", fn);\n";
        assert_eq!(first_body_line(src, 1), 4);
        assert_eq!(first_body_line(src, 6), 6);
    }
}
