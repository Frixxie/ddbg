//! Convert ANSI SGR escape sequences into ratatui styled lines.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Parse `text` into a line, applying SGR (`ESC [ ... m`) sequences on top of
/// `base`. Other escape sequences are dropped.
pub fn line(text: &str, base: Style) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = base;
    let mut buf = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            buf.push(c);
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut fin = None;
        for c in chars.by_ref() {
            if ('\x40'..='\x7e').contains(&c) {
                fin = Some(c);
                break;
            }
            params.push(c);
        }
        if fin != Some('m') {
            continue;
        }
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut buf), style));
        }
        style = apply_sgr(style, base, &params);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    Line::from(spans)
}

fn apply_sgr(mut style: Style, base: Style, params: &str) -> Style {
    let codes: Vec<u16> = params
        .split([';', ':'])
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let mut it = codes.iter().copied();
    while let Some(code) = it.next() {
        style = match code {
            0 => base,
            1 => style.add_modifier(Modifier::BOLD),
            2 => style.add_modifier(Modifier::DIM),
            3 => style.add_modifier(Modifier::ITALIC),
            4 => style.add_modifier(Modifier::UNDERLINED),
            7 => style.add_modifier(Modifier::REVERSED),
            9 => style.add_modifier(Modifier::CROSSED_OUT),
            22 => style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style.remove_modifier(Modifier::ITALIC),
            24 => style.remove_modifier(Modifier::UNDERLINED),
            27 => style.remove_modifier(Modifier::REVERSED),
            29 => style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg(basic(code - 30)),
            39 => style.fg(base.fg.unwrap_or(Color::Reset)),
            40..=47 => style.bg(basic(code - 40)),
            49 => style.bg(base.bg.unwrap_or(Color::Reset)),
            90..=97 => style.fg(basic(code - 90 + 8)),
            100..=107 => style.bg(basic(code - 100 + 8)),
            38 | 48 => {
                let color = match it.next() {
                    Some(5) => it.next().map(|n| Color::Indexed(n as u8)),
                    Some(2) => match (it.next(), it.next(), it.next()) {
                        (Some(r), Some(g), Some(b)) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                        _ => None,
                    },
                    _ => None,
                };
                match color {
                    Some(c) if code == 38 => style.fg(c),
                    Some(c) => style.bg(c),
                    None => style,
                }
            }
            _ => style,
        };
    }
    style
}

fn basic(n: u16) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn sgr_sequences_preserve_visible_text(parts: Vec<String>, codes: Vec<u8>) -> bool {
        let mut input = String::new();
        let mut expected = String::new();
        for (i, part) in parts.iter().enumerate() {
            // Literal ESC belongs to the parser's control syntax, not text.
            let part = part.replace('\x1b', "");
            let code = codes.get(i).copied().unwrap_or(0);
            input.push_str(&format!("\x1b[{code}m{part}"));
            expected.push_str(&part);
        }
        let actual: String = line(&input, Style::new())
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        actual == expected
    }

    #[quickcheck]
    fn sgr_reset_restores_base_style(codes: Vec<u16>, foreground: u8, background: u8) -> bool {
        let base = Style::new()
            .fg(Color::Indexed(foreground))
            .bg(Color::Indexed(background))
            .add_modifier(Modifier::ITALIC);
        let codes = codes
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(";");
        let parsed = line(&format!("\x1b[{codes}mx\x1b[0my"), base);
        parsed
            .spans
            .last()
            .is_some_and(|span| span.content == "y" && span.style == base)
    }

    #[quickcheck]
    fn extended_colors_decode(red: u8, green: u8, blue: u8, index: u8) -> bool {
        let parsed = line(
            &format!("\x1b[38;2;{red};{green};{blue};48;5;{index}mx"),
            Style::new(),
        );
        parsed.spans.len() == 1
            && parsed.spans[0].style.fg == Some(Color::Rgb(red, green, blue))
            && parsed.spans[0].style.bg == Some(Color::Indexed(index))
    }

    #[quickcheck]
    fn malformed_escape_sequences_never_panic(text: String) -> bool {
        for prefix in [
            "",
            "\x1b",
            "\x1b[",
            "\x1b[38;2;",
            "\x1b[999999999999999999;",
        ] {
            let _ = line(&format!("{prefix}{text}"), Style::new());
        }
        true
    }

    #[test]
    fn parses_sgr() {
        let l = line("a\x1b[1;31mb\x1b[0mc\x1b[38;2;1;2;3md", Style::new());
        assert_eq!(l.spans.len(), 4);
        assert_eq!(l.spans[1].content, "b");
        assert_eq!(l.spans[1].style.fg, Some(Color::Red));
        assert_eq!(l.spans[2].style, Style::new());
        assert_eq!(l.spans[3].style.fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn drops_other_escapes() {
        let l = line("x\x1b[2Ky", Style::new());
        assert_eq!(l.spans.len(), 1);
        assert_eq!(l.spans[0].content, "xy");
    }
}
