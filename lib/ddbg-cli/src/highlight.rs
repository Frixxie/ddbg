//! ANSI syntax highlighting for source listings, backed by syntect.

use std::path::Path;
use std::sync::LazyLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let mut themes = ThemeSet::load_defaults();
    themes
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});

/// Highlight a whole file (so multi-line constructs are tracked correctly),
/// returning one ANSI-escaped string per line. Only foreground colors are
/// emitted, and each line ends with a reset.
pub fn highlight(path: &Path, lines: &[String]) -> Vec<String> {
    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| SYNTAXES.find_syntax_by_extension(e))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, &THEME);
    lines
        .iter()
        .map(|line| {
            let with_nl = format!("{}\n", line.trim_end());
            match h.highlight_line(&with_nl, &SYNTAXES) {
                Ok(regions) => {
                    let s = as_24_bit_terminal_escaped(&regions, false);
                    format!("{}\x1b[0m", s.trim_end_matches('\n'))
                }
                Err(_) => line.trim_end().to_owned(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_ansi_colors() {
        let lines = vec!["fn main() { let x = 1; }".to_owned()];
        let out = highlight(Path::new("a.rs"), &lines);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("\x1b[38;2;"));
        assert!(out[0].ends_with("\x1b[0m"));
    }
}
