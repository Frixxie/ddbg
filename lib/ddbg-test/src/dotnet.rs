//! .NET provider for Microsoft.Testing.Platform (MTP) test applications
//! (phase A: plain process execution with command-line filtering).
//!
//! - build: `dotnet build <project> -getProperty:...` for each likely test
//!   project, which builds and reports `TargetPath` and
//!   `IsTestingPlatformApplication`;
//! - discovery: `dotnet <assembly> --list-tests`;
//! - execution: `dotnet <assembly> <filter>`, one invocation per test;
//! - debugging: a [`LaunchTarget`] for the assembly with the same filter.
//!
//! `--list-tests` prints display names only, so a framework-specific filter
//! is needed to run one test. Only xUnit v3 is supported so far; MSTest
//! lists unqualified method names, which are not a usable identity. MTP
//! server mode (phase B) will provide stable test-node UIDs for all
//! frameworks.

use std::collections::BTreeMap;
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

/// Test framework behind an MTP application, identified by its banner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Framework {
    XUnitV3,
    /// Unsupported framework; holds the banner line for error messages.
    Other(String),
}

/// A built MTP test application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestAssembly {
    pub project: PathBuf,
    pub assembly: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DotNetTestProvider {
    /// Directory containing the project or solution.
    root: PathBuf,
}

impl DotNetTestProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Build the test projects under `root` and return the MTP applications.
    pub async fn build(&self) -> anyhow::Result<Vec<TestAssembly>> {
        let projects = ddbg_project::dotnet_projects(&self.root).unwrap_or_default();
        let mut out = Vec::new();
        for project in projects.into_iter().filter(|p| looks_like_test_project(p)) {
            let res = Command::new("dotnet")
                .arg("build")
                .arg(&project)
                .args([
                    "-nologo",
                    "-t:Build",
                    "-getProperty:TargetPath",
                    "-getProperty:IsTestingPlatformApplication",
                ])
                .stdin(Stdio::null())
                .output()
                .await
                .context("failed to run dotnet")?;
            let stdout = String::from_utf8_lossy(&res.stdout);
            if !res.status.success() {
                bail!(
                    "dotnet build {} failed:\n{}",
                    project.display(),
                    build_errors(&stdout)
                );
            }
            match parse_properties(&stdout) {
                Some((assembly, true)) => out.push(TestAssembly { project, assembly }),
                Some((_, false)) => tracing::debug!(?project, "not an MTP application"),
                None => tracing::warn!(?project, "could not read TargetPath"),
            }
        }
        if out.is_empty() {
            bail!(
                "no Microsoft.Testing.Platform test projects found in {}",
                self.root.display()
            );
        }
        Ok(out)
    }

    async fn list(&self, asm: &TestAssembly) -> anyhow::Result<(Framework, Vec<String>)> {
        let out = dotnet_exec(&asm.assembly, ["--list-tests", "--no-ansi"])
            .output()
            .await
            .context("failed to run dotnet")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!(
                "{} --list-tests failed:\n{}{}",
                asm.assembly.display(),
                stdout.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let (mut framework, names) = parse_list(&stdout);
        // The banner is not printed in every environment (e.g. CI), so also
        // detect xUnit v3 from the assemblies next to the test application.
        if output_dir(&asm.assembly).join("xunit.v3.core.dll").exists() {
            framework = Framework::XUnitV3;
        }
        Ok((framework, names))
    }
}

impl TestProvider for DotNetTestProvider {
    fn id(&self) -> ProviderId {
        ProviderId::DotNet
    }

    async fn discover(&self, query: &TestQuery) -> anyhow::Result<Vec<TestCase>> {
        let mut cases = Vec::new();
        for asm in self.build().await? {
            let (framework, names) = self.list(&asm).await?;
            let mut sources = None;
            for name in names.into_iter().filter(|n| query.matches(n)) {
                let mut case = test_case(&asm, &framework, name);
                let sources = sources.get_or_insert_with(|| {
                    asm.project.parent().map(read_sources).unwrap_or_default()
                });
                if let Some((file, line)) = locate_method(sources, method_name(&case.name)) {
                    case.source = Some(file);
                    case.line = Some(line);
                }
                cases.push(case);
            }
        }
        Ok(cases)
    }

