//! Tab completion for the REPL.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ddbg_core::{EngineHandle, Reply, command::Command};
use reedline::{Completer, CompletionResult, Span, Suggestion};

use crate::parser::{COMMANDS, lookup};

/// Completion runs on the editor thread. Only explicit Tab requests query
/// the engine; a slow/unavailable adapter simply yields no live suggestions.
pub struct DdbgCompleter {
    pub cwd: PathBuf,
    pub tests: Arc<Mutex<Vec<String>>>,
    pub engine: EngineHandle,
    pub runtime: tokio::runtime::Handle,
    pub functions: Option<Vec<crate::functions::Function>>,
}

impl DdbgCompleter {
    fn suggestions(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let Some(before) = line.get(..pos) else {
            return Vec::new();
        };
        let Some((command, rest)) = before.trim_start().split_once(char::is_whitespace) else {
            return suggestions(line, pos);
        };
        let Some(command) = lookup(command).map(|c| c.name) else {
            return Vec::new();
        };
        let start = pos - rest.trim_start().len();
        let mut span = Span::new(start, pos);
        let mut prefix = &before[start..];
        let mut candidates = Vec::new();
        match command {
            "break" => {
                candidates.extend(complete_path(prefix, span, &self.cwd));
                let functions = self
                    .functions
                    .get_or_insert_with(|| crate::functions::discover(&self.cwd));
                // A single colon scopes a function to a file; `::` belongs
                // to a qualified Rust/C++ function name.
                let scope = prefix
                    .char_indices()
                    .find(|&(i, c)| {
                        c == ':' && !prefix[..i].ends_with(':') && !prefix[i + 1..].starts_with(':')
                    })
                    .map(|(i, _)| &prefix[..i]);
                for f in functions {
                    if let Some(file) = scope {
                        if !f.path.ends_with(file) {
                            continue;
                        }
                        candidates.push(candidate(
                            format!("{file}:{}", f.name),
                            f.label.clone(),
                            span,
                        ));
                    } else {
                        candidates.push(candidate(f.name.clone(), f.label.clone(), span));
                    }
                }
            }
            "test-run" | "test-debug" | "tests" => {
                if command == "test-debug" {
                    if let Some(rest) = prefix
                        .strip_prefix("--break ")
                        .or_else(|| prefix.strip_prefix("-b "))
                    {
                        prefix = rest.trim_start();
                        span = Span::new(pos - prefix.len(), pos);
                    } else {
                        for flag in ["-b", "--break"] {
                            candidates.push(candidate(
                                flag.into(),
                                "Break at test start".into(),
                                span,
                            ));
                        }
                    }
                }
                for (i, name) in self.tests.lock().unwrap().iter().enumerate() {
                    candidates.push(candidate(name.clone(), "Test".into(), span));
                    if command != "tests" {
                        candidates.push(candidate((i + 1).to_string(), name.clone(), span));
                    }
                }
            }
            "delete" | "thread" | "frame" | "print" => {
                let query = match command {
                    "delete" => Command::Breakpoints,
                    "thread" => Command::Threads,
                    "frame" => Command::Backtrace,
                    _ => Command::Locals,
                };
                if let Ok(Ok(reply)) = self.runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(500), self.engine.execute(query))
                        .await
                }) {
                    candidates.extend(reply_candidates(reply, span));
                }
            }
            "run" => return suggestions_in(line, pos, &self.cwd),
            "help" => return suggestions(line, pos),
            _ => {}
        }
        candidates.retain(|s| s.value.starts_with(prefix));
        candidates.sort_by(|a, b| a.value.cmp(&b.value));
        candidates.dedup_by(|a, b| a.value == b.value);
        candidates
    }
}

fn candidate(value: String, description: String, span: Span) -> Suggestion {
    Suggestion {
        value,
        description: Some(description),
        span,
        append_whitespace: true,
        ..Default::default()
    }
}

fn reply_candidates(reply: Reply, span: Span) -> Vec<Suggestion> {
    match reply {
        Reply::Breakpoints(bps) => bps
            .into_iter()
            .map(|b| candidate(b.id.to_string(), b.requested.to_string(), span))
            .collect(),
        Reply::Threads { threads, .. } => threads
            .into_iter()
            .map(|t| candidate(t.id.0.to_string(), t.name, span))
            .collect(),
        Reply::Backtrace { frames, .. } => frames
            .into_iter()
            .enumerate()
            .map(|(i, f)| candidate(i.to_string(), f.name, span))
            .collect(),
        Reply::Locals(scopes) => scopes
            .into_iter()
            .flat_map(|s| s.variables)
            .map(|v| candidate(v.name, v.type_name.unwrap_or_default(), span))
            .collect(),
        _ => Vec::new(),
    }
}

impl Completer for DdbgCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        CompletionResult::fresh(self.suggestions(line, pos))
    }
}

pub fn suggestions(line: &str, pos: usize) -> Vec<Suggestion> {
    suggestions_in(line, pos, Path::new("."))
}

