//! Command-line frontend.

mod adapter_test;
mod args;
mod commands;
mod logging;
pub mod parser;
mod render;
mod repl;
mod testing;

use std::sync::Arc;

use clap::Parser;
use ddbg_core::adapter::{DebugAdapter, LldbDapAdapter, NetCoreDbgAdapter, adapter_for_command};
use ddbg_core::command::Command;
use ddbg_core::{EngineConfig, LaunchTarget, engine};
use ddbg_project::{Project, ProjectKind};
use ddbg_test::{AnyProvider, DotNetTestProvider, RustTestProvider};

pub use args::{Args, Subcommand};

/// Entry point used by the `ddbg` binary.
pub async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    logging::init(&args)?;

    if let Some(Subcommand::AdapterTest { adapter }) = &args.command {
        return adapter_test::run(adapter).await;
    }

    let cwd = std::env::current_dir()?;

    let mut target = args
        .program
        .split_first()
        .map(|(program, rest)| LaunchTarget::new(program, rest.to_vec()));
    let project = if target.is_none() && !args.no_detect {
        ddbg_project::detect(&cwd)
    } else {
        None
    };
    if let Some(project) = &project {
        target = discover_target(project);
    }
    if let Some(t) = &mut target {
        t.stop_on_entry = args.stop_on_entry;
        apply_dotnet_launch(t);
    }

    let is_dotnet = match &project {
        Some(p) => p.kind == ProjectKind::DotNet,
        None => target
            .as_ref()
            .is_some_and(|t| t.program.extension().is_some_and(|e| e == "dll")),
    };
    let adapter: Arc<dyn DebugAdapter> = match &args.adapter {
        Some(cmd) => {
            let words = parser::split_words(cmd).map_err(anyhow::Error::msg)?;
            let (program, rest) = words
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("empty --adapter"))?;
            adapter_for_command(program, rest.to_vec()).into()
        }
        None if is_dotnet => Arc::new(NetCoreDbgAdapter::default()),
        None => Arc::new(LldbDapAdapter::for_rust()),
    };

    let mut initial = Vec::new();
    if args.run || (args.stop_on_entry && target.is_some()) {
        initial.push(Command::Run(None));
    }

    let engine = engine::spawn(EngineConfig {
        adapter,
        cwd: cwd.clone(),
        target,
    });
    // Detection is skipped when a program is given; tests still need it.
    let test_project = project.clone().or_else(|| {
        (!args.no_detect)
            .then(|| ddbg_project::detect(&cwd))
            .flatten()
    });
    let tests = match &test_project {
        Some(p) => {
            let provider = match p.kind {
                ProjectKind::Rust => AnyProvider::Rust(RustTestProvider::new(&p.root)),
                ProjectKind::DotNet => AnyProvider::DotNet(DotNetTestProvider::new(&p.root)),
            };
            testing::Tests::new(Some(provider), "")
        }
        None => testing::Tests::new(None, "no project detected; cannot discover tests"),
    };
    let candidates = project.map(|p| p.candidates).unwrap_or_default();
    repl::run(engine, cwd, initial, args.verbose, candidates, tests).await
}

/// For .NET assemblies, run from the project directory with the environment
/// `dotnet run` would use, so `appsettings.{Environment}.json` is picked up.
pub(crate) fn apply_dotnet_launch(t: &mut LaunchTarget) {
    if t.program.extension().is_none_or(|e| e != "dll") {
        return;
    }
    let program = t.absolute_program();
    let Some(launch) = ddbg_project::dotnet_launch(&program) else {
        return;
    };
    t.program = program;
    t.cwd = launch.cwd;
    for (k, v) in launch.env {
        t.env.entry(k).or_insert(v);
    }
}

/// Pick a binary to debug.
fn discover_target(project: &Project) -> Option<LaunchTarget> {
    tracing::debug!(?project, "detected project");
    let Some(binary) = project.binary.clone() else {
        if !project.candidates.is_empty() {
            let names: Vec<_> = project
                .candidates
                .iter()
                .map(|p| {
                    let rel = p.strip_prefix(&project.root).unwrap_or(p);
                    rel.display().to_string()
                })
                .collect();
            eprintln!(
                "{:?} project at {}: multiple binaries:\n  {}\nuse `run <name>` to pick one",
                project.kind,
                project.root.display(),
                names.join("\n  "),
            );
        }
        return None;
    };
    let note = if binary.exists() {
        ""
    } else {
        match project.kind {
            ProjectKind::Rust => " (not built yet; run `cargo build`)",
            ProjectKind::DotNet => " (not built yet; run `dotnet build`)",
        }
    };
    eprintln!(
        "{:?} project at {}: target {}{note}",
        project.kind,
        project.root.display(),
        binary.display()
    );
    Some(LaunchTarget::new(binary, Vec::new()))
}
