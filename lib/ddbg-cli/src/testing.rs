//! `tests`, `test-run` and `test-debug`: the frontend drives a
//! [`TestProvider`] and hands the resulting [`DebugTarget`] to the engine.

use std::fmt::Write;

use anyhow::{anyhow, bail};
use ddbg_core::breakpoint::SourceLocation;
use ddbg_core::command::{FunctionLocation, Location, TestQuery as QueryCmd, TestSelector};
use ddbg_core::{DebugTarget, LaunchTarget};
use ddbg_test::{AnyProvider, TestCase, TestOutcome, TestProvider, TestQuery, TestRunResult};

/// Test state for the REPL: the provider and the last displayed list.
pub struct Tests {
    provider: Option<AnyProvider>,
    /// Why there is no provider, shown when a test command is used.
    unavailable: String,
    /// Most recently displayed list; `test-run 2` indexes into it.
    listed: Vec<TestCase>,
}

impl Tests {
    pub fn new(provider: Option<AnyProvider>, unavailable: impl Into<String>) -> Self {
        Self {
            provider,
            unavailable: unavailable.into(),
            listed: Vec::new(),
        }
    }

    fn provider(&self) -> anyhow::Result<&AnyProvider> {
        self.provider
            .as_ref()
            .ok_or_else(|| anyhow!("{}", self.unavailable))
    }

    /// `tests [filter]`: discover and number matching tests.
    pub async fn list(&mut self, query: &QueryCmd) -> anyhow::Result<Vec<TestCase>> {
        let query = TestQuery {
            filter: query.filter.clone(),
        };
        self.listed = self.provider()?.discover(&query).await?;
        Ok(self.listed.clone())
    }

    /// `test-run <test>`.
    pub async fn run(&mut self, sel: &TestSelector) -> anyhow::Result<String> {
        let test = self.select(sel).await?;
        let result = self.provider()?.run(&[test.id]).await?;
        Ok(render_run(&result))
    }

    /// `test-debug <test>`: the launch target for the engine, plus the
    /// location of the start of the test (for `test-debug -b`).
    pub async fn debug_target(
        &mut self,
        sel: &TestSelector,
    ) -> anyhow::Result<(LaunchTarget, Location)> {
        let test = self.select(sel).await?;
        match self.provider()?.debug_target(&test.id).await? {
            DebugTarget::Launch(t) => Ok((t, start_location(&test))),
            DebugTarget::Attach(_) => bail!("attach targets are not supported yet"),
        }
    }

    async fn select(&mut self, sel: &TestSelector) -> anyhow::Result<TestCase> {
        match sel {
            TestSelector::Index(n) => pick_index(&self.listed, *n),
            TestSelector::Name(name) => {
                // Always rediscover so the binary is rebuilt from current sources.
                let found = self.provider()?.discover(&TestQuery::new(name)).await?;
                let picked = pick_name(&found, name);
                if picked.is_err() && found.len() > 1 {
                    self.listed = found;
                }
                picked
            }
        }
    }
}

/// Where a test starts: its source line when known, else its function.
fn start_location(test: &TestCase) -> Location {
    match (&test.source, test.line) {
        (Some(file), Some(line)) => Location::Source(SourceLocation::new(file, line)),
        _ => Location::Function(FunctionLocation::new(test.name.as_str(), None)),
    }
}

fn pick_index(listed: &[TestCase], n: usize) -> anyhow::Result<TestCase> {
    if listed.is_empty() {
        bail!("no tests listed; run `tests` first");
    }
    n.checked_sub(1)
        .and_then(|i| listed.get(i))
        .cloned()
        .ok_or_else(|| anyhow!("no test {n}; valid range is 1-{}", listed.len()))
}

/// Exact name, then exact display name, then a unique substring match.
fn pick_name(found: &[TestCase], name: &str) -> anyhow::Result<TestCase> {
    let unique = |v: Vec<&TestCase>| match v.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    };
    if let Some(t) = unique(found.iter().filter(|t| t.name == name).collect())
        .or_else(|| unique(found.iter().filter(|t| t.display_name == name).collect()))
    {
        return Ok(t);
    }
    match found {
        [] => bail!("no test matches `{name}`"),
        [one] => Ok(one.clone()),
        many => bail!(
            "`{name}` matches {} tests; pick one by number:\n{}",
            many.len(),
            render_list(many)
        ),
    }
}

/// Numbered test list, as shown by `tests`.
pub fn render_list(tests: &[TestCase]) -> String {
    if tests.is_empty() {
        return "no tests found".into();
    }
    let width = tests.len().to_string().len();
    let mut s = String::new();
    for (i, t) in tests.iter().enumerate() {
        let _ = writeln!(s, "{:>width$}  {}", i + 1, t.name);
    }
    s.pop();
    s
}

fn render_run(run: &TestRunResult) -> String {
    let mut s = String::new();
    for r in &run.results {
        let label = match r.outcome {
            TestOutcome::Passed => "PASS",
            TestOutcome::Failed => "FAIL",
            TestOutcome::Ignored => "SKIP",
        };
        let _ = writeln!(s, "{label}  {}", r.id.name);
        if r.outcome == TestOutcome::Failed {
            let _ = writeln!(s, "{}", r.output.trim_end());
        }
    }
    let _ = write!(
        s,
        "{} passed, {} failed, {} ignored",
        run.count(TestOutcome::Passed),
        run.count(TestOutcome::Failed),
        run.count(TestOutcome::Ignored)
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddbg_test::{ProviderData, ProviderId, TestId};

    fn case(name: &str) -> TestCase {
        TestCase {
            id: TestId {
                provider: ProviderId::Rust,
                name: name.into(),
                data: ProviderData::Rust {
                    binary: "/b".into(),
                    package: "p".into(),
                    target: "p".into(),
                    package_dir: "/p".into(),
                },
            },
            name: name.into(),
            display_name: name.rsplit("::").next().unwrap().into(),
            source: None,
            line: None,
            suite: None,
        }
    }

    #[test]
    fn selects_by_index() {
        let l = [case("a::x"), case("a::y")];
        assert_eq!(pick_index(&l, 2).unwrap().name, "a::y");
        assert!(pick_index(&l, 0).is_err());
        assert!(pick_index(&l, 3).is_err());
        assert!(pick_index(&[], 1).is_err());
    }

    #[test]
    fn selects_by_name() {
        let l = [case("a::parse"), case("a::parse_all"), case("b::lex")];
        assert_eq!(pick_name(&l, "a::parse").unwrap().name, "a::parse");
        assert_eq!(pick_name(&l, "parse").unwrap().name, "a::parse");
        assert_eq!(pick_name(&l[2..], "le").unwrap().name, "b::lex");
        assert!(pick_name(&l[..2], "pars").is_err());
        assert!(pick_name(&[], "x").is_err());
    }

    #[test]
    fn renders_numbered_list() {
        let l: Vec<_> = (0..10).map(|i| case(&format!("t{i}"))).collect();
        let s = render_list(&l);
        assert!(s.starts_with(" 1  t0\n"));
        assert!(s.ends_with("10  t9"));
    }
}
