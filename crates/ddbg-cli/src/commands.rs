//! Tab completion for the REPL.

use std::path::Path;

use reedline::{Completer, CompletionResult, Span, Suggestion};

use crate::parser::{COMMANDS, lookup};

/// Completes command names, and file paths for `break` and `run`.
#[derive(Default)]
pub struct DdbgCompleter;

impl Completer for DdbgCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        CompletionResult::fresh(suggestions(line, pos))
    }
}

pub fn suggestions(line: &str, pos: usize) -> Vec<Suggestion> {
    let line = &line[..pos];
    let start = line.rfind(char::is_whitespace).map_or(0, |i| i + 1);
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
        Some("break" | "run") => complete_path(word, span),
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

fn complete_path(word: &str, span: Span) -> Vec<Suggestion> {
    let (dir, prefix) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let read_dir = if dir.is_empty() {
        Path::new(".")
    } else {
        Path::new(dir)
    };
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