    async fn run(&self, tests: &[TestId]) -> anyhow::Result<TestRunResult> {
        let mut run = TestRunResult::default();
        for id in tests {
            let (assembly, framework) = dotnet_data(id)?;
            let out = dotnet_exec(assembly, filter_args(framework, &id.name)?)
                .arg("--no-ansi")
                .args(progress_off(assembly))
                .output()
                .await
                .context("failed to run dotnet")?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let output = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
            run.results.push(TestResult {
                id: id.clone(),
                outcome: parse_summary(&stdout).unwrap_or(TestOutcome::Failed),
                output,
            });
        }
        Ok(run)
    }

    async fn debug_target(&self, test: &TestId) -> anyhow::Result<DebugTarget> {
        let (assembly, framework) = dotnet_data(test)?;
        let mut args = filter_args(framework, &test.name)?;
        args.push("--no-ansi".into());
        args.extend(progress_off(assembly).iter().map(|s| s.to_string()));
        if *framework == Framework::XUnitV3 {
            args.extend(["--parallel", "none"].map(String::from));
        }
        Ok(DebugTarget::Launch(LaunchTarget {
            program: assembly.to_owned(),
            args,
            cwd: output_dir(assembly),
            env: test_env(),
            stop_on_entry: false,
        }))
    }
}

/// Arguments that disable progress output. MTP v2 deprecated
/// `--no-progress` in favour of `--progress off`, which v1 lacks.
fn progress_off(assembly: &Path) -> &'static [&'static str] {
    let deps = std::fs::read_to_string(assembly.with_extension("deps.json")).unwrap_or_default();
    match mtp_major_version(&deps) {
        Some(v) if v >= 2 => &["--progress", "off"],
        _ => &["--no-progress"],
    }
}

/// Major version of Microsoft.Testing.Platform from a `.deps.json` file.
fn mtp_major_version(deps_json: &str) -> Option<u32> {
    const KEY: &str = "\"Microsoft.Testing.Platform/";
    let start = deps_json.find(KEY)? + KEY.len();
    let rest = &deps_json[start..];
    rest[..rest.find('.')?].parse().ok()
}

fn dotnet_data(id: &TestId) -> anyhow::Result<(&Path, &Framework)> {
    match &id.data {
        ProviderData::DotNet {
            assembly,
            framework,
            ..
        } => Ok((assembly, framework)),
        _ => bail!("test {} does not belong to the .NET provider", id.name),
    }
}

/// `dotnet <assembly> args...`, run from the output directory like
/// `dotnet test` does.
fn dotnet_exec<I, S>(assembly: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("dotnet");
    cmd.arg(assembly)
        .args(args)
        .envs(test_env())
        .current_dir(output_dir(assembly))
        .stdin(Stdio::null());
    cmd
}

fn output_dir(assembly: &Path) -> PathBuf {
    assembly
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_owned)
}

fn test_env() -> BTreeMap<String, String> {
    BTreeMap::from([("TESTINGPLATFORM_TELEMETRY_OPTOUT".into(), "1".into())])
}

/// Arguments selecting a single test (all rows, for a data-driven test).
fn filter_args(framework: &Framework, name: &str) -> anyhow::Result<Vec<String>> {
    match framework {
        Framework::XUnitV3 => Ok(vec!["--filter-method".into(), method_name(name).into()]),
        Framework::Other(banner) => bail!(
            "running single tests is not supported for this framework yet ({banner}); \
             only xUnit v3 is supported"
        ),
    }
}

