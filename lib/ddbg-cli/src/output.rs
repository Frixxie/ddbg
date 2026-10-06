//! Plain-text terminal output for nonterminal frontends. Raw output is retained
//! by the driver; this filter is incremental because DAP chunks can split ANSI
//! sequences in the middle.

#[derive(Debug, Default)]
enum State {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

#[derive(Debug, Default)]
pub struct PlainOutput {
    state: State,
}

impl PlainOutput {
    pub fn push(&mut self, text: &str) -> String {
        let mut plain = String::new();
        for c in text.chars() {
            match self.state {
                State::Text if c == '\u{1b}' => self.state = State::Escape,
                State::Text => plain.push(c),
                State::Escape => {
                    self.state = match c {
                        '[' => State::Csi,
                        ']' => State::Osc,
                        // Escape sequences can have intermediate bytes (e.g. ESC ( B).
                        ' '..='/' => State::Escape,
                        _ => State::Text,
                    }
                }
                State::Csi if ('@'..='~').contains(&c) => self.state = State::Text,
                State::Csi => {}
                State::Osc if c == '\u{7}' => self.state = State::Text,
                State::Osc if c == '\u{1b}' => self.state = State::OscEscape,
                State::Osc => {}
                State::OscEscape => {
                    self.state = match c {
                        '\\' | '\u{7}' => State::Text,
                        '\u{1b}' => State::OscEscape,
                        _ => State::Osc,
                    }
                }
            }
        }
        plain
    }
}

pub fn plain_text(text: &str) -> String {
    PlainOutput::default().push(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colors_cursor_commands_and_hyperlinks_without_losing_text() {
        assert_eq!(plain_text("\x1b[32m✓ passed\x1b[0m\n\x1b[2K"), "✓ passed\n");
        assert_eq!(
            plain_text("\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x07"),
            "link"
        );
        assert_eq!(plain_text("\x1b(Bplain"), "plain");
    }

    #[test]
    fn escape_sequences_can_span_output_reads() {
        let mut filter = PlainOutput::default();
        assert_eq!(filter.push("hello\x1b[3"), "hello");
        assert_eq!(
            filter.push("2m world\x1b]8;;https://example.org\x1b"),
            " world"
        );
        assert_eq!(filter.push("\\link\x1b]8;;\x07\n"), "link\n");
    }
}
