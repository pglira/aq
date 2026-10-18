//! Drawing of the TUI.

use std::time::Duration;

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use tui_textarea::TextArea;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{App, Filter, Focus, Overlay, Setting, View, clamp_list_width};
use super::form::{Field, Form};
use crate::chat::{AQ, Entry, HUMAN, Kind};
use crate::markdown;
use crate::queue::Loaded;
use crate::task::Status;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const HUMAN_COLOR: Color = Color::Green;
const AGENT_COLOR: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if app.q.is_none() {
        draw_no_queue(f, app, area);
        return;
    }
    let [main, footer] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let list_w = match app.config.list_width {
        Some(w) => clamp_list_width(w, main.width),
        None => (main.width * 35 / 100).clamp(26, 48).min(main.width),
    };
    app.main_area = main;
    app.list_drawn = list_w;
    let [left, right] =
        Layout::horizontal([Constraint::Length(list_w), Constraint::Min(10)]).areas(main);
    app.page = right.height.saturating_sub(4);
    draw_list(f, app, left);
    draw_right(f, app, right);
    draw_footer(f, app, footer);
    draw_overlay(f, app, area);
}

fn border(focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }))
}

fn draw_no_queue(f: &mut Frame, app: &App, area: Rect) {
    let text = vec![
        Line::from("No aq queue found.".bold()),
        Line::default(),
        Line::from(format!("Press i to make .aq/ in {}", app.project.display())),
        Line::from("Press q to quit."),
    ];
    let r = centered(area, 60, 8);
    f.render_widget(
        Paragraph::new(text)
            .block(border(true).title(" aq "))
            .wrap(Wrap { trim: false }),
        r,
    );
    if let Some((msg, true, _)) = &app.message {
        let r2 = Rect {
            y: r.y + r.height,
            height: 1,
            ..r
        };
        f.render_widget(Paragraph::new(msg.clone().red()), r2);
    }
}

/// The symbol and the color of a task in the list. `waits` is true if the task
/// waits for a dependency.
fn status_symbol(s: Status, waits: bool) -> (&'static str, Color) {
    if waits {
        return ("⧗", DIM);
    }
    let sym = match s {
        Status::Todo => "○",
        Status::InProgress => "◔",
        Status::Blocked => "?",
        Status::Review => "◐",
        Status::Done => "●",
        Status::Failed => "✗",
    };
    (sym, status_color(s))
}

fn status_color(s: Status) -> Color {
    match s {
        Status::Todo => Color::White,
        Status::InProgress => Color::Blue,
        Status::Blocked => Color::Yellow,
        Status::Review => Color::Magenta,
        Status::Done => Color::Green,
        Status::Failed => Color::Red,
    }
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let title = format!(" Tasks [{}] {} ", app.filter.name(), app.visible.len());
    let block = border(app.focus == Focus::List).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let height = inner.height as usize;
    let top = app.sel.saturating_sub(height.saturating_sub(1));
    let mut lines = Vec::new();
    for (row, idx) in app.visible.iter().enumerate().skip(top).take(height) {
        let l = &app.tasks[*idx];
        let waits = app.waits(l);
        let (sym, color) = status_symbol(l.task.status(), waits);
        let status = if waits {
            "todo (dep)"
        } else {
            l.task.status().as_str()
        };
        let mail = if app.unread(l) > 0 { " ✉" } else { "  " };
        let id = format!("{:>3} ", l.task.id());
        let status = format!(" {status}");
        // The symbol and its space use two columns; the mark uses two columns.
        let title_w = w.saturating_sub(2 + id.chars().count() + status.chars().count() + 2);
        let title = truncate(&l.task.display_title(), title_w);
        let pad = title_w.saturating_sub(title.chars().count());
        let selected = row == app.sel;
        let mut base = Style::default();
        if selected {
            base = base.bg(if app.focus == Focus::List {
                Color::Rgb(45, 55, 72)
            } else {
                Color::Rgb(38, 38, 38)
            });
        }
        let prio = match l.task.meta.priority {
            1 => base.add_modifier(Modifier::BOLD),
            3 => base.fg(Color::Gray),
            _ => base,
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{sym} "), base.fg(color)),
            Span::styled(id, base.fg(DIM)),
            Span::styled(
                title,
                if l.task.has_title() {
                    prio
                } else {
                    prio.fg(DIM)
                },
            ),
            Span::styled(" ".repeat(pad), base),
            Span::styled(status, base.fg(status_color(l.task.status()))),
            Span::styled(mail, base.fg(Color::Yellow)),
        ]));
    }
    if app.visible.is_empty() {
        lines.push(Line::styled(
            "no tasks; press c to add one",
            Style::default().fg(DIM),
        ));
    }
    if !app.bad.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("{} invalid task file(s)", app.bad.len()),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn truncate(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(w - 1).collect();
    out.push('…');
    out
}