/// `Ns.Class.Method(a: 1)` → `Ns.Class.Method`.
pub(crate) fn method_name(name: &str) -> &str {
    name.split_once('(').map_or(name, |(m, _)| m).trim_end()
}

fn test_case(asm: &TestAssembly, framework: &Framework, name: String) -> TestCase {
    let method = method_name(&name);
    let (suite, short) = match method.rsplit_once('.') {
        Some((suite, m)) => (Some(suite.to_owned()), m),
        None => (None, method),
    };
    let display_name = format!("{short}{}", &name[method.len()..]);
    TestCase {
        id: TestId {
            provider: ProviderId::DotNet,
            name: name.clone(),
            data: ProviderData::DotNet {
                assembly: asm.assembly.clone(),
                project: asm.project.clone(),
                framework: framework.clone(),
            },
        },
        name,
        display_name,
        source: None,
        line: None,
        suite,
    }
}

/// C# sources of a project directory (skipping `bin` and `obj`).
fn read_sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_dir() {
                let name = entry.file_name();
                if !matches!(name.to_str(), Some("bin" | "obj") | None)
                    && !name.to_string_lossy().starts_with('.')
                {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "cs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path, text));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Source file and 1-based line where the body of test method
/// `Ns.Class.Method` (or `Ns.Outer+Inner.Method`) starts.
///
/// A line breakpoint is needed rather than a function breakpoint: the body
/// of an `async` method lives in a compiler-generated state machine, so a
/// function breakpoint on the method name never binds.
fn locate_method(sources: &[(PathBuf, String)], name: &str) -> Option<(PathBuf, u32)> {
    let (qualified_class, method) = name.rsplit_once('.')?;
    let (namespace, class) = match qualified_class.rsplit_once('.') {
        Some((ns, c)) => (Some(ns), c),
        None => (None, qualified_class),
    };
    // Nested classes are reported as `Outer+Inner`; generic ones as `C`1`.
    let class = class.rsplit('+').next()?;
    let class = class.split('`').next()?;
    sources.iter().find_map(|(path, text)| {
        if let Some(ns) = namespace
            && !text.contains(&format!("namespace {ns}"))
        {
            return None;
        }
        let lines: Vec<&str> = text.lines().collect();
        let class_at = lines.iter().position(|l| declares(l, "class", class))?;
        let decl = (class_at..lines.len()).find(|&i| declares_method(lines[i], method))?;
        // Prefer the line with the opening brace, where the body starts.
        let body = (decl..lines.len().min(decl + 10))
            .find(|&i| {
                let l = strip_comment(lines[i]);
                l.contains('{') || l.contains("=>")
            })
            .unwrap_or(decl);
        Some((path.clone(), u32::try_from(body + 1).ok()?))
    })
}

fn strip_comment(line: &str) -> &str {
    line.split_once("//").map_or(line, |(code, _)| code)
}

/// `line` declares `<keyword> <name>` (e.g. `public sealed class Foo : Bar`).
fn declares(line: &str, keyword: &str, name: &str) -> bool {
    let mut words = strip_comment(line)
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty());
    while let Some(w) = words.next() {
        if w == keyword {
            return words.next() == Some(name);
        }
    }
    false
}

/// `line` looks like the declaration (not a call) of method `name`.
fn declares_method(line: &str, name: &str) -> bool {
    let code = strip_comment(line);
    let Some(at) = find_word(code, name) else {
        return false;
    };
    let before = code[..at].trim_end();
    let after = code[at + name.len()..].trim_start();
    // Declarations have a return type (an identifier, `>`, `]` or `?`)
    // right before the name and an argument list or generics after it.
    (after.starts_with('(') || after.starts_with('<'))
        && before
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || matches!(c, '>' | ']' | '?'))
        && !before.ends_with("new")
        && !before.ends_with("return")
        && !before.ends_with("await")
}