fn suggestions_in(line: &str, pos: usize, cwd: &Path) -> Vec<Suggestion> {
    let Some(line) = line.get(..pos) else {
        return Vec::new();
    };
    let start = line
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = &line[start..];
    let span = Span::new(start, pos);

    let first = line.trim_start();
    let Some((command, _)) = first.split_once(char::is_whitespace) else {
        // Completing the command word itself.
        return COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(word))
            .map(|c| Suggestion {
                value: c.name.to_owned(),
                description: Some(c.help.to_owned()),
                span,
                append_whitespace: true,
                ..Default::default()
            })
            .collect();
    };

    match lookup(command).map(|c| c.name) {
        Some("break" | "run") => complete_path(word, span, cwd),
        Some("help") => COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(word))
            .map(|c| Suggestion {
                value: c.name.to_owned(),
                span,
                ..Default::default()
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn complete_path(word: &str, span: Span, cwd: &Path) -> Vec<Suggestion> {
    let (dir, prefix) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let read_dir = cwd.join(dir);
    let Ok(entries) = std::fs::read_dir(read_dir) else {
        return Vec::new();
    };
    let mut out: Vec<Suggestion> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let suffix = if is_dir { "/" } else { "" };
            Some(Suggestion {
                value: format!("{dir}{name}{suffix}"),
                span,
                append_whitespace: false,
                ..Default::default()
            })
        })
        .collect();
    out.sort_by(|a, b| a.value.cmp(&b.value));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completer() -> DdbgCompleter {
        let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        DdbgCompleter {
            engine: ddbg_core::engine::spawn(ddbg_core::EngineConfig {
                adapter: Arc::new(ddbg_core::adapter::LldbDapAdapter::default()),
                cwd: cwd.clone(),
                target: None,
            }),
            cwd,
            runtime: tokio::runtime::Handle::current(),
            tests: Arc::new(Mutex::new(vec![
                "parser::empty".into(),
                "a test with spaces".into(),
            ])),
            functions: None,
        }
    }

    #[tokio::test]
    async fn completes_tests_flags_aliases_and_replaces_the_entire_selector() {
        let mut c = completer();
        for line in ["tr par", "td -b par", "test-debug --break par", "tests par"] {
            let found = c.suggestions(line, line.len());
            assert_eq!(found.len(), 1, "{line}");
            assert_eq!(found[0].value, "parser::empty");
            assert_eq!(&line[found[0].span.start..found[0].span.end], "par");
        }
        assert_eq!(
            c.suggestions("td 2", 4)[0].description.as_deref(),
            Some("a test with spaces")
        );
        assert_eq!(c.suggestions("td --br", 7)[0].value, "--break");
        let line = "tr a test w";
        let found = c.suggestions(line, line.len());
        assert_eq!(found[0].value, "a test with spaces");
        assert_eq!(found[0].span.start, 3);
        c.tests.lock().unwrap().clear();
        assert!(c.suggestions("tr par", 6).is_empty());
    }

    #[tokio::test]
    async fn completes_functions_paths_and_cursor_in_middle_of_line() {
        let mut c = completer();
        assert!(
            c.suggestions("b parse_loc", 11)
                .iter()
                .any(|s| s.value == "parse_location")
        );
        let line = "b src/parser.rs:parse_loc";
        assert!(
            c.suggestions(line, line.len())
                .iter()
                .any(|s| s.value == "src/parser.rs:parse_location")
        );
        assert!(c.suggestions("b sr", 4).iter().any(|s| s.value == "src/"));
        let line = "td\u{2003}par trailing";
        let pos = line.find(" trailing").unwrap();
        let found = c.suggestions(line, pos);
        assert_eq!(found[0].value, "parser::empty");
        assert_eq!(&line[found[0].span.start..found[0].span.end], "par");
        assert!(c.suggestions(line, 3).is_empty()); // inside a UTF-8 character
    }

    #[tokio::test]
    async fn breakpoint_completion_queries_current_state_and_handles_not_stopped() {
        use ddbg_core::breakpoint::{BreakpointId, SourceLocation};
        use ddbg_core::command::Location;

        let c = completer();
        c.engine
            .execute(Command::Break(Location::Source(SourceLocation::new(
                "src/lib.rs",
                10,
            ))))
            .await
            .unwrap();
        let mut c = tokio::task::spawn_blocking(move || {
            let mut c = c;
            let found = c.suggestions("d ", 2);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].value, "1");
            assert!(
                found[0]
                    .description
                    .as_ref()
                    .unwrap()
                    .ends_with("src/lib.rs:10")
            );
            assert!(c.suggestions("p ", 2).is_empty());
            c
        })
        .await
        .unwrap();
        c.engine
            .execute(Command::DeleteBreakpoint(BreakpointId(1)))
            .await
            .unwrap();
        tokio::task::spawn_blocking(move || assert!(c.suggestions("d ", 2).is_empty()))
            .await
            .unwrap();
    }

    #[test]
    fn local_variables_keep_names_and_type_descriptions() {
        let found = reply_candidates(
            Reply::Locals(vec![ddbg_core::command::ScopeVariables {
                scope: "Locals".into(),
                variables: vec![ddbg_core::variable::Variable {
                    name: "point".into(),
                    value: "{x: 3}".into(),
                    type_name: Some("Point".into()),
                    children: None,
                }],
            }]),
            Span::new(2, 4),
        );
        assert_eq!(found[0].value, "point");
        assert_eq!(found[0].description.as_deref(), Some("Point"));
        assert_eq!(found[0].span, Span::new(2, 4));
    }

    fn values(line: &str) -> Vec<String> {
        suggestions(line, line.len())
            .into_iter()
            .map(|s| s.value)
            .collect()
    }

    #[test]
    fn completes_command_names() {
        assert_eq!(values("br"), ["break", "breakpoints"]);
        assert_eq!(values("con"), ["continue"]);
        assert!(values("").len() == COMMANDS.len());
    }

    #[test]
    fn completes_paths_for_break() {
        let v = values(&format!("b {}/sr", env!("CARGO_MANIFEST_DIR")));
        assert_eq!(v, [format!("{}/src/", env!("CARGO_MANIFEST_DIR"))]);
        assert!(values("print fo").is_empty());
    }
}
