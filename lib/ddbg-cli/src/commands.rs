//! Tab completion for the REPL.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ddbg_core::{EngineHandle, Reply, command::Command};
use reedline::{Completer, CompletionResult};
pub use reedline::{Span, Suggestion};

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
        if let Some((offset, query)) = expression_query(line, pos) {
            let result = self.runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(500), self.engine.execute(query)).await
            });
            return match result {
                Ok(Ok(reply)) => expression_suggestions(line, offset, reply),
                _ => Vec::new(),
            };
        }
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
                for name in self.tests.lock().unwrap().iter() {
                    candidates.push(candidate(name.clone(), "Test".into(), span));
                }
            }
            "delete" | "condition" | "thread" | "frame" | "unwatch" => {
                let query = match command {
                    "delete" | "condition" => Command::Breakpoints,
                    "thread" => Command::Threads,
                    "unwatch" => Command::Watches,
                    _ => Command::Backtrace,
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

/// For `print`/`eval`/`set` lines, the byte offset where the expression
/// starts and the engine query that completes it at `pos`.
pub fn expression_query(line: &str, pos: usize) -> Option<(usize, Command)> {
    let before = line.get(..pos)?;
    let trimmed = before.trim_start();
    let (word, rest) = trimmed.split_once(char::is_whitespace)?;
    let rest = match lookup(word)?.name {
        "print" | "eval" | "watch" => rest.trim_start(),
        "set" => crate::parser::set_body(rest.trim_start()),
        "break" => crate::parser::split_break_condition(rest.trim_start()).1?,
        "condition" => rest
            .trim_start()
            .split_once(char::is_whitespace)?
            .1
            .trim_start(),
        _ => return None,
    };
    let offset = pos - rest.len();
    let text = line[offset..].to_owned();
    Some((
        offset,
        Command::Complete {
            column: rest.chars().count(),
            text,
        },
    ))
}

/// Turn a [`Reply::Completions`] for an expression starting at byte
/// `offset` of `line` into suggestions spanning the whole line.
pub fn expression_suggestions(line: &str, offset: usize, reply: Reply) -> Vec<Suggestion> {
    let Reply::Completions(items) = reply else {
        return Vec::new();
    };
    let expr = &line[offset..];
    let byte = |chars: usize| {
        offset
            + expr
                .char_indices()
                .nth(chars)
                .map_or(expr.len(), |(i, _)| i)
    };
    let mut out: Vec<Suggestion> = items
        .into_iter()
        .map(|c| Suggestion {
            value: c.text,
            description: c.kind.filter(|k| !k.is_empty()),
            span: Span::new(byte(c.start), byte(c.start + c.length)),
            append_whitespace: false,
            ..Default::default()
        })
        .collect();
    out.dedup_by(|a, b| a.value == b.value && a.span == b.span);
    out
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
        Reply::Watches(watches) => watches
            .into_iter()
            .map(|w| candidate(w.id.to_string(), w.expression, span))
            .collect(),
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
        assert!(c.suggestions("td 2", 4).is_empty());
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
        c.engine
            .execute(Command::Watch("x + 1".into()))
            .await
            .unwrap();
        let mut c = tokio::task::spawn_blocking(move || {
            let mut c = c;
            let found = c.suggestions("d ", 2);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].value, "1");
            assert_eq!(c.suggestions("condition ", 10)[0].value, "1");
            assert_eq!(c.suggestions("unwatch ", 8)[0].value, "1");
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
    fn expression_queries_skip_command_and_set_var() {
        let q = |l: &str| expression_query(l, l.len());
        assert_eq!(
            q("p po"),
            Some((
                2,
                Command::Complete {
                    text: "po".into(),
                    column: 2
                }
            ))
        );
        let Some((off, Command::Complete { text, column })) = q("set var p.x = y") else {
            panic!()
        };
        assert_eq!((off, text.as_str(), column), (8, "p.x = y", 7));
        assert!(q("b foo").is_none());
        assert!(q("print").is_none());
        assert_eq!(
            q("watch po"),
            Some((
                6,
                Command::Complete {
                    text: "po".into(),
                    column: 2
                }
            ))
        );
        for line in ["b foo if po", "condition 1 po", "b foo if po "] {
            let Some((offset, Command::Complete { text, column })) = q(line) else {
                panic!("{line}")
            };
            assert_eq!(&line[offset..], text);
            assert_eq!(column, text.chars().count());
            assert!(text.starts_with("po"));
        }
    }

    #[test]
    fn expression_completions_map_to_line_spans() {
        use ddbg_core::command::Completion;
        let line = "p é.po";
        let found = expression_suggestions(
            line,
            2,
            Reply::Completions(vec![Completion {
                label: "point".into(),
                text: "point".into(),
                kind: Some("Point".into()),
                start: 2,
                length: 2,
            }]),
        );
        assert_eq!(found[0].value, "point");
        assert_eq!(found[0].description.as_deref(), Some("Point"));
        assert_eq!(&line[found[0].span.start..found[0].span.end], "po");
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
        assert_eq!(values("con"), ["continue", "condition"]);
        assert_eq!(values("wat"), ["watch", "watches"]);
        assert!(values("").len() == COMMANDS.len());
    }

    #[test]
    fn completes_paths_for_break() {
        let v = values(&format!("b {}/sr", env!("CARGO_MANIFEST_DIR")));
        assert_eq!(v, [format!("{}/src/", env!("CARGO_MANIFEST_DIR"))]);
        assert!(values("print fo").is_empty());
    }
}