fn find_word(haystack: &str, word: &str) -> Option<usize> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    haystack.match_indices(word).map(|(i, _)| i).find(|&i| {
        !haystack[..i].chars().next_back().is_some_and(is_ident)
            && !haystack[i + word.len()..]
                .chars()
                .next()
                .is_some_and(is_ident)
    })
}

/// Cheap pre-filter so non-test projects are not built.
fn looks_like_test_project(project: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(project) else {
        return false;
    };
    const MARKERS: &[&str] = &[
        "IsTestProject",
        "IsTestingPlatformApplication",
        "Microsoft.Testing.Platform",
        "Microsoft.NET.Test.Sdk",
        "MSTest",
        "xunit",
        "TUnit",
        "NUnit",
    ];
    let lower = text.to_ascii_lowercase();
    MARKERS
        .iter()
        .any(|m| lower.contains(&m.to_ascii_lowercase()))
}

/// `TargetPath` and `IsTestingPlatformApplication` from
/// `dotnet build -getProperty:...` JSON output.
fn parse_properties(stdout: &str) -> Option<(PathBuf, bool)> {
    let start = stdout.find('{')?;
    let v: Value = serde_json::from_str(&stdout[start..]).ok()?;
    let props = &v["Properties"];
    let path = props["TargetPath"].as_str().filter(|s| !s.is_empty())?;
    let is_mtp = props["IsTestingPlatformApplication"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("true"));
    Some((PathBuf::from(path), is_mtp))
}

fn build_errors(stdout: &str) -> String {
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains(": error ")).collect();
    if errors.is_empty() {
        stdout.trim().to_owned()
    } else {
        errors.join("\n")
    }
}

/// Framework and test names from `--list-tests` output: a banner line, then
/// indented names, then a `Test discovery summary`.
pub fn parse_list(stdout: &str) -> (Framework, Vec<String>) {
    let banner = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let framework = if banner.starts_with("xUnit.net v3") {
        Framework::XUnitV3
    } else {
        Framework::Other(banner.to_owned())
    };
    let names = stdout
        .lines()
        .take_while(|l| !l.starts_with("Test discovery summary"))
        .filter(|l| l.starts_with("  ") && !l.trim().is_empty())
        .map(|l| l.trim().to_owned())
        .collect();
    (framework, names)
}

