//! Frontend-shared command dispatch: test commands, program resolution and
//! the engine round-trip. Used by the REPL and the TUI.

use std::path::{Path, PathBuf};

use ddbg_core::command::Command;
use ddbg_core::{EngineHandle, Reply};

use crate::testing::Tests;

/// Result of executing one command, for a frontend to present.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // short-lived, one per command
pub enum Outcome {
    /// The engine answered; render with [`crate::render::Renderer::reply`].
    Reply(Command, Reply),
    /// Preformatted text (test lists and test runs).
    Text(String),
    Error(String),
    Quit,
}

pub struct Session {
    pub engine: EngineHandle,
    pub cwd: PathBuf,
    /// Binaries found by project detection, used to resolve `run <name>`.
    pub candidates: Vec<PathBuf>,
    pub tests: Tests,
}

impl Session {
    /// Execute `cmd`. `progress` receives status lines for slow operations.
    pub async fn execute(
        &mut self,
        mut cmd: Command,
        progress: &mut (dyn FnMut(&str) + Send),
    ) -> Outcome {
        // Test commands are handled by the frontend; only the resulting
        // debug target reaches the engine.
        let text = match &cmd {
            Command::Tests(q) => {
                progress("discovering tests...");
                Some(self.tests.list(q).await)
            }
            Command::TestRun(sel) => {
                progress("building tests...");
                Some(self.tests.run(sel).await)
            }
            Command::TestDebug(sel) => {
                progress("building tests...");
                // Test targets are complete; no program resolution needed.
                match self.tests.debug_target(sel).await {
                    Ok(t) => return self.engine_execute(Command::Run(Some(t))).await,
                    Err(e) => return Outcome::Error(format!("{e:#}")),
                }
            }
            _ => None,
        };
        if let Some(text) = text {
            return match text {
                Ok(t) => Outcome::Text(t),
                Err(e) => Outcome::Error(format!("{e:#}")),
            };
        }
        if let Command::Run(Some(t)) = &mut cmd {
            if let Some(p) = resolve_program(&t.program, &self.cwd, &self.candidates) {
                t.program = p;
            }
            crate::apply_dotnet_launch(t);
        }
        self.engine_execute(cmd).await
    }

    async fn engine_execute(&self, cmd: Command) -> Outcome {
        match self.engine.execute(cmd.clone()).await {
            Ok(Reply::Quit) => Outcome::Quit,
            Ok(reply) => Outcome::Reply(cmd, reply),
            Err(e) => Outcome::Error(e.to_string()),
        }
    }
}

/// Resolve a bare program name (e.g. `App.dll`, `App` or `my-bin`) against
/// detected project binaries when it does not exist relative to `cwd`.
pub(crate) fn resolve_program(
    program: &Path,
    cwd: &Path,
    candidates: &[PathBuf],
) -> Option<PathBuf> {
    if program.components().count() != 1 || cwd.join(program).exists() {
        return None;
    }
    candidates
        .iter()
        .find(|c| c.file_name() == Some(program.as_os_str()))
        .or_else(|| {
            candidates
                .iter()
                .find(|c| c.file_stem() == Some(program.as_os_str()))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bare_names_against_candidates() {
        let cands = vec![
            PathBuf::from("/p/A/bin/Debug/net8.0/My.A.dll"),
            PathBuf::from("/p/target/debug/tool"),
        ];
        let cwd = Path::new("/nonexistent-ddbg-cwd");
        let r = |s: &str| resolve_program(Path::new(s), cwd, &cands);
        assert_eq!(r("My.A.dll"), Some(cands[0].clone()));
        assert_eq!(r("My.A"), Some(cands[0].clone()));
        assert_eq!(r("tool"), Some(cands[1].clone()));
        assert_eq!(r("other"), None);
        assert_eq!(r("./tool"), None);
    }
}