/// The tabs of the task view as a border title, for example ` 1 Chat │ 2 Details `.
/// The active tab is bold in the accent color; the other tabs are dim.
fn view_tabs(active: View, focused: bool) -> Line<'static> {
    let sep = Style::default().fg(if focused { ACCENT } else { DIM });
    let mut spans = vec![Span::raw(" ")];
    for (i, v) in View::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", sep));
        }
        let style = if *v == active {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        spans.push(Span::styled(format!("{} {}", i + 1, v.title()), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn draw_right(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::View;
    let block = border(focused).title(view_tabs(app.view, focused));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    let Some(l) = app.selected().cloned() else {
        f.render_widget(Paragraph::new("no task selected").fg(DIM), header);
        return;
    };
    let m = &l.task.meta;
    let mut h = vec![
        Span::styled(format!("#{} ", m.id), Style::default().fg(DIM)),
        if l.task.has_title() {
            Span::styled(
                m.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(l.task.display_title(), Style::default().fg(DIM))
        },
        Span::raw("  "),
        Span::styled(
            m.status.as_str(),
            Style::default().fg(status_color(m.status)),
        ),
        Span::styled(format!("  p{}", m.priority), Style::default().fg(DIM)),
    ];
    if let Some(a) = &m.assignee {
        h.push(Span::styled(
            format!("  {a}"),
            Style::default().fg(AGENT_COLOR),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(h)), header);
    let body = Rect {
        y: body.y + 1,
        height: body.height.saturating_sub(1),
        ..body
    };
    match app.view {
        View::Chat => draw_chat(f, app, &l, body),
        View::Details => draw_details(f, app, &l, body),
    }
}

fn author_color(from: &str) -> Color {
    match from {
        HUMAN => HUMAN_COLOR,
        AQ => DIM,
        _ => AGENT_COLOR,
    }
}

fn time_label(e: &Entry) -> String {
    let local = e.ts.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%m-%d %H:%M").to_string()
    }
}

/// A line with `left` at the start and `right` at the end of `width` columns.
fn spread(mut left: Vec<Span<'static>>, right: Span<'static>, width: usize) -> Line<'static> {
    let used: usize = left.iter().map(|s| s.width()).sum::<usize>() + right.width();
    left.push(Span::raw(" ".repeat(width.saturating_sub(used).max(1))));
    left.push(right);
    Line::from(left)
}

/// Wraps `lines` at word boundaries to `width` columns and puts `prefix` in front
/// of each wrapped row.
fn wrap_prefixed(
    lines: Vec<Line<'static>>,
    prefix: &Span<'static>,
    width: usize,
) -> Vec<Line<'static>> {
    let avail = width.saturating_sub(prefix.width()).max(1);
    let mut out = Vec::new();
    for line in lines {
        let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
        let mut used = 0;
        for span in line.spans {
            for word in span.content.split_inclusive(' ') {
                let fit = word.trim_end().width();
                if used > 0 && used + fit > avail {
                    rows.push(Vec::new());
                    used = 0;
                    if word.trim().is_empty() {
                        continue;
                    }
                }
                if used + fit <= avail {
                    rows.last_mut()
                        .unwrap()
                        .push(Span::styled(word.to_string(), span.style));
                    used += word.width();
                    continue;
                }
                // A word longer than a row is split at character boundaries.
                let mut piece = String::new();
                for c in word.chars() {
                    let cw = c.width().unwrap_or(0);
                    if used + cw > avail && used > 0 {
                        rows.last_mut()
                            .unwrap()
                            .push(Span::styled(std::mem::take(&mut piece), span.style));
                        rows.push(Vec::new());
                        used = 0;
                    }
                    piece.push(c);
                    used += cw;
                }
                rows.last_mut()
                    .unwrap()
                    .push(Span::styled(piece, span.style));
            }
        }
        for mut row in rows {
            row.insert(0, prefix.clone());
            out.push(Line::from(row).style(line.style));
        }
    }
    out
}

/// Splits the text of an event into its verb and an optional body, as in
/// `submitted: <summary>`.
fn split_event(text: &str) -> (&str, Option<&str>) {
    match text.split_once(':') {
        Some((verb, body)) if !verb.contains(char::is_whitespace) && !body.trim().is_empty() => {
            (verb, Some(body.trim()))
        }
        _ => (text, None),
    }
}

/// The lines of one chat entry, `width` columns wide. Messages show in a box with
/// the author and the time in the top border. Events without a body use one dim line.
fn entry_lines(e: &Entry, selected: bool, width: usize) -> Vec<Line<'static>> {
    let author = if e.from == HUMAN {
        "you".to_string()
    } else {
        e.from.clone()
    };
    let time = Span::styled(time_label(e), Style::default().fg(DIM));
    let (verb, body) = match e.kind {
        Kind::Event => split_event(&e.text),
        _ => ("", Some(e.text.as_str())),
    };
    let Some(body) = body else {
        let (mark, mark_style) = if selected {
            ("▶ ", Style::default().fg(ACCENT))
        } else {
            ("· ", Style::default().fg(DIM))
        };
        let text = Span::styled(
            format!("{author} {verb}"),
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        );
        return vec![spread(
            vec![Span::styled(mark, mark_style), text],
            time,
            width,
        )];
    };
    let (tag, tag_color) = match e.kind {
        Kind::Question => ("question", Color::Yellow),
        Kind::Review => ("review", Color::Magenta),
        Kind::Log => ("log", DIM),
        Kind::Event if verb == "failed" => (verb, Color::Red),
        Kind::Event => (verb, Color::Green),
        Kind::Message => ("", DIM),
    };
    // The box has the color of the author. The selected box uses heavy lines.
    let mut frame = Style::default().fg(author_color(&e.from));
    let [tl, tr, bl, br, h, v] = if selected {
        frame = frame.add_modifier(Modifier::BOLD);
        ["┏", "┓", "┗", "┛", "━", "┃"]
    } else {
        ["┌", "┐", "└", "┘", "─", "│"]
    };
    let border = |s: String| Span::styled(s, frame);
    let mut head = vec![
        border(format!("{tl}{h} ")),
        Span::styled(
            author,
            Style::default()
                .fg(author_color(&e.from))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !tag.is_empty() {
        head.push(Span::styled(
            format!("  {tag}"),
            Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
        ));
    }
    head.push(Span::raw(" "));
    let time = format!(" {} {h}{tr}", time_label(e));
    let used: usize = head.iter().map(|s| s.width()).sum::<usize>() + time.width();
    head.push(border(h.repeat(width.saturating_sub(used))));
    head.push(border(time));
    let mut lines = vec![Line::from(head)];
    let base = if e.kind == Kind::Log {
        Style::default().fg(Color::Gray)
    } else {
        Style::default()
    };
    let left = border(format!("{v} "));
    let text = markdown::render(body, base).lines;
    for line in wrap_prefixed(text, &left, width.saturating_sub(2)) {
        // The line style goes to the text spans only, not to the border.
        let mut row: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, line.style.patch(s.style)))
            .collect();
        row[0].style = frame;
        if let [_, .., last] = row.as_mut_slice() {
            last.content = last.content.trim_end().to_string().into();
        }
        let used: usize = row.iter().map(|s| s.width()).sum();
        row.push(Span::raw(" ".repeat(width.saturating_sub(used + 2))));
        row.push(border(format!(" {v}")));
        lines.push(Line::from(row));
    }
    lines.push(Line::from(border(format!(
        "{bl}{}{br}",
        h.repeat(width.saturating_sub(2))
    ))));
    lines
}

/// True if the entry draws as one compact event line.
fn is_compact(e: &Entry) -> bool {
    e.kind == Kind::Event && split_event(&e.text).1.is_none()
}

fn draw_chat(f: &mut Frame, app: &mut App, l: &Loaded, area: Rect) {
    let input_lines = app.input.lines().len().clamp(1, 6) as u16;
    let [chat, input] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(input_lines + 2)]).areas(area);
    let chat = Rect {
        x: chat.x + 1,
        width: chat.width.saturating_sub(2),
        ..chat
    };
    let width = chat.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sel_range = None;
    let n = l.chat.len();
    for (i, e) in l.chat.iter().enumerate() {
        let selected = app.focus == Focus::View && app.chat_sel.unwrap_or(n.saturating_sub(1)) == i;
        // A blank line separates entries, except two compact events in a row.
        if i > 0 && !(is_compact(e) && is_compact(&l.chat[i - 1])) {
            lines.push(Line::default());
        }
        let start = lines.len();
        lines.extend(entry_lines(e, selected, width));
        if selected {
            sel_range = Some((start, lines.len()));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled("no messages", Style::default().fg(DIM)));
    }
    let total = lines.len();
    let height = chat.height as usize;
    let max_scroll = total.saturating_sub(height);
    let scroll = match (app.chat_sel, sel_range) {
        (Some(_), Some((s, e))) => e.saturating_sub(height).min(s).min(max_scroll),
        _ => max_scroll,
    };
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), chat);

    let focused = app.focus == Focus::Input;
    let hint = if app.reject.is_some() {
        " review comment (Enter rejects, Esc cancels) "
    } else if l.task.status() == Status::Blocked {
        " answer (Enter sends) "
    } else {
        " message (Enter sends, Alt+Enter new line) "
    };
    let block = border(focused).title(if focused { hint } else { " i: write " });
    let inner = block.inner(input);
    f.render_widget(block, input);
    style_area(&mut app.input, focused);
    f.render_widget(&app.input, inner);
}

