//! Rust/libtest provider driven through Cargo's command-line interfaces.
//!
//! - build: `cargo test --no-run --message-format=json`, reading executable
//!   paths from `compiler-artifact` messages;
//! - discovery: `<test-binary> --list`;
//! - execution: `<test-binary> <name>... --exact`;
//! - debugging: a [`LaunchTarget`] for `<test-binary> <name> --exact
//!   --nocapture --test-threads=1`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, bail};
use ddbg_core::{DebugTarget, LaunchTarget};
use serde_json::Value;
use tokio::process::Command;

use crate::{
    ProviderData, ProviderId, TestCase, TestId, TestOutcome, TestProvider, TestQuery, TestResult,
    TestRunResult,
};

/// A test executable produced by Cargo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestBinary {
    pub executable: PathBuf,
    pub package: String,
    pub target: String,
    /// Directory containing the package's `Cargo.toml`; tests run from here,
    /// like under `cargo test`.
    pub package_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RustTestProvider {
    /// Directory containing (or below) the `Cargo.toml` to build.
    root: PathBuf,
    /// Extra arguments for `cargo test --no-run`, e.g. `-p foo`.
    cargo_args: Vec<String>,
}

impl RustTestProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cargo_args: Vec::new(),
        }
    }

    pub fn with_cargo_args(mut self, args: Vec<String>) -> Self {
        self.cargo_args = args;
        self
    }

    /// Build all test executables and return them.
    pub async fn build(&self) -> anyhow::Result<Vec<TestBinary>> {
        let out = Command::new("cargo")
            .args(["test", "--no-run", "--message-format=json"])
            .args(&self.cargo_args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .await
            .context("failed to run cargo")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            let errors = compiler_errors(&stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!(
                "cargo test --no-run failed:\n{}",
                if errors.is_empty() {
                    stderr.trim()
                } else {
                    errors.trim()
                }
            );
        }
        Ok(parse_build_output(&stdout))
    }

    async fn list(&self, bin: &TestBinary) -> anyhow::Result<Vec<String>> {
        let out = Command::new(&bin.executable)
            .args(["--list", "--format", "terse"])
            .current_dir(&bin.package_dir)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("failed to run {}", bin.executable.display()))?;
        if !out.status.success() {
            bail!(
                "{} --list failed: {}",
                bin.executable.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(parse_list(&String::from_utf8_lossy(&out.stdout)))
    }
}

impl TestProvider for RustTestProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Rust
    }

    async fn discover(&self, query: &TestQuery) -> anyhow::Result<Vec<TestCase>> {
        let mut cases = Vec::new();
        for bin in self.build().await? {
            for name in self.list(&bin).await? {
                if !query.matches(&name) {
                    continue;
                }
                cases.push(test_case(&bin, name));
            }
        }
        Ok(cases)
    }

    async fn run(&self, tests: &[TestId]) -> anyhow::Result<TestRunResult> {
        // One harness invocation per executable.
        let mut groups: BTreeMap<(PathBuf, PathBuf), Vec<&TestId>> = BTreeMap::new();
        for id in tests {
            let (bin, dir) = rust_data(id)?;
            groups
                .entry((bin.to_owned(), dir.to_owned()))
                .or_default()
                .push(id);
        }

        let mut run = TestRunResult::default();
        for ((bin, dir), ids) in groups {
            let out = Command::new(&bin)
                .args(ids.iter().map(|id| id.name.as_str()))
                .arg("--exact")
                .envs(cargo_env(&dir))
                .current_dir(&dir)
                .stdin(Stdio::null())
                .output()
                .await
                .with_context(|| format!("failed to run {}", bin.display()))?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let output = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
            let outcomes = parse_run(&stdout);
            for id in ids {
                // A missing result means the harness crashed before reporting.
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
        }
        Ok(run)
    }

    async fn debug_target(&self, test: &TestId) -> anyhow::Result<DebugTarget> {
        let (bin, dir) = rust_data(test)?;
        Ok(DebugTarget::Launch(LaunchTarget {
            program: bin.to_owned(),
            args: vec![
                test.name.clone(),
                "--exact".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            cwd: dir.to_owned(),
            env: cargo_env(dir),
            stop_on_entry: false,
        }))
    }
}

fn rust_data(id: &TestId) -> anyhow::Result<(&Path, &Path)> {
    match &id.data {
        ProviderData::Rust {
            binary,
            package_dir,
            ..
        } => Ok((binary, package_dir)),
        _ => bail!("test {} does not belong to the Rust provider", id.name),
    }
}

/// Runtime environment Cargo provides to test executables.
fn cargo_env(package_dir: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "CARGO_MANIFEST_DIR".to_owned(),
        package_dir.display().to_string(),
    )])
}

