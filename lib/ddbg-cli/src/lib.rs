//! Command-line frontend.

mod adapter_test;
mod args;
pub mod commands;
pub mod functions;
mod highlight;
mod logging;
pub mod parser;
pub mod render;
mod repl;
pub mod session;
pub mod testing;

use std::sync::Arc;

pub use clap::Parser;
use ddbg_core::adapter::{
    DebugAdapter, DebugpyAdapter, LldbDapAdapter, NetCoreDbgAdapter, adapter_for_command,
};
use ddbg_core::command::Command;
use ddbg_core::{EngineConfig, LaunchTarget, engine};
use ddbg_project::{Project, ProjectKind};
use ddbg_test::{AnyProvider, DotNetTestProvider, RustTestProvider};

pub use args::{Args, Subcommand};
pub use session::{Outcome, Session, TestCase};

/// Everything a frontend needs to start: a running engine and setup.
pub struct Prepared {
    pub session: Session,
    /// Commands to execute before handing control to the user.
    pub initial: Vec<Command>,
    pub verbose: bool,
    /// Program the engine starts with, if one was given or detected. When
    /// `None`, frontends can offer [`Session::candidates`] to pick from.
    pub program: Option<std::path::PathBuf>,
    /// Kind of the detected project, if any.
    pub project: Option<ProjectKind>,
    /// Messages about project detection (chosen target, unbuilt binaries,
    /// multiple candidates). [`start`] prints them to stderr.
    pub notes: Vec<String>,
}

/// Entry point used by the `ddbg` binary when no other frontend is chosen.
pub async fn run() -> anyhow::Result<()> {
    run_with(Args::parse()).await
}

/// Run the REPL frontend with already-parsed arguments.
pub async fn run_with(args: Args) -> anyhow::Result<()> {
    match start(&args).await? {
        Some(p) => repl::run(p.session, p.initial, p.verbose).await,
        None => Ok(()),
    }
}

/// Initialize logging as configured by `--log-dap` and `DDBG_LOG`. Logs go
/// to a file, never to stdout or stderr.
pub fn init_logging(args: &Args) -> anyhow::Result<()> {
    logging::init(args)
}

/// Initialize logging and handle subcommands. Returns `None` when a
/// subcommand ran to completion, otherwise the prepared debug session.
pub async fn start(args: &Args) -> anyhow::Result<Option<Prepared>> {
    logging::init(args)?;

    if let Some(Subcommand::AdapterTest { adapter }) = &args.command {
        adapter_test::run(adapter).await?;
        return Ok(None);
    }
    if let Some(Subcommand::Mcp) = &args.command {
        anyhow::bail!("`ddbg mcp` is handled by the ddbg binary");
    }

    let prepared = prepare(args, std::env::current_dir()?)?;
    for note in &prepared.notes {
        eprintln!("{note}");
    }
    Ok(Some(prepared))
}

/// Build a debug session from `args` as if `ddbg` was started in `cwd`,
/// without initializing logging or running subcommands. Used by
/// [`start`] and by programmatic drivers such as `ddbg-driver`.
pub fn prepare(args: &Args, cwd: std::path::PathBuf) -> anyhow::Result<Prepared> {
    let mut target = args
        .program
        .split_first()
        .map(|(program, rest)| LaunchTarget::new(program, rest.to_vec()));
    let project = if target.is_none() && !args.no_detect {
        ddbg_project::detect(&cwd)
    } else {
        None
    };
    let mut notes = Vec::new();
    if let Some(project) = &project {
        target = discover_target(project, &mut notes);
    }
    if let Some(t) = &mut target {
        t.cwd.clone_from(&cwd);
        t.stop_on_entry = args.stop_on_entry;
        apply_dotnet_launch(t);
    }

    let adapter: Arc<dyn DebugAdapter> = match &args.adapter {
        Some(cmd) => {
            let words = parser::split_words(cmd).map_err(anyhow::Error::msg)?;
            let (program, rest) = words
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("empty --adapter"))?;
            adapter_for_command(program, rest.to_vec()).into()
        }
        None => {
            let kind = project
                .as_ref()
                .map(|p| p.kind.clone())
                .or_else(|| target.as_ref().and_then(kind_of_target));
            let root = project.as_ref().map_or(cwd.as_path(), |p| &p.root);
            default_adapter(kind.as_ref(), root)
        }
    };

    let mut initial = Vec::new();
    if args.run || (args.stop_on_entry && target.is_some()) {
        initial.push(Command::Run(None));
    }

    let program = target.as_ref().map(|t| t.program.clone());
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
                ProjectKind::Rust => Some(AnyProvider::Rust(RustTestProvider::new(&p.root))),
                ProjectKind::DotNet => Some(AnyProvider::DotNet(DotNetTestProvider::new(&p.root))),
                ProjectKind::Python | ProjectKind::C => None,
            };
            let msg = format!(
                "test discovery is not supported for {:?} projects yet",
                p.kind
            );
            testing::Tests::new(provider, msg)
        }
        None => testing::Tests::new(None, "no project detected; cannot discover tests"),
    };
    let candidates = project.map(|p| p.candidates).unwrap_or_default();
    Ok(Prepared {
        session: Session {
            engine,
            cwd,
            candidates,
            tests,
        },
        initial,
        verbose: args.verbose,
        program,
        project: test_project.map(|p| p.kind),
        notes,
    })
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

/// Guess the language of a program given on the command line.
fn kind_of_target(t: &LaunchTarget) -> Option<ProjectKind> {
    if t.program.as_os_str() == "-m" {
        return Some(ProjectKind::Python);
    }
    match t.program.extension()?.to_str()? {
        "dll" => Some(ProjectKind::DotNet),
        "py" | "pyw" => Some(ProjectKind::Python),
        _ => None,
    }
}

/// The adapter to use when `--adapter` is not given. Unknown programs are
/// assumed to be native and debugged with lldb-dap.
fn default_adapter(kind: Option<&ProjectKind>, root: &std::path::Path) -> Arc<dyn DebugAdapter> {
    match kind {
        Some(ProjectKind::DotNet) => Arc::new(NetCoreDbgAdapter::default()),
        Some(ProjectKind::Python) => Arc::new(match ddbg_project::python_interpreter(root) {
            Some(python) => DebugpyAdapter::with_python(python.to_string_lossy()),
            None => DebugpyAdapter::default(),
        }),
        Some(ProjectKind::C) => Arc::new(LldbDapAdapter::default()),
        Some(ProjectKind::Rust) | None => Arc::new(LldbDapAdapter::for_rust()),
    }
}

/// Pick a binary to debug.
fn discover_target(project: &Project, notes: &mut Vec<String>) -> Option<LaunchTarget> {
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
            notes.push(format!(
                "{:?} project at {}: multiple binaries:\n  {}\nuse `run <name>` to pick one",
                project.kind,
                project.root.display(),
                names.join("\n  "),
            ));
        }
        return None;
    };
    let note = if binary.exists() {
        ""
    } else {
        match project.kind {
            ProjectKind::Rust => " (not built yet; run `cargo build`)",
            ProjectKind::DotNet => " (not built yet; run `dotnet build`)",
            ProjectKind::C => " (not built yet)",
            ProjectKind::Python => " (not found)",
        }
    };
    notes.push(format!(
        "{:?} project at {}: target {}{note}",
        project.kind,
        project.root.display(),
        binary.display()
    ));
    Some(LaunchTarget::new(binary, Vec::new()))
}