fn style_area(ta: &mut TextArea<'_>, focused: bool) {
    ta.set_cursor_style(if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    });
    ta.set_selection_style(Style::default().bg(Color::Rgb(60, 80, 110)));
}

fn field_line(name: &str, value: Span<'static>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<10}"), Style::default().fg(DIM)),
        value,
    ])
}

fn draw_details(f: &mut Frame, app: &App, l: &Loaded, area: Rect) {
    let m = &l.task.meta;
    let mut lines = vec![
        field_line(
            "status",
            Span::styled(
                m.status.as_str(),
                Style::default().fg(status_color(m.status)),
            ),
        ),
        field_line(
            "priority",
            Span::raw(match m.priority {
                1 => "1 (high)",
                3 => "3 (low)",
                _ => "2 (normal)",
            }),
        ),
        field_line(
            "assignee",
            Span::raw(m.assignee.clone().unwrap_or_else(|| "-".into())),
        ),
    ];
    if m.depends_on.is_empty() {
        lines.push(field_line("depends", Span::raw("-")));
    } else {
        for (i, d) in m.depends_on.iter().enumerate() {
            let (text, color) = match app.tasks.iter().find(|t| t.task.id() == *d) {
                Some(t) => (
                    format!("{d} {} ({})", t.task.display_title(), t.task.status()),
                    status_color(t.task.status()),
                ),
                None => (format!("{d} (missing)"), Color::Red),
            };
            let span = Span::styled(text, Style::default().fg(color));
            lines.push(field_line(if i == 0 { "depends" } else { "" }, span));
        }
    }
    let fmt = |t: chrono::DateTime<chrono::FixedOffset>| {
        t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
    };
    lines.push(field_line("created", Span::raw(fmt(m.created))));
    lines.push(field_line("updated", Span::raw(fmt(m.updated))));
    if let Some(c) = m.claimed_at {
        lines.push(field_line("claimed", Span::raw(fmt(c))));
    }
    lines.push(field_line("directory", Span::raw(l.dir_name())));
    for (name, text) in [
        ("Description", &l.task.description),
        ("Acceptance", &l.task.acceptance),
    ] {
        lines.push(Line::default());
        lines.push(Line::styled(
            name,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        if text.trim().is_empty() {
            lines.push(Line::styled("-", Style::default().fg(DIM)));
        } else {
            lines.extend(markdown::render(text, Style::default()).lines);
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.details_scroll, 0)),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, err, at)) = &app.message
        && at.elapsed() < Duration::from_secs(6)
    {
        let style = if *err {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        f.render_widget(Paragraph::new(Span::styled(msg.clone(), style)), area);
        return;
    }
    let hints = match app.focus {
        Focus::Input if app.reject.is_some() => {
            "Enter reject with the comment  Alt+Enter new line  Esc cancel the rejection"
        }
        Focus::Input => {
            "Enter send  Alt+Enter new line  Ctrl+A/E line start/end  Ctrl+U/K/W delete  Ctrl+Y yank  Esc leave"
        }
        _ => {
            "j/k move  h/l focus  1-2 view  i write  c new  e edit  s status  , settings  ? help  q quit"
        }
    };
    f.render_widget(Paragraph::new(hints).fg(DIM), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_overlay(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(ov) = app.overlay.as_mut() else {
        return;
    };
    match ov {
        Overlay::Help { scroll } => {
            let lines = help_lines();
            let title = " help (Esc closes, j/k scroll) ";
            let w = lines
                .iter()
                .map(Line::width)
                .max()
                .unwrap_or(0)
                .max(title.len()) as u16
                + 2;
            let h = lines.len() as u16 + 2;
            let r = centered(area, w, h);
            // Keep the last line at the bottom edge of the box when scrolled.
            *scroll = (*scroll).min(h.saturating_sub(r.height));
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new(lines)
                    .block(border(true).title(title))
                    .scroll((*scroll, 0)),
                r,
            );
        }
        Overlay::ConfirmArchive => {
            let r = centered(area, 50, 5);
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new("Move all done tasks to .aq/archive/?\n\ny: yes   n: no")
                    .block(border(true).title(" archive ")),
                r,
            );
        }
        Overlay::Menu { id, actions, sel } => {
            let r = centered(area, 60, actions.len() as u16 + 4);
            f.render_widget(Clear, r);
            let lines: Vec<Line> = actions
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let style = if i == *sel {
                        Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    Line::styled(format!(" {} ", a.label()), style)
                })
                .collect();
            f.render_widget(
                Paragraph::new(lines)
                    .block(border(true).title(format!(" status of task {id} (Enter, Esc) "))),
                r,
            );
        }
        Overlay::Search { input } => {
            let r = centered(area, 50, 3);
            f.render_widget(Clear, r);
            let block = border(true).title(" search (Enter, Esc) ");
            let inner = block.inner(r);
            f.render_widget(block, r);
            style_area(input, true);
            f.render_widget(&**input, inner);
        }
        Overlay::InvalidEdit { id, error, .. } => {
            let r = centered(area, 70, 9);
            f.render_widget(Clear, r);
            let text = vec![
                Line::styled(
                    format!("Task {id} is not valid:"),
                    Style::default().fg(Color::Red),
                ),
                Line::raw(error.clone()),
                Line::default(),
                Line::raw("e: edit again   d: discard the edit"),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(border(true).title(" invalid task ")),
                r,
            );
        }
        Overlay::Settings { .. } => {
            if let Some(Overlay::Settings { sel, mut edit }) = app.overlay.take() {
                draw_settings(f, app, sel, edit.as_deref_mut(), area);
                app.overlay = Some(Overlay::Settings { sel, edit });
            }
        }
        Overlay::Form(form) => draw_form(f, form, area),
    }
}

