//! Test subsystem: language-independent test model and providers.
//!
//! Providers discover and run tests, and translate a test into a
//! [`DebugTarget`]. They never debug processes themselves.

pub mod dotnet;
pub mod rust;

use std::path::PathBuf;

use ddbg_core::DebugTarget;

pub use dotnet::DotNetTestProvider;
pub use rust::RustTestProvider;

/// Identifies which provider owns a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    Rust,
    DotNet,
}

/// Provider-specific identity needed to locate and run a test.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderData {
    Rust {
        /// Test executable produced by Cargo.
        binary: PathBuf,
        /// Cargo package the executable belongs to.
        package: String,
        /// Cargo target name (e.g. crate name or integration test file).
        target: String,
        /// Package directory; the working directory for runs.
        package_dir: PathBuf,
    },
    DotNet {
        /// Built test assembly (`.dll`).
        assembly: PathBuf,
        /// Project file the assembly is built from.
        project: PathBuf,
        framework: dotnet::Framework,
    },
}

/// Structured test identity. Unique across providers; never just the display
/// name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TestId {
    pub provider: ProviderId,
    /// Fully-qualified name as understood by the test harness.
    pub name: String,
    pub data: ProviderData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    pub id: TestId,
    /// Fully-qualified harness name, e.g. `parser::tests::empty_input`.
    pub name: String,
    pub display_name: String,
    pub source: Option<PathBuf>,
    pub line: Option<u32>,
    pub suite: Option<String>,
}

impl TestCase {
    /// Name of the function implementing the test, usable as a debugger
    /// function breakpoint. Data-driven .NET tests carry their arguments
    /// in the name (`Ns.Class.Method(a: 1)`), which debuggers cannot bind.
    pub fn function_name(&self) -> &str {
        match self.id.provider {
            ProviderId::DotNet => dotnet::method_name(&self.name),
            _ => &self.name,
        }
    }
}

/// Filter for discovery. An empty filter matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestQuery {
    /// Case-insensitive substring matched against the test name.
    pub filter: Option<String>,
}

impl TestQuery {
    pub fn new(filter: impl Into<String>) -> Self {
        let f = filter.into();
        Self {
            filter: (!f.is_empty()).then_some(f),
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        match &self.filter {
            None => true,
            Some(f) => name.to_lowercase().contains(&f.to_lowercase()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Passed,
    Failed,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub id: TestId,
    pub outcome: TestOutcome,
    /// Captured stdout+stderr of the run that executed this test.
    pub output: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestRunResult {
    pub results: Vec<TestResult>,
}

impl TestRunResult {
    pub fn count(&self, outcome: TestOutcome) -> usize {
        self.results.iter().filter(|r| r.outcome == outcome).count()
    }
}

#[allow(async_fn_in_trait)]
pub trait TestProvider {
    fn id(&self) -> ProviderId;

    async fn discover(&self, query: &TestQuery) -> anyhow::Result<Vec<TestCase>>;

    async fn run(&self, tests: &[TestId]) -> anyhow::Result<TestRunResult>;

    async fn debug_target(&self, test: &TestId) -> anyhow::Result<DebugTarget>;
}

/// Any supported provider, for frontends that pick one at runtime.
#[derive(Debug, Clone)]
pub enum AnyProvider {
    Rust(RustTestProvider),
    DotNet(DotNetTestProvider),
}

impl TestProvider for AnyProvider {
    fn id(&self) -> ProviderId {
        match self {
            Self::Rust(p) => p.id(),
            Self::DotNet(p) => p.id(),
        }
    }

    async fn discover(&self, query: &TestQuery) -> anyhow::Result<Vec<TestCase>> {
        match self {
            Self::Rust(p) => p.discover(query).await,
            Self::DotNet(p) => p.discover(query).await,
        }
    }

    async fn run(&self, tests: &[TestId]) -> anyhow::Result<TestRunResult> {
        match self {
            Self::Rust(p) => p.run(tests).await,
            Self::DotNet(p) => p.run(tests).await,
        }
    }

    async fn debug_target(&self, test: &TestId) -> anyhow::Result<DebugTarget> {
        match self {
            Self::Rust(p) => p.debug_target(test).await,
            Self::DotNet(p) => p.debug_target(test).await,
        }
    }
}
