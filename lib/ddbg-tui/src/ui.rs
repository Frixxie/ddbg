//! Layout and drawing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Focus, Status};
use crate::picker::{Picker, PickerItem, ProgramPicker, TestPicker};

const KEYS: &[(&str, &str)] = &[
    ("r", "run / restart"),
    ("e", "pick program to run"),
    ("c, F5", "continue"),
    ("p, Ctrl-C", "pause"),
    ("n, F10", "next (step over)"),
    ("s, F11", "step into"),
    ("f, Shift-F11", "finish (step out)"),
    ("K", "kill"),
    ("b, F9", "toggle breakpoint at cursor"),
    ("u / d", "frame up / down"),
    ("Enter", "select frame (stack pane)"),
    (".", "jump to execution point"),
    ("j/k, arrows", "move / scroll"),
    ("g / G", "top / bottom"),
    ("Tab", "cycle focus: source, stack, log"),
    ("t", "pick a test to debug or run"),
    (":", "command line (REPL syntax)"),
    ("q", "quit"),
];

pub fn draw(f: &mut Frame, app: &mut App) {
    let [title, main, log, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(10),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [source, side] =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(main);
    let [stack, locals, breakpoints] = Layout::vertical([
        Constraint::Percentage(35),
        Constraint::Percentage(40),
        Constraint::Percentage(25),
    ])
    .areas(side);

    draw_title(f, app, title);
    draw_source(f, app, source);
    draw_stack(f, app, stack);
    draw_locals(f, app, locals);
    draw_breakpoints(f, app, breakpoints);
    draw_log(f, app, log);
    draw_bottom(f, app, bottom);
    if let Some(picker) = &app.picker {
        draw_picker(f, app, picker);
    }
    if let Some(picker) = &app.programs {
        draw_programs(f, app, picker);
    }
    if app.show_help {
        draw_help(f);
    }
}

/// Centered rect of at most `w`x`h` cells.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_picker(f: &mut Frame, app: &App, picker: &TestPicker) {
    let title = match &picker.items {
        None => "tests (discovering...)".to_owned(),
        Some(all) => format!("tests {}/{}", picker.matches().len(), all.len()),
    };
    let detail = picker.selected().and_then(|t| {
        let src = t.source.as_ref()?;
        let path = src.strip_prefix(&app.cwd).unwrap_or(src).display();
        Some(match t.line {
            Some(l) => format!("{path}:{l}"),
            None => path.to_string(),
        })
    });
    draw_filter_list(
        f,
        picker,
        &title,
        |t| {
            let mut spans = vec![Span::raw(t.name.clone())];
            if t.display_name != t.name {
                spans.push(Span::styled(
                    format!("  {}", t.display_name),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        },
        detail,
        "no matching tests",
        "Enter debug  ^B debug+break  ^R run  ↑/↓ move  Esc close",
    );
}

fn draw_programs(f: &mut Frame, app: &App, picker: &ProgramPicker) {
    let total = picker.items.as_ref().map_or(0, Vec::len);
    let title = format!("programs {}/{}", picker.matches().len(), total);
    let detail = picker.selected().map(|p| {
        let built = if p.path.exists() { "" } else { "  (not built)" };
        format!("{}{built}", p.path.display())
    });
    draw_filter_list(
        f,
        picker,
        &title,
        |p| {
            let mut spans = vec![Span::raw(p.label.clone())];
            if app.program.as_ref() == Some(&p.path) {
                spans.push(Span::styled("  (current)", Style::new().fg(Color::Yellow)));
            }
            if !p.path.exists() {
                spans.push(Span::styled("  not built", Style::new().fg(Color::Red)));
            }
            Line::from(spans)
        },
        detail,
        "no matching programs",
        "Enter run  ^B run+stop on entry  ↑/↓ move  Esc close",
    );
}

/// Centered popup with a filter line, a list, a detail line and key hints.
fn draw_filter_list<T: PickerItem>(
    f: &mut Frame,
    picker: &Picker<T>,
    title: &str,
    item: impl Fn(&T) -> Line<'static>,
    detail: Option<String>,
    empty_text: &str,
    hint_text: &str,
) {
    let area = f.area();
    let rect = centered(area, area.width * 4 / 5, area.height * 4 / 5);
    let matches = picker.matches();
    let block = block(title, true);
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    let [filter, list, detail_area, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    f.render_widget(
        Line::from(vec![
            Span::styled("> ", Style::new().fg(Color::Cyan).bold()),
            Span::raw(picker.filter.as_str()),
        ]),
        filter,
    );
    f.set_cursor_position((
        filter.x + 2 + picker.filter.chars().count() as u16,
        filter.y,
    ));

    let items: Vec<ListItem> = matches.iter().map(|t| ListItem::new(item(t))).collect();
    let mut state = ListState::default();
    if !matches.is_empty() {
        state.select(Some(picker.cursor));
    }
    let empty = picker.items.is_some() && matches.is_empty();
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().reversed()),
        list,
        &mut state,
    );
    if empty {
        f.render_widget(
            Line::styled(empty_text.to_owned(), Style::new().fg(Color::DarkGray)),
            list,
        );
    }
    if let Some(detail) = detail {
        f.render_widget(
            Line::styled(detail, Style::new().fg(Color::DarkGray)),
            detail_area,
        );
    }
    f.render_widget(
        Line::styled(hint_text.to_owned(), Style::new().fg(Color::DarkGray)),
        hint,
    );
}

fn block(title: &str, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::bordered()
        .border_style(style)
        .title(format!(" {title} "))
}

fn draw_title(f: &mut Frame, app: &App, area: Rect) {
    let (text, color) = match app.status {
        Status::Idle => ("not started".to_owned(), Color::DarkGray),
        Status::Running => ("running".to_owned(), Color::Green),
        Status::Stopped => ("stopped".to_owned(), Color::Yellow),
        Status::Exited(code) => (format!("exited ({code})"), Color::Blue),
        Status::Terminated => ("terminated".to_owned(), Color::Blue),
    };
    let mut spans = vec![
        Span::styled(" ddbg ", Style::new().reversed().bold()),
        Span::raw(" "),
        Span::styled(text, Style::new().fg(color).bold()),
    ];
    if let Some(program) = &app.program {
        let name = program.strip_prefix(&app.cwd).unwrap_or(program).display();
        spans.push(Span::styled(
            format!("  {name}"),
            Style::new().fg(Color::Cyan),
        ));
    } else if !app.candidates.is_empty() {
        spans.push(Span::styled(
            "  no program (e to pick)",
            Style::new().fg(Color::DarkGray),
        ));
    }
    if app.busy > 0 {
        spans.push(Span::styled(
            "  working...",
            Style::new().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Line::from(spans), area);
}

fn draw_source(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Source;
    let Some(source) = &app.source else {
        let p = Paragraph::new("No source. Press r to run, ? for help.")
            .block(block("source", focused))
            .fg(Color::DarkGray);
        f.render_widget(p, area);
        return;
    };
    let title = source
        .path
        .strip_prefix(&app.cwd)
        .unwrap_or(&source.path)
        .display()
        .to_string();
    let block = block(&title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(lines) = &source.lines else {
        f.render_widget(Paragraph::new("(source unavailable)").fg(Color::Red), inner);
        return;
    };
    let height = inner.height as usize;
    let cursor = app.cursor as usize;
    // Keep the cursor roughly centered.
    let top = cursor
        .saturating_sub(height / 2 + 1)
        .min(lines.len().saturating_sub(height));
    let width = lines.len().to_string().len();

    let rows: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(i, text)| {
            let n = i as u32 + 1;
            let bp = app.breakpoint_at(n).map(|bp| bp.verified);
            let gutter = match bp {
                Some(true) => Span::styled("●", Style::new().fg(Color::Red)),
                Some(false) => Span::styled("○", Style::new().fg(Color::Red)),
                None => Span::raw(" "),
            };
            let is_exec = app.exec_line == Some(n);
            let arrow = if is_exec {
                Span::styled("▶", Style::new().fg(Color::Yellow).bold())
            } else {
                Span::raw(" ")
            };
            let mut line = Line::from(vec![
                gutter,
                Span::styled(format!("{n:>width$} "), Style::new().fg(Color::DarkGray)),
                arrow,
                Span::raw(" "),
                Span::raw(text.as_str()),
            ]);
            if is_exec {
                line = line.style(Style::new().bg(Color::Rgb(60, 60, 0)));
            }
            if focused && n == app.cursor {
                line = line.patch_style(Style::new().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    f.render_widget(Paragraph::new(rows), inner);
}

fn draw_stack(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Stack;
    let items: Vec<ListItem> = app
        .stack
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let marker = if app.selected_frame == Some(i) {
                "▶"
            } else {
                " "
            };
            let loc = match (&frame.path, &frame.source_name) {
                (Some(p), _) => p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                (None, Some(n)) => n.clone(),
                (None, None) => String::new(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}#{i:<2} "), Style::new().fg(Color::Yellow)),
                Span::raw(frame.name.clone()),
                Span::styled(
                    format!("  {loc}:{}", frame.line),
                    Style::new().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    if focused && !app.stack.is_empty() {
        state.select(Some(app.stack_cursor));
    }
    let list = List::new(items)
        .block(block("stack", focused))
        .highlight_style(Style::new().reversed());
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_locals(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let multiple = app.locals.len() > 1;
    for scope in &app.locals {
        if multiple {
            lines.push(Line::styled(scope.scope.clone(), Style::new().bold()));
        }
        for v in &scope.variables {
            let mut spans = vec![
                Span::raw(if multiple { "  " } else { "" }),
                Span::styled(v.name.clone(), Style::new().fg(Color::Cyan)),
            ];
            if let Some(t) = &v.type_name {
                spans.push(Span::styled(
                    format!(": {t}"),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::raw(format!(" = {}", v.value)));
            lines.push(Line::from(spans));
        }
    }
    let p = Paragraph::new(lines)
        .block(block("locals", false))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_breakpoints(f: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .breakpoints
        .iter()
        .map(|bp| {
            let (mark, color) = if bp.verified {
                ("●", Color::Red)
            } else {
                ("○", Color::DarkGray)
            };
            let loc = match &bp.resolved {
                Some(r) => r.to_string(),
                None => bp.requested.to_string(),
            };
            let loc = loc
                .strip_prefix(&format!("{}/", app.cwd.display()))
                .map(str::to_owned)
                .unwrap_or(loc);
            Line::from(vec![
                Span::styled(format!("{mark} "), Style::new().fg(color)),
                Span::raw(format!("{} {loc}", bp.id)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(block("breakpoints", false)),
        area,
    );
}

fn draw_log(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Log;
    let block = block("output", focused);
    let inner = block.inner(area);
    let height = inner.height as usize;
    let end = app.log.len().saturating_sub(app.log_scroll);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = app.log[start..end]
        .iter()
        .map(|l| {
            if l.starts_with("error:") {
                Line::styled(l.as_str(), Style::new().fg(Color::Red))
            } else if l.starts_with("ddbg> ") {
                Line::styled(l.as_str(), Style::new().fg(Color::DarkGray))
            } else {
                Line::raw(l.as_str())
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_bottom(f: &mut Frame, app: &App, area: Rect) {
    match &app.input {
        Some(input) => {
            let line = Line::from(vec![
                Span::styled(":", Style::new().bold()),
                Span::raw(input),
            ]);
            f.render_widget(line, area);
            f.set_cursor_position((area.x + 1 + input.chars().count() as u16, area.y));
        }
        None => {
            let hint = "r run  e program  c cont  n next  s step  f finish  b break  t tests  : cmd  ? help  q quit";
            f.render_widget(Line::styled(hint, Style::new().fg(Color::DarkGray)), area);
        }
    }
}

fn draw_help(f: &mut Frame) {
    let area = f.area();
    let rect = centered(area, 52, KEYS.len() as u16 + 4);
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{k:>14}  "), Style::new().fg(Color::Yellow)),
                Span::raw(*d),
            ])
        })
        .chain([
            Line::raw(""),
            Line::styled("  any key to close", Style::new().fg(Color::DarkGray)),
        ])
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block("keys", true)), rect);
}
