//! Python provider for pytest, driven through its command-line interface.
//!
//! - discovery: `python -m pytest --collect-only -q`, reading node IDs;
//! - execution: `python -m pytest -v <node-id>...`;
//! - debugging: a [`LaunchTarget`] for `-m pytest <node-id> -v -s`, which
//!   debugpy launches as a module.
//!
//! uv projects run through `uv run --with pytest`; otherwise the interpreter
//! is the project's virtualenv when present, else `python3`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, bail};
use ddbg_core::{DebugTarget, LaunchTarget};
use ddbg_project::PythonLauncher;
use tokio::process::Command;

use crate::{
    ProviderData, ProviderId, TestCase, TestCounts, TestId, TestOutcome, TestProvider, TestQuery,
    TestResult, TestRunResult,
};

/// pytest exit code when no tests were collected.
const NO_TESTS_COLLECTED: i32 = 5;

/// Options shared by all pytest invocations: no `.pytest_cache` writes and
/// one result per line without progress percentages.
const COMMON_ARGS: &[&str] = &[
    "-p",
    "no:cacheprovider",
    "-o",
    "console_output_style=classic",
];

#[derive(Debug, Clone)]
pub struct PythonTestProvider {
    /// Project root; pytest runs from here, so node IDs are relative to it.
    root: PathBuf,
    launcher: PythonLauncher,
}

impl PythonTestProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let launcher = ddbg_project::python_launcher(&root);
        Self { root, launcher }
    }

    pub fn with_python(mut self, python: impl Into<PathBuf>) -> Self {
        self.launcher = PythonLauncher::Interpreter(python.into());
        self
    }

    pub fn with_uv(mut self) -> Self {
        self.launcher = PythonLauncher::Uv;
        self
    }

    fn program(&self) -> &Path {
        match &self.launcher {
            PythonLauncher::Uv => Path::new("uv"),
            PythonLauncher::Interpreter(python) => python,
        }
    }

    fn pytest(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(self.program());
        if self.launcher == PythonLauncher::Uv {
            cmd.args(["run", "--with", "pytest", "python"]);
        }
        cmd.args(["-m", "pytest"])
            .args(COMMON_ARGS)
            .current_dir(dir)
            .stdin(Stdio::null());
        cmd
    }
}