fn test_case(bin: &TestBinary, name: String) -> TestCase {
    let display_name = name.rsplit("::").next().unwrap_or(&name).to_owned();
    let suite = name.rsplit_once("::").map(|(s, _)| s.to_owned());
    TestCase {
        id: TestId {
            provider: ProviderId::Rust,
            name: name.clone(),
            data: ProviderData::Rust {
                binary: bin.executable.clone(),
                package: bin.package.clone(),
                target: bin.target.clone(),
                package_dir: bin.package_dir.clone(),
            },
        },
        name,
        display_name,
        source: None,
        line: None,
        suite,
    }
}

/// Extract test executables from `cargo --message-format=json` output.
pub fn parse_build_output(stdout: &str) -> Vec<TestBinary> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["profile"]["test"] == true)
        .filter_map(|m| {
            let executable = PathBuf::from(m["executable"].as_str()?);
            let manifest = Path::new(m["manifest_path"].as_str()?);
            Some(TestBinary {
                executable,
                package: package_name(m["package_id"].as_str()?)?,
                target: m["target"]["name"].as_str()?.to_owned(),
                package_dir: manifest.parent()?.to_owned(),
            })
        })
        .collect()
}

/// Rendered compiler errors from `cargo --message-format=json` output.
fn compiler_errors(stdout: &str) -> String {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-message" && m["message"]["level"] == "error")
        .filter_map(|m| m["message"]["rendered"].as_str().map(str::to_owned))
        .collect()
}

/// Package name from a Cargo package ID, in either the current
/// (`path+file:///x#name@1.0.0`, `...#1.0.0` when name matches the URL's last
/// segment) or legacy (`name 1.0.0 (path+file:///x)`) format.
fn package_name(id: &str) -> Option<String> {
    if let Some((url, frag)) = id.rsplit_once('#') {
        return match frag.split_once('@') {
            Some((name, _)) => Some(name.to_owned()),
            None => url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .map(str::to_owned),
        };
    }
    id.split_whitespace().next().map(str::to_owned)
}

/// Test names from libtest `--list` output (`name: test` lines).
pub fn parse_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

/// Outcomes from libtest's human-readable output (`test name ... ok`).
pub fn parse_run(stdout: &str) -> HashMap<&str, TestOutcome> {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("test "))
        .filter_map(|l| l.split_once(" ... "))
        .filter_map(|(name, status)| {
            let word = status.split([' ', ',']).next()?;
            let outcome = match word {
                "ok" => TestOutcome::Passed,
                "FAILED" => TestOutcome::Failed,
                "ignored" => TestOutcome::Ignored,
                _ => return None,
            };
            Some((name, outcome))
        })
        .collect()
}