/// Outcome from the `Test run summary` block.
pub fn parse_summary(stdout: &str) -> Option<TestOutcome> {
    let summary = &stdout[stdout.find("Test run summary")?..];
    let count = |key: &str| -> Option<u32> {
        summary
            .lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .and_then(|v| v.trim().parse().ok())
    };
    let total = count("total:")?;
    Some(if total == 0 || count("failed:").unwrap_or(0) > 0 {
        TestOutcome::Failed
    } else if count("skipped:").unwrap_or(0) == total {
        TestOutcome::Ignored
    } else {
        TestOutcome::Passed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn summary_counts_determine_outcome(passed: u16, failed: u16, skipped: u16) -> bool {
        let total = u32::from(passed) + u32::from(failed) + u32::from(skipped);
        let summary = format!(
            "Test run summary: generated\n  total: {total}\n  failed: {failed}\n  succeeded: {passed}\n  skipped: {skipped}\n"
        );
        let expected = if total == 0 || failed > 0 {
            TestOutcome::Failed
        } else if passed == 0 {
            TestOutcome::Ignored
        } else {
            TestOutcome::Passed
        };
        parse_summary(&summary) == Some(expected)
    }

    const LIST: &str =
        "xUnit.net v3 Microsoft.Testing.Platform v1 Runner v3.2.2+728c1dce01 (64-bit .NET 10.0.12)


  HelloTests.CalculatorTests.Adds
  HelloTests.CalculatorTests.AddsMany(a: 1, b: 1, expected: 2)

Test discovery summary: found 2 test(s) - /x/HelloTests.dll (net10.0|arm64)
  duration: 66ms
";

    #[test]
    fn reads_mtp_major_version() {
        let deps = r#"{"libraries": {"Microsoft.Testing.Platform/2.4.0": {}, "Microsoft.Testing.Platform.MSBuild/2.4.0": {}}}"#;
        assert_eq!(mtp_major_version(deps), Some(2));
        assert_eq!(
            mtp_major_version(r#""Microsoft.Testing.Platform/1.9.1""#),
            Some(1)
        );
        assert_eq!(mtp_major_version("{}"), None);
    }

    #[test]
    fn parses_xunit_list() {
        let (fw, names) = parse_list(LIST);
        assert_eq!(fw, Framework::XUnitV3);
        assert_eq!(
            names,
            [
                "HelloTests.CalculatorTests.Adds",
                "HelloTests.CalculatorTests.AddsMany(a: 1, b: 1, expected: 2)"
            ]
        );
    }

    #[test]
    fn other_frameworks_are_recognised_but_not_filterable() {
        let (fw, names) =
            parse_list("MSTest v4.0.2 (UTC)\n\n  A\n  C (1)\n\nTest discovery summary: found 2");
        assert_eq!(fw, Framework::Other("MSTest v4.0.2 (UTC)".into()));
        assert_eq!(names, ["A", "C (1)"]);
        assert!(filter_args(&fw, "A").is_err());
    }

    #[test]
    fn test_case_names() {
        let asm = TestAssembly {
            project: "/p/T.csproj".into(),
            assembly: "/p/bin/T.dll".into(),
        };
        let c = test_case(&asm, &Framework::XUnitV3, "Ns.C.M(a: 1)".into());
        assert_eq!(c.display_name, "M(a: 1)");
        assert_eq!(c.suite.as_deref(), Some("Ns.C"));
        assert_eq!(
            filter_args(&Framework::XUnitV3, &c.name).unwrap(),
            ["--filter-method", "Ns.C.M"]
        );
    }

    #[test]
    fn parses_summaries() {
        let s = |total, failed, skipped| {
            format!(
                "Test run summary: x\n  total: {total}\n  failed: {failed}\n  succeeded: 0\n  skipped: {skipped}\n"
            )
        };
        assert_eq!(parse_summary(&s(1, 0, 0)), Some(TestOutcome::Passed));
        assert_eq!(parse_summary(&s(2, 1, 0)), Some(TestOutcome::Failed));
        assert_eq!(parse_summary(&s(1, 0, 1)), Some(TestOutcome::Ignored));
        assert_eq!(parse_summary(&s(0, 0, 0)), Some(TestOutcome::Failed));
        assert_eq!(parse_summary("crashed"), None);
    }

    #[test]
    fn locates_test_methods() {
        let src = "namespace Ns.Web;

public class Other { public void Run() { } }

public class Tests : IAsyncDisposable
{
    [Fact]
    public void Run() => Assert.True(true);

    [Fact]
    public async Task ShouldPost() // comment {
    {
        await ShouldPost2();
    }

    public class Inner
    {
        [Fact] public void Deep()
        {
        }
    }
}
";
        let sources = vec![(PathBuf::from("/p/T.cs"), src.to_owned())];
        let at = |n: &str| locate_method(&sources, n).map(|(_, l)| l);
        assert_eq!(at("Ns.Web.Tests.ShouldPost"), Some(12));
        assert_eq!(at("Ns.Web.Tests.Run"), Some(8));
        assert_eq!(at("Ns.Web.Tests+Inner.Deep"), Some(19));
        assert_eq!(at("Ns.Web.Tests.Missing"), None);
        assert_eq!(at("Other.Ns.Tests.ShouldPost"), None);
    }

    #[test]
    fn parses_build_properties() {
        let out = r#"{
  "Properties": {
    "TargetPath": "/p/bin/Debug/net10.0/T.dll",
    "IsTestingPlatformApplication": "true"
  }
}"#;
        assert_eq!(
            parse_properties(out),
            Some(("/p/bin/Debug/net10.0/T.dll".into(), true))
        );
    }
}