impl TestProvider for PythonTestProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Python
    }

    async fn discover(&self, query: &TestQuery) -> anyhow::Result<Vec<TestCase>> {
        let out = self
            .pytest(&self.root)
            .args(["--collect-only", "-q"])
            .output()
            .await
            .with_context(|| format!("failed to run {}", self.program().display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() && out.status.code() != Some(NO_TESTS_COLLECTED) {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!(
                "pytest --collect-only failed:\n{}",
                format!("{stdout}{stderr}").trim()
            );
        }
        Ok(parse_collect(&stdout)
            .into_iter()
            .filter(|id| query.matches(id))
            .map(|id| test_case(&self.root, id))
            .collect())
    }

    async fn run(&self, tests: &[TestId]) -> anyhow::Result<TestRunResult> {
        let mut run = TestRunResult::default();
        if tests.is_empty() {
            return Ok(run);
        }
        for id in tests {
            python_root(id)?;
        }
        let out = self
            .pytest(&self.root)
            .arg("-v")
            .args(tests.iter().map(|id| id.name.as_str()))
            .output()
            .await
            .with_context(|| format!("failed to run {}", self.program().display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let output = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
        let outcomes = parse_run(&stdout);
        for id in tests {
            // A missing result means pytest failed before reporting the test.
            let outcome = outcomes
                .get(id.name.as_str())
                .copied()
                .unwrap_or(TestOutcome::Failed);
            run.results.push(TestResult {
                id: id.clone(),
                outcome,
                output: output.clone(),
            });
        }
        Ok(run)
    }

    async fn debug_target(&self, test: &TestId) -> anyhow::Result<DebugTarget> {
        let root = python_root(test)?;
        let mut args = vec![
            "pytest".to_owned(),
            test.name.clone(),
            "-v".into(),
            "-s".into(),
        ];
        args.extend(COMMON_ARGS.iter().map(|a| (*a).to_owned()));
        Ok(DebugTarget::Launch(LaunchTarget {
            // The debugpy adapter treats `-m` as "run the module in args[0]".
            program: "-m".into(),
            args,
            cwd: root.to_owned(),
            env: Default::default(),
            stop_on_entry: false,
        }))
    }
}

fn python_root(id: &TestId) -> anyhow::Result<&Path> {
    match &id.data {
        ProviderData::Python { root } => Ok(root),
        _ => bail!("test {} does not belong to the Python provider", id.name),
    }
}

fn test_case(root: &Path, name: String) -> TestCase {
    let (file, rest) = name.split_once("::").unwrap_or((&name, ""));
    let source = root.join(file);
    let parts: Vec<&str> = rest.split("::").collect();
    let function = parts.last().copied().unwrap_or_default();
    let function = function.split_once('[').map_or(function, |(f, _)| f);
    let line = std::fs::read_to_string(&source)
        .ok()
        .and_then(|src| first_body_line(&src, function));
    let display_name = name.rsplit("::").next().unwrap_or(&name).to_owned();
    let suite = name.rsplit_once("::").map(|(s, _)| s.to_owned());
    TestCase {
        id: TestId {
            provider: ProviderId::Python,
            name: name.clone(),
            data: ProviderData::Python {
                root: root.to_owned(),
            },
        },
        name,
        display_name,
        source: Some(source),
        line,
        suite,
    }
}

/// 1-based line of the first statement in `def function`, where a breakpoint
/// stops when the test runs (the `def` line itself runs at import).
pub fn first_body_line(src: &str, function: &str) -> Option<u32> {
    let lines: Vec<&str> = src.lines().collect();
    let def = lines.iter().position(|l| {
        let l = l.trim_start();
        let l = l.strip_prefix("async ").unwrap_or(l);
        l.strip_prefix("def ")
            .and_then(|l| l.strip_prefix(function))
            .is_some_and(|l| l.trim_start().starts_with('('))
    })?;
    let header_end = (def..lines.len()).find(|&i| strip_comment(lines[i]).ends_with(':'))?;
    let body = (header_end + 1..lines.len()).find(|&i| {
        let t = lines[i].trim();
        !t.is_empty() && !t.starts_with('#')
    });
    let line = body.unwrap_or(header_end);
    u32::try_from(line + 1).ok()
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#')
        .map_or(line, |(code, _)| code)
        .trim_end()
}

/// Node IDs from `pytest --collect-only -q` output.
pub fn parse_collect(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter(|l| l.contains("::"))
        .map(|l| l.trim().to_owned())
        .collect()
}

/// Outcomes from `pytest -v` output (`node-id PASSED`). Setup and teardown
/// errors are reported as `ERROR` and count as failures.
pub fn parse_run(stdout: &str) -> HashMap<&str, TestOutcome> {
    let mut outcomes = HashMap::new();
    for line in stdout.lines() {
        let Some((name, status)) = line.trim_end().rsplit_once(' ') else {
            continue;
        };
        if !name.contains("::") || name.contains(' ') {
            continue;
        }
        let outcome = match status {
            "PASSED" | "XPASS" => TestOutcome::Passed,
            "FAILED" | "ERROR" => TestOutcome::Failed,
            "SKIPPED" | "XFAIL" => TestOutcome::Ignored,
            _ => continue,
        };
        // A teardown error follows a PASSED line for the same test.
        let entry = outcomes.entry(name).or_insert(outcome);
        if outcome == TestOutcome::Failed {
            *entry = outcome;
        }
    }
    outcomes
}

/// Counts from pytest's final summary line, e.g.
/// `==== 1 failed, 2 passed, 1 skipped in 0.12s ====` or `no tests ran`.
pub fn parse_counts(stdout: &str) -> Option<TestCounts> {
    let summary = stdout.lines().rev().find_map(|l| {
        let l = l.trim().trim_matches('=').trim();
        let (counts, time) = l.rsplit_once(" in ")?;
        time.ends_with('s').then_some(counts)
    })?;
    let mut c = TestCounts {
        total: 0,
        passed: 0,
        failed: 0,
        ignored: 0,
    };
    if summary != "no tests ran" {
        for part in summary.split(", ") {
            let (n, label) = part.trim().split_once(' ')?;
            let n: u32 = n.parse().ok()?;
            let slot = match label {
                "passed" | "xpassed" => &mut c.passed,
                "failed" | "error" | "errors" => &mut c.failed,
                "skipped" | "xfailed" => &mut c.ignored,
                // Not executed tests or diagnostics.
                "deselected" | "warning" | "warnings" | "rerun" => continue,
                _ => return None,
            };
            *slot = slot.checked_add(n)?;
        }
    }
    c.total = c.passed.checked_add(c.failed)?.checked_add(c.ignored)?;
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_collect() {
        let out = "tests/test_a.py::test_one\ntests/test_a.py::TestX::test_two[1-2]\n\n2 tests collected in 0.01s\n";
        assert_eq!(
            parse_collect(out),
            [
                "tests/test_a.py::test_one",
                "tests/test_a.py::TestX::test_two[1-2]"
            ]
        );
        assert!(parse_collect("\nno tests collected in 0.01s\n").is_empty());
    }

    #[test]
    fn parses_run() {
        let out = "============ test session starts ============\n\
            tests/test_a.py::test_one PASSED\n\
            tests/test_a.py::test_two[a b] FAILED\n\
            tests/test_a.py::test_three SKIPPED (slow)\n\
            tests/test_a.py::test_four PASSED\n\
            tests/test_a.py::test_four ERROR\n\
            FAILED tests/test_a.py::test_two - assert 1 == 2\n";
        let r = parse_run(out);
        assert_eq!(r["tests/test_a.py::test_one"], TestOutcome::Passed);
        assert_eq!(r["tests/test_a.py::test_four"], TestOutcome::Failed);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn parses_counts() {
        let c = parse_counts("x\n===== 1 failed, 2 passed, 1 skipped, 3 warnings in 0.12s =====\n")
            .unwrap();
        assert_eq!((c.total, c.passed, c.failed, c.ignored), (4, 2, 1, 1));
        assert_eq!(
            parse_counts("== 1 passed in 0.01s ==").unwrap().outcome(),
            TestOutcome::Passed
        );
        assert_eq!(
            parse_counts("== no tests ran in 0.01s ==").unwrap().total,
            0
        );
        assert_eq!(parse_counts("test session starts"), None);
    }

    #[test]
    fn finds_first_body_line() {
        let src = "import x\n\n@mark\ndef test_a(\n    fixture,\n):  # c\n    # note\n\n    assert 1\n\nasync def test_b() -> None:\n    pass\n";
        assert_eq!(first_body_line(src, "test_a"), Some(9));
        assert_eq!(first_body_line(src, "test_b"), Some(12));
        assert_eq!(first_body_line(src, "test_"), None);
    }

    #[test]
    fn debug_target_shape() {
        let case = test_case(
            Path::new("/p"),
            "tests/test_a.py::TestX::test_two[1]".into(),
        );
        assert_eq!(case.display_name, "test_two[1]");
        assert_eq!(case.suite.as_deref(), Some("tests/test_a.py::TestX"));
        assert_eq!(case.source, Some(PathBuf::from("/p/tests/test_a.py")));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let DebugTarget::Launch(t) = rt
            .block_on(PythonTestProvider::new("/p").debug_target(&case.id))
            .unwrap()
        else {
            panic!("expected launch")
        };
        assert_eq!(t.program, PathBuf::from("-m"));
        assert_eq!(
            t.args[..2],
            ["pytest", "tests/test_a.py::TestX::test_two[1]"]
        );
        assert_eq!(t.cwd, PathBuf::from("/p"));
    }
}