/// The text of the value of a setting.
fn setting_value(app: &App, s: Setting) -> Span<'static> {
    let dim = Style::default().fg(DIM);
    match s {
        Setting::ListWidth => match app.config.list_width {
            Some(w) => Span::raw(format!("{w} columns")),
            None => Span::styled(format!("auto ({} columns)", app.list_drawn), dim),
        },
        Setting::ListFilter if app.config.list_filter == Filter::default() => {
            Span::styled(app.config.list_filter.name(), dim)
        }
        Setting::ListFilter => Span::raw(app.config.list_filter.name()),
        Setting::Editor if app.config.editor.is_empty() => {
            let cmd = crate::editor::resolve(
                "",
                std::env::var("VISUAL").ok(),
                std::env::var("EDITOR").ok(),
            );
            Span::styled(format!("$VISUAL / $EDITOR / vi ({cmd})"), dim)
        }
        Setting::Editor => Span::raw(app.config.editor.clone()),
        Setting::Notifications if app.config.notifications => Span::raw("on"),
        Setting::Notifications => Span::styled("off", dim),
    }
}

fn draw_settings(
    f: &mut Frame,
    app: &App,
    sel: Setting,
    edit: Option<&mut TextArea<'static>>,
    area: Rect,
) {
    let r = centered(area, 70, Setting::ALL.len() as u16 + 8);
    f.render_widget(Clear, r);
    let block = border(true).title(" settings ");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let file = match &app.config_path {
        Some(p) => Span::raw(p.display().to_string()),
        None => Span::styled("none: the settings are not saved", Style::default().fg(DIM)),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("  {:<14}", "file"), Style::default().fg(DIM)),
            file,
        ]),
        Line::default(),
    ];
    let first_row = lines.len() as u16;
    for s in Setting::ALL {
        let mark = if s == sel { "> " } else { "  " };
        let name = Span::styled(
            format!("{mark}{:<14}", s.name()),
            if s == sel {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        );
        lines.push(Line::from(vec![name, setting_value(app, s)]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "  The list width also saves after a mouse drag of the list border.",
        Style::default().fg(DIM),
    ));
    lines.push(Line::default());
    let hint = if edit.is_some() {
        "  Enter save  Esc cancel  empty value: default"
    } else {
        "  j/k move  h/l change  Enter edit  d default  Esc close"
    };
    lines.push(Line::styled(hint, Style::default().fg(DIM)));
    f.render_widget(Paragraph::new(lines), inner);
    if let Some(ta) = edit {
        let i = Setting::ALL.iter().position(|s| *s == sel).unwrap_or(0) as u16;
        let row = Rect {
            x: inner.x + 16,
            y: inner.y + first_row + i,
            width: inner.width.saturating_sub(16),
            height: 1,
        };
        f.render_widget(Clear, row);
        style_area(ta, true);
        f.render_widget(&*ta, row);
    }
}

fn draw_form(f: &mut Frame, form: &mut Form, area: Rect) {
    let r = centered(
        area,
        area.width.saturating_sub(8).max(40),
        area.height.saturating_sub(4).max(16),
    );
    f.render_widget(Clear, r);
    let title = match form.id {
        Some(id) => format!(" edit task {id} "),
        None => " new task ".to_string(),
    };
    let block = border(true).title(title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let [title_a, desc, acc, row, err, hint] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Percentage(60),
        Constraint::Percentage(40),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let [prio_a, deps_a] =
        Layout::horizontal([Constraint::Length(30), Constraint::Min(16)]).areas(row);
    let cur = form.field;
    let field_block = |name: &str, field: Field| border(cur == field).title(format!(" {name} "));

    for (field, name, a) in [
        (Field::Title, "title (optional)", title_a),
        (Field::Deps, "depends on (ids)", deps_a),
        (
            Field::Description,
            "description (Markdown, Ctrl+G: $EDITOR)",
            desc,
        ),
        (
            Field::Acceptance,
            "acceptance (Markdown, Ctrl+G: $EDITOR)",
            acc,
        ),
    ] {
        let block = field_block(name, field);
        let inner = block.inner(a);
        f.render_widget(block, a);
        if let Some(ta) = form.area_mut(field) {
            style_area(ta, cur == field);
            f.render_widget(&*ta, inner);
        }
    }
    let prio: Vec<Span> = (1..=3u8)
        .map(|p| {
            let label = match p {
                1 => " 1 high ",
                2 => " 2 normal ",
                _ => " 3 low ",
            };
            if p == form.priority {
                Span::styled(
                    label,
                    Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED),
                )
            } else {
                Span::styled(label, Style::default().fg(DIM))
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(prio)).block(field_block("priority (←/→)", Field::Priority)),
        prio_a,
    );
    if let Some(e) = &form.error {
        f.render_widget(Paragraph::new(e.clone()).fg(Color::Red), err);
    }
    f.render_widget(
        Paragraph::new("Tab/Shift+Tab field   Ctrl+S save   Esc cancel").fg(DIM),
        hint,
    );
}

fn help_lines() -> Vec<Line<'static>> {
    let sections: [(&str, &[(&str, &str)]); 3] = [
        (
            "Navigation",
            &[
                ("j / k", "next / previous item"),
                ("g / G", "first / last item"),
                ("Ctrl+D / Ctrl+U", "half page down / up"),
                ("h / l, Tab", "focus the list / the view"),
                ("1 2, [ ]", "view: chat, details"),
                ("/, n, N", "search, next hit, previous hit"),
                ("Enter", "open the link of a message"),
                ("f", "filter: open, review, blocked, all, done"),
                ("R, F5", "reload"),
                ("mouse drag", "move the list border: change the list width"),
            ],
        ),
        (
            "Actions",
            &[
                ("i", "write in the chat of the task"),
                ("c", "new task"),
                ("e / E", "edit the task in a form / in $EDITOR"),
                ("s", "change the status: approve, reject, retry, answer"),
                ("x", "archive the done tasks"),
                ("p", "copy the start text for the orchestrator"),
                ("O / W", "edit ORCHESTRATOR.md / WORKER.md in $EDITOR"),
                (
                    ",",
                    "settings: list width, list filter, editor, notifications",
                ),
                ("q", "quit"),
            ],
        ),
        (
            "Text input",
            &[
                ("Enter", "send"),
                ("Alt+Enter, Shift+Enter", "new line"),
                ("Shift+arrows", "select text"),
                ("Ctrl+A / Ctrl+E", "line start / end"),
                ("Ctrl+F / Ctrl+B", "char forward / back"),
                ("Alt+F / Alt+B", "word forward / back"),
                ("Ctrl+P / Ctrl+N", "line up / down"),
                ("Ctrl+U / Ctrl+K", "delete to line start / end"),
                ("Ctrl+D / Ctrl+H", "delete next / previous char"),
                ("Alt+D / Ctrl+W", "delete next / previous word"),
                ("Ctrl+Y", "insert the deleted text (yank)"),
                ("Ctrl+C / Ctrl+X", "copy / cut the selection"),
                ("Ctrl+V", "paste from the clipboard"),
                ("Ctrl+Z", "undo"),
                ("Ctrl+G", "form: edit the field in $EDITOR"),
                ("Esc", "leave the input; keep the draft; cancel a rejection"),
            ],
        ),
    ];
    let mut lines = Vec::new();
    for (name, keys) in sections {
        lines.push(Line::styled(
            name,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        for (k, d) in keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<24}"), Style::default().fg(Color::Yellow)),
                Span::raw(*d),
            ]));
        }
        lines.push(Line::default());
    }
    lines.push(Line::styled(
        "Chat messages use Markdown. Colored text: [text]{.red}",
        Style::default().fg(DIM),
    ));
    lines
}
