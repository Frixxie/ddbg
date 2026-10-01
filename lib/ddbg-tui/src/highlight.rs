//! Syntax highlighting for the source pane, backed by syntect.

use std::path::Path;
use std::sync::LazyLock;

use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// A line of source split into styled segments.
pub type StyledLine = Vec<(Style, String)>;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let mut themes = ThemeSet::load_defaults();
    themes
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});

/// Highlight `lines` based on the extension of `path`. Only foreground
/// colors and font styles are set, so the source pane's own line
/// backgrounds (exec line, cursor) stay visible.
pub fn highlight(path: &Path, lines: &[String]) -> Vec<StyledLine> {
    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| SYNTAXES.find_syntax_by_extension(e))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, &THEME);
    lines
        .iter()
        .map(|line| {
            // The syntax set expects lines with trailing newlines.
            let with_nl = format!("{line}\n");
            match h.highlight_line(&with_nl, &SYNTAXES) {
                Ok(regions) => regions
                    .into_iter()
                    .filter_map(|(style, text)| {
                        let text = text.trim_end_matches('\n');
                        (!text.is_empty()).then(|| (convert(style), text.to_string()))
                    })
                    .collect(),
                Err(_) => vec![(Style::new(), line.clone())],
            }
        })
        .collect()
}

fn convert(s: syntect::highlighting::Style) -> Style {
    let fg = s.foreground;
    let mut style = Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_target_languages() {
        for (file, src) in [
            ("a.rs", "fn main() { let x = 1; }"),
            ("a.c", "int main(void) { return 0; }"),
            ("a.py", "def main():\n    return 1"),
            ("a.cs", "public class A { int x = 1; }"),
        ] {
            let lines: Vec<String> = src.lines().map(String::from).collect();
            let styled = highlight(Path::new(file), &lines);
            assert_eq!(styled.len(), lines.len());
            let colors: std::collections::HashSet<_> =
                styled.iter().flatten().map(|(s, _)| s.fg).collect();
            assert!(colors.len() > 1, "{file} was not highlighted");
            let text: String = styled[0].iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(text, lines[0]);
        }
    }
}