/// Counts from the final libtest summary, including a zero-test run.
pub fn parse_counts(stdout: &str) -> Option<crate::TestCounts> {
    let summary = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("test result:"))?;
    let count = |label: &str| -> Option<u32> {
        summary.split(';').find_map(|part| {
            let part = part.trim();
            let part = part.strip_suffix(label)?.trim_end();
            part.rsplit_once(' ').map_or(part, |(_, n)| n).parse().ok()
        })
    };
    let counts = crate::TestCounts {
        passed: count("passed")?,
        failed: count("failed")?,
        ignored: count("ignored")?,
        total: 0,
    };
    let total = counts
        .passed
        .checked_add(counts.failed)?
        .checked_add(counts.ignored)?;
    Some(crate::TestCounts { total, ..counts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn generated_test_lists_roundtrip(names: Vec<Vec<u16>>) -> bool {
        let names: Vec<_> = names
            .iter()
            .map(|parts| {
                parts
                    .iter()
                    .map(|n| format!("module{n}::"))
                    .collect::<String>()
                    + "test"
            })
            .collect();
        let output = names
            .iter()
            .map(|name| format!("{name}: test\n"))
            .collect::<String>()
            + "bench: benchmark\n\nsummary\n";
        parse_list(&output) == names
    }

    #[quickcheck]
    fn generated_test_outcomes_roundtrip(statuses: Vec<u8>) -> bool {
        let mut output = String::from("running tests\n");
        let mut expected = HashMap::new();
        for (i, status) in statuses.into_iter().enumerate() {
            let (word, outcome) = match status % 3 {
                0 => ("ok", TestOutcome::Passed),
                1 => ("FAILED", TestOutcome::Failed),
                _ => ("ignored, slow", TestOutcome::Ignored),
            };
            let name = format!("module::test{i}");
            output.push_str(&format!("test {name} ... {word}\n"));
            expected.insert(name, outcome);
        }
        let actual: HashMap<_, _> = parse_run(&output)
            .into_iter()
            .map(|(name, outcome)| (name.to_owned(), outcome))
            .collect();
        actual == expected
    }

    #[test]
    fn parses_build_artifacts() {
        let out = r#"{"reason":"compiler-artifact","package_id":"path+file:///ws/a#app@0.1.0","manifest_path":"/ws/a/Cargo.toml","target":{"name":"app","kind":["bin"]},"profile":{"test":true},"executable":"/ws/target/debug/deps/app-123"}
{"reason":"compiler-artifact","package_id":"path+file:///ws/a#app@0.1.0","manifest_path":"/ws/a/Cargo.toml","target":{"name":"app","kind":["bin"]},"profile":{"test":false},"executable":"/ws/target/debug/app"}
{"reason":"compiler-artifact","package_id":"registry+https://x#serde@1.0.0","manifest_path":"/r/serde/Cargo.toml","target":{"name":"serde","kind":["lib"]},"profile":{"test":false},"executable":null}
{"reason":"build-finished","success":true}"#;
        assert_eq!(
            parse_build_output(out),
            vec![TestBinary {
                executable: "/ws/target/debug/deps/app-123".into(),
                package: "app".into(),
                target: "app".into(),
                package_dir: "/ws/a".into(),
            }]
        );
    }

    #[test]
    fn package_id_formats() {
        assert_eq!(package_name("path+file:///ws/a#app@0.1.0").unwrap(), "app");
        assert_eq!(package_name("path+file:///ws/app#0.1.0").unwrap(), "app");
        assert_eq!(
            package_name("app 0.1.0 (path+file:///ws/a)").unwrap(),
            "app"
        );
    }

    #[test]
    fn parses_list() {
        let out = "a::tests::one: test\nb::two: test\nbench_x: benchmark\n\n2 tests, 1 benchmark\n";
        assert_eq!(parse_list(out), vec!["a::tests::one", "b::two"]);
    }

    #[test]
    fn parses_run() {
        let out = "running 3 tests\ntest a::one ... ok\ntest a::two ... FAILED\ntest a::three ... ignored, slow\n\ntest result: FAILED.";
        let r = parse_run(out);
        assert_eq!(r["a::one"], TestOutcome::Passed);
        assert_eq!(r["a::two"], TestOutcome::Failed);
        assert_eq!(r["a::three"], TestOutcome::Ignored);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn parses_complete_runner_counts_including_zero_tests() {
        let summary = |passed, failed, ignored| {
            format!(
                "test result: ok. {passed} passed; {failed} failed; {ignored} ignored; 0 measured; 2 filtered out; finished in 0.00s\n"
            )
        };
        let counts = parse_counts(&summary(1, 0, 0)).unwrap();
        assert_eq!(counts.total, 1);
        assert_eq!(counts.outcome(), TestOutcome::Passed);
        assert_eq!(
            parse_counts(&summary(0, 1, 0)).unwrap().outcome(),
            TestOutcome::Failed
        );
        assert_eq!(
            parse_counts(&summary(0, 0, 1)).unwrap().outcome(),
            TestOutcome::Ignored
        );
        assert_eq!(parse_counts(&summary(0, 0, 0)).unwrap().total, 0);
        assert_eq!(parse_counts("test result: ok. 1 passed;"), None);
    }

    #[test]
    fn query_matching() {
        assert!(TestQuery::default().matches("x"));
        assert!(TestQuery::new("Parser").matches("parser::tests::empty"));
        assert!(!TestQuery::new("lexer").matches("parser::tests::empty"));
    }

    #[test]
    fn debug_target_shape() {
        let bin = TestBinary {
            executable: "/t/app-1".into(),
            package: "app".into(),
            target: "app".into(),
            package_dir: "/ws/a".into(),
        };
        let case = test_case(&bin, "parser::tests::empty".into());
        assert_eq!(case.display_name, "empty");
        assert_eq!(case.suite.as_deref(), Some("parser::tests"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let DebugTarget::Launch(t) = rt
            .block_on(RustTestProvider::new("/ws").debug_target(&case.id))
            .unwrap()
        else {
            panic!("expected launch")
        };
        assert_eq!(t.program, PathBuf::from("/t/app-1"));
        assert_eq!(
            t.args,
            [
                "parser::tests::empty",
                "--exact",
                "--nocapture",
                "--test-threads=1"
            ]
        );
        assert_eq!(t.cwd, PathBuf::from("/ws/a"));
    }
}
