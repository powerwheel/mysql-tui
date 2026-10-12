use crate::app::{App, Busy, Confirm, Focus, Mode, PasswordMode, StatusKind};
use crate::exporter::ExportFormat;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, Wrap,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

pub fn draw(f: &mut Frame, app: &mut App) {
    match app.mode {
        Mode::Password => draw_password(f, app),
        Mode::ConnList => draw_conn_list(f, app),
        Mode::ConnForm => draw_conn_form(f, app),
        Mode::Query => draw_query(f, app),
    }
    if let Some(confirm) = app.confirm.as_ref() {
        draw_confirm(f, confirm);
    }
    if app.show_help {
        draw_help(f);
    }
    if let Some(key) = app.table_detail_key.clone() {
        draw_table_info(f, app, key);
    }
    if app.history_open {
        draw_history(f, app);
    }
    if app.cell_view.is_some() {
        draw_cell_view(f, app);
    }
}

// ---- shared helpers ----

fn status_color(kind: StatusKind) -> Color {
    match kind {
        StatusKind::Info => Color::Gray,
        StatusKind::Ok => Color::Green,
        StatusKind::Error => Color::Red,
    }
}

fn hint_text(app: &App) -> String {
    match app.busy {
        Some(Busy::Connecting) => "connecting...".to_string(),
        Some(Busy::Querying) => "working... Esc cancel".to_string(),
        Some(Busy::Exporting) => "exporting... Esc cancel".to_string(),
        None => match (app.mode, app.focus) {
            (Mode::Query, Focus::Editor) => "Editor | F9 run | F3 history | Tab \u{2192} results | F1 help".to_string(),
            (Mode::Query, Focus::Results)
                if app.current_result().is_some_and(|rv| rv.result.has_more) =>
            {
                "Results | n next batch | Enter view cell | y/Y copy | Tab \u{2192} sidebar | F1 help".to_string()
            }
            (Mode::Query, Focus::Results) if app.result_views.len() > 1 => {
                "Results | Enter view cell | [ ] result set | y/Y copy | Tab \u{2192} sidebar | F1 help".to_string()
            }
            (Mode::Query, Focus::Results) => {
                "Results | Enter view cell | y/Y copy | F3 history | Tab \u{2192} sidebar | F1 help".to_string()
            }
            (Mode::Query, Focus::Sidebar) => "Sidebar | \u{2190}\u{2192} expand | Enter use/insert | F2 table detail | Tab \u{2192} editor".to_string(),
            _ => String::new(),
        },
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let (msg, kind) = &app.status;
    let hint = hint_text(app);
    let hint_w = hint.chars().count() as u16;
    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_w)]).split(area);
    let left = Paragraph::new(Line::from(Span::styled(
        format!(" {msg}"),
        Style::default().fg(status_color(*kind)),
    )));
    f.render_widget(left, cols[0]);
    if !hint.is_empty() {
        let right = Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(DIM))))
            .alignment(Alignment::Right);
        f.render_widget(right, cols[1]);
    }
}

fn bottom_line(area: Rect) -> Rect {
    Rect {
        y: area.bottom().saturating_sub(1),
        height: 1,
        ..area
    }
}

fn centered_fixed(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Renders `value` with a block cursor at `char_idx`, styled when active.
fn field_spans(value: &str, char_idx: usize, active: bool) -> Vec<Span<'static>> {
    let chars: Vec<char> = value.chars().collect();
    let before: String = chars.iter().take(char_idx).collect();
    let cur: String = chars
        .get(char_idx)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".into());
    let after: String = chars.iter().skip(char_idx + 1).collect();
    if active {
        vec![
            Span::styled(before, Style::default().fg(Color::Yellow)),
            Span::styled(cur, Style::default().bg(Color::Yellow).fg(Color::Black)),
            Span::styled(after, Style::default().fg(Color::Yellow)),
        ]
    } else {
        vec![Span::styled(
            value.to_string(),
            Style::default().fg(Color::Gray),
        )]
    }
}

// ---- connection list ----

fn draw_conn_list(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered_fixed(
        area,
        70.min(area.width),
        (app.config.connections.len() as u16 + 6)
            .clamp(9, 24)
            .min(area.height),
    );
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" MySQL Connections ")
        .title_bottom(Line::from(" Ctrl+Q quit ").right_aligned())
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = Vec::new();
    if app.config.connections.is_empty() {
        lines.push(Line::from(Span::styled(
            "no connections yet - press n to create one",
            Style::default().fg(DIM),
        )));
    } else {
        for (i, conn) in app.config.connections.iter().enumerate() {
            let selected = i == app.conn_list_selected;
            let auto = app.config.auto_connect == Some(i);
            let marker = if auto { "*" } else { " " };
            let name_w = app
                .config
                .connections
                .iter()
                .map(|c| c.display_name().chars().count())
                .max()
                .unwrap_or(0)
                .max(6);
            let text = format!(
                "{marker} {:<name_w$}  {}",
                conn.display_name(),
                conn.describe(),
            );
            let style = if selected {
                Style::default()
                    .bg(ACCENT)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter connect | n new | e edit | d delete | a auto-connect",
        Style::default().fg(DIM),
    )));
    if let Some(e) = &app.config_error {
        lines.push(Line::from(Span::styled(
            format!("config: {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
    draw_status(f, bottom_line(area), app);
}

// ---- connection form ----

fn draw_conn_form(f: &mut Frame, app: &App) {
    let area = f.area();
    draw_conn_list(f, app); // backdrop
    let Some(form) = app.form.as_ref() else {
        return;
    };
    let label_w = form
        .fields
        .iter()
        .map(|fld| fld.label.len())
        .max()
        .unwrap_or(0)
        .max(6);
    let w = (label_w as u16 + 44).min(area.width);
    let h = (form.fields.len() as u16 + 5).min(area.height);
    let rect = centered_fixed(area, w, h);
    f.render_widget(Clear, rect);
    let title = if form.index.is_some() {
        " Edit Connection "
    } else {
        " New Connection "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in form.fields.iter().enumerate() {
        let active = i == form.active;
        let char_idx = field
            .value
            .get(..field.cursor)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(
                format!("{:<label_w$}", field.label),
                if active {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
            Span::raw(": "),
        ];
        if field.is_choice() {
            let style = if active {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            };
            spans.push(Span::styled(format!("\u{2039} {} \u{203a}", field.value), style));
        } else {
            spans.extend(field_spans(&field.rendered(), char_idx, active));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tab/Up/Down field | \u{2190}\u{2192} cycle TLS | Ctrl+S save | Esc cancel",
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- password screen ----

fn draw_password(f: &mut Frame, app: &App) {
    let area = f.area();
    let w = 48u16.min(area.width);
    let h = match app.password_state.as_ref().map(|p| p.mode) {
        Some(PasswordMode::Setup) => 8,
        Some(PasswordMode::Unlock) => 6,
        None => 6,
    };
    let h = h.min(area.height);
    let rect = centered_fixed(area, w, h);
    f.render_widget(Clear, rect);

    let title = match app.password_state.as_ref().map(|p| p.mode) {
        Some(PasswordMode::Setup) => " Set Password ",
        Some(PasswordMode::Unlock) => " Unlock ",
        None => " Password ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let Some(ps) = app.password_state.as_ref() else {
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    // Password field
    let pw_active = ps.active == 0;
    let pw_display = if ps.password.is_empty() {
        String::new()
    } else {
        "*".repeat(ps.password.len())
    };
    let mut pw_spans = vec![
        Span::raw(" "),
        Span::styled(
            format!("{:<10}", "Password"),
            if pw_active {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
        Span::raw(": "),
    ];
    pw_spans.extend(field_spans(&pw_display, pw_display.chars().count(), pw_active));
    lines.push(Line::from(pw_spans));

    // Confirm field (setup mode only)
    if ps.mode == PasswordMode::Setup {
        let cf_active = ps.active == 1;
        let cf_display = if ps.confirm.is_empty() {
            String::new()
        } else {
            "*".repeat(ps.confirm.len())
        };
        let mut cf_spans = vec![
            Span::raw(" "),
            Span::styled(
                format!("{:<10}", "Confirm"),
                if cf_active {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
            Span::raw(": "),
        ];
        cf_spans.extend(field_spans(&cf_display, cf_display.chars().count(), cf_active));
        lines.push(Line::from(cf_spans));
    }

    lines.push(Line::from(""));

    // Error message
    if !ps.error.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}", ps.error),
            Style::default().fg(Color::Red),
        )));
    }

    lines.push(Line::from(""));

    // Hint
    let hint = match ps.mode {
        PasswordMode::Setup => "Tab field | Enter confirm | Esc quit",
        PasswordMode::Unlock => "Enter unlock | Esc quit",
    };
    lines.push(Line::from(Span::styled(
        format!(" {hint}"),
        Style::default().fg(DIM),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}

// ---- query screen ----

fn draw_query(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let hsplit = Layout::horizontal([Constraint::Length(22), Constraint::Min(0)]).split(area);
    draw_sidebar(f, app, hsplit[0]);
    let main = hsplit[1];

    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // tab strip
        Constraint::Length(6), // editor
        Constraint::Min(3),    // results
        Constraint::Length(1), // status
    ])
    .split(main);

    draw_header(f, rows[0], app);
    draw_tabs(f, rows[1], app);

    let dialog_open = app.query_dialog_open();
    let editor_focused = app.focus == Focus::Editor && !dialog_open;

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SQL ")
        .border_style(if editor_focused {
            Style::default().fg(ACCENT)
        } else {
            Style::default().fg(DIM)
        });
    let inner = block.inner(rows[2]);
    f.render_widget(block, rows[2]);

    app.editor_view = (inner.width, inner.height);
    app.clamp_editor_scroll();
    let visible: Vec<Line> = app
        .editor
        .render_lines(app.editor_scroll.1)
        .into_iter()
        .skip(app.editor_scroll.0)
        .collect();
    f.render_widget(Paragraph::new(visible), inner);

    if editor_focused {
        let (x, y) = editor_cursor_screen(app, inner);
        f.set_cursor_position((x, y));
        if app
            .popup
            .as_ref()
            .is_some_and(|p| !p.suggestions.is_empty())
        {
            draw_completion(f, app, inner);
        }
    }

    draw_results(f, app, rows[3]);
    draw_status(f, rows[4], app);

    if let Some(dlg) = app.export_dlg.as_ref() {
        draw_export(f, dlg, area);
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();
    for (i, (title, running)) in app.tab_titles().into_iter().enumerate() {
        let active = i == app.tab_idx;
        let marker = if running { " \u{27f3}" } else { "" };
        let label = format!(" {}:{title}{marker} ", i + 1);
        let style = if active {
            Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
        } else if running {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(
        "Ctrl+T new  Ctrl+W close  Alt+\u{2190}/\u{2192} switch",
        Style::default().fg(DIM),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let dialog_open = app.query_dialog_open();
    let focused = app.focus == Focus::Sidebar && !dialog_open;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Databases ")
        .border_style(if focused {
            Style::default().fg(ACCENT)
        } else {
            Style::default().fg(DIM)
        });
    let inner = block.inner(area);
    f.render_widget(block, area);

    use crate::app::SidebarItem;

    let items = app.sidebar_items();
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("no databases", Style::default().fg(DIM))),
            inner,
        );
        return;
    }

    let cur_db = &app.completer.schema().database;
    let view_h = inner.height as usize;
    let sel = app.sidebar_selected.min(items.len().saturating_sub(1));
    let start = if sel >= app.sidebar_scroll + view_h {
        sel.saturating_sub(view_h.saturating_sub(1))
    } else if sel < app.sidebar_scroll {
        sel
    } else {
        app.sidebar_scroll
    };
    app.sidebar_scroll = start;
    let end = (start + view_h).min(items.len());

    let mut lines: Vec<Line> = Vec::new();
    for (i, item) in items.iter().enumerate().skip(start).take(end - start) {
        let is_sel = i == sel;
        let (prefix, label, is_cur) = match item {
            SidebarItem::Database(db) => {
                let expanded = app.sidebar_expanded.contains(db);
                let arrow = if expanded { "\u{25be}" } else { "\u{25b8}" };
                (format!("{arrow} "), db.clone(), db == cur_db)
            }
            SidebarItem::Table(db, table) => {
                let table_key = format!("{}.{}", db, table);
                let expanded = app.sidebar_tables_expanded.contains(&table_key);
                let arrow = if expanded { "\u{25be}" } else { "\u{25b8}" };
                (format!("  {arrow} "), table.clone(), false)
            }
            SidebarItem::Column(_, _, col) => ("    \u{2022} ".to_string(), col.clone(), false),
        };
        let full = format!("{}{}", prefix, label);
        let max_w = inner.width as usize;
        let text = if full.chars().count() > max_w {
            let keep = max_w.saturating_sub(1);
            let mut s: String = full.chars().take(keep).collect();
            s.push('\u{2026}');
            s
        } else {
            full
        };
        let style = if is_sel && focused {
            Style::default()
                .bg(ACCENT)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else if is_cur {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        " mysql-tui ",
        Style::default()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )];
    match (&app.conn_info, app.busy) {
        (Some(info), _) => {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                info.label(),
                Style::default().fg(Color::Green),
            ));
            if info.tunnel_port.is_some() {
                spans.push(Span::styled(
                    " (ssh)",
                    Style::default().fg(Color::Green),
                ));
            }
            if info.ssl_cipher.is_some() {
                spans.push(Span::styled(
                    " (tls)",
                    Style::default().fg(Color::Green),
                ));
            }
            spans.push(Span::styled(
                format!("  MySQL {}", info.server_version),
                Style::default().fg(DIM),
            ));
            spans.push(Span::styled(
                format!("  {} tables", app.completer.schema().table_count()),
                Style::default().fg(DIM),
            ));
        }
        (None, Some(Busy::Connecting)) => {
            spans.push(Span::styled(
                " connecting...",
                Style::default().fg(Color::Yellow),
            ));
        }
        _ => {}
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn editor_cursor_screen(app: &App, inner: Rect) -> (u16, u16) {
    let (line, off) = app.editor.cursor;
    let lines = app.editor.lines();
    let line = line.min(lines.len().saturating_sub(1));
    let col = lines[line]
        .get(..off)
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let (sy, sx) = app.editor_scroll;
    let y = inner.y + (line.saturating_sub(sy) as u16).min(inner.height.saturating_sub(1));
    let x = inner.x + (col.saturating_sub(sx) as u16).min(inner.width.saturating_sub(1));
    (x, y)
}

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let dialog_open = app.query_dialog_open();
    let focused = app.focus == Focus::Results && !dialog_open;
    app.results_view_w = area.width;

    if let Some(err) = &app.last_error {
        let border = if focused { Color::Red } else { DIM };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Error ")
            .border_style(Style::default().fg(border));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let mut lines: Vec<Line> = err
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::Red))))
            .collect();
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "y copy error | run another query to dismiss",
            Style::default().fg(DIM),
        )));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        return;
    }

    let Some(rv) = app.current_result() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(Style::default().fg(DIM));
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(Span::styled(
                "no result yet - write a query and press Enter",
                Style::default().fg(DIM),
            )),
            inner,
        );
        return;
    };

    let total_cols = rv.result.columns.len();
    let total_rows = rv.result.rows.len();
    let first = app.first_col.min(total_cols.saturating_sub(1));

    // pick the widest run of columns that fits the viewport
    let mut shown: Vec<usize> = Vec::new();
    let mut used: u16 = 0;
    for ci in first..total_cols {
        let w = rv.widths[ci];
        let extra = if shown.is_empty() { 0 } else { 2 };
        if !shown.is_empty() && used.saturating_add(w + extra) > inner_width(area) {
            break;
        }
        used += w + extra;
        shown.push(ci);
    }
    if shown.is_empty() {
        shown.push(first);
    }
    let last = *shown.last().unwrap();

    let set_label = if app.result_views.len() > 1 {
        format!(
            " (set {}/{}) ",
            app.result_idx + 1,
            app.result_views.len()
        )
    } else {
        " ".to_string()
    };
    let more = if rv.result.has_more {
        match rv.result.next_page() {
            Some((_, page)) => format!(" | n: next {page}"),
            None => " (truncated)".to_string(),
        }
    } else if rv.result.truncated {
        " (truncated)".to_string()
    } else {
        String::new()
    };
    let mut title = format!(" Results{set_label}- {total_rows} rows x {total_cols} cols{more} ");
    if total_cols > 1 && (first > 0 || last + 1 < total_cols) {
        title.push_str(&format!("| cols {}-{} ", first + 1, last + 1));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if focused {
            Style::default().fg(ACCENT)
        } else {
            Style::default().fg(DIM)
        });
    let inner = block.inner(area);
    f.render_widget(block, area);

    if total_cols == 0 || total_rows == 0 {
        f.render_widget(
            Paragraph::new(Span::styled("empty result set", Style::default().fg(DIM))),
            inner,
        );
        return;
    }

    let header = Row::new(shown.iter().map(|&ci| {
        Cell::from(rv.result.columns[ci].clone())
            .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
    }))
    .bottom_margin(0);

    let sel_row = app.table_state.selected();
    let sel_col = app.selected_col.min(total_cols - 1);
    let data_rows = rv.display.iter().enumerate().map(|(ri, r)| {
        Row::new(shown.iter().map(|&ci| {
            let text = r.get(ci).map(|s| fit_cell(s, rv.widths[ci])).unwrap_or_default();
            let mut cell = Cell::from(text);
            if focused && Some(ri) == sel_row && ci == sel_col {
                cell = cell.style(
                    Style::default()
                        .bg(ACCENT)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                );
            }
            cell
        }))
    });

    let constraints: Vec<Constraint> = shown
        .iter()
        .map(|&ci| Constraint::Length(rv.widths[ci]))
        .collect();
    let table = Table::new(data_rows, constraints)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, inner, &mut app.table_state);
}

fn inner_width(area: Rect) -> u16 {
    area.width.saturating_sub(2)
}

/// First line of `s` fitted into `width` cells; `…` marks truncation and `⏎`
/// marks a value that continues on further lines.
fn fit_cell(s: &str, width: u16) -> String {
    let width = width as usize;
    let mut lines = s.split('\n');
    let first = lines.next().unwrap_or("").trim_end_matches('\r');
    let multiline = lines.next().is_some();
    let marker = if multiline { "\u{23ce}" } else { "" };
    if first.width() + marker.width() <= width {
        return format!("{first}{marker}");
    }
    let mut out = String::new();
    let mut used = 0;
    for c in first.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('\u{2026}');
    out
}

/// Hard-wrap every line of `text` to at most `width` display cells.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let mut cur = String::new();
        let mut used = 0;
        for c in line.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                used = 0;
            }
            cur.push(c);
            used += w;
        }
        out.push(cur);
    }
    out
}

// ---- table info panel ----

fn draw_table_info(f: &mut Frame, app: &App, table_key: String) {
    let area = f.area();
    let w = 90.min(area.width);
    let h = (area.height * 3 / 4).max(10).min(area.height);
    let rect = centered_fixed(area, w, h);
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" DDL: {} | j/k scroll | y copy | Esc close ", table_key))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if inner.height < 3 || inner.width < 20 {
        return;
    }

    let schema = app.completer.schema();
    let ddl = schema.ddl.get(&table_key);

    let mut lines: Vec<Line> = Vec::new();

    match ddl {
        Some(d) if !d.is_empty() => {
            for line in d.lines() {
                lines.push(highlight_sql(line));
            }
        }
        _ => {
            lines.push(Line::from(Span::styled(
                "  loading...",
                Style::default().fg(DIM),
            )));
        }
    }

    let total = lines.len();
    let visible = inner.height as usize;
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.detail_scroll.min(max_scroll);

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    f.render_widget(para, inner);
}

// ---- cell viewer popup ----

fn draw_cell_view(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let total_rows = app.current_result().map(|rv| rv.result.rows.len()).unwrap_or(0);
    let Some(view) = app.cell_view.as_mut() else {
        return;
    };
    let w = (area.width * 9 / 10).clamp(20.min(area.width), area.width);
    let h = (area.height * 3 / 4).clamp(6.min(area.height), area.height);
    let rect = centered_fixed(area, w, h);
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Cell: {} (row {}/{total_rows}) ", view.column, view.row + 1))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let body = rows[0];
    let text_w = body.width.saturating_sub(1) as usize; // leave room for the scrollbar
    let wrapped = wrap_text(view.text(), text_w);
    let total = wrapped.len();
    let page = body.height as usize;
    view.page = page;
    let max_scroll = total.saturating_sub(page);
    view.scroll = view.scroll.min(max_scroll);
    let scroll = view.scroll;

    let is_null = view.raw == "NULL";
    let style = if is_null {
        Style::default().fg(DIM).add_modifier(Modifier::ITALIC)
    } else {
        Style::default()
    };
    let lines: Vec<Line> = wrapped
        .iter()
        .skip(scroll)
        .take(page)
        .map(|l| Line::from(Span::styled(l.clone(), style)))
        .collect();
    let text_area = Rect { width: text_w as u16, ..body };
    f.render_widget(Paragraph::new(lines), text_area);

    if total > page {
        let mut sb_state = ScrollbarState::new(max_scroll).position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            body,
            &mut sb_state,
        );
    }

    let json_hint = match (&view.pretty, view.show_pretty) {
        (Some(_), true) => " | p raw",
        (Some(_), false) => " | p pretty JSON",
        (None, _) => "",
    };
    let footer = format!(
        "{} chars, {} bytes, {} lines | line {}-{}/{}{json_hint}",
        view.raw.chars().count(),
        view.raw.len(),
        view.raw.split('\n').count(),
        (scroll + 1).min(total.max(1)),
        (scroll + page).min(total),
        total,
    );
    let keys = " | j/k scroll  \u{2190}/\u{2192} column  J/K row  y copy  Esc close";
    let footer_w = rows[1].width as usize;
    let mut spans = vec![Span::styled(footer.clone(), Style::default().fg(Color::Gray))];
    if footer.width() + keys.width() <= footer_w {
        spans.push(Span::styled(keys, Style::default().fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[1]);
}

fn highlight_sql(line: &str) -> Line<'static> {
    let keywords = [
        "CREATE", "TABLE", "NOT", "NULL", "DEFAULT", "AUTO_INCREMENT", "PRIMARY", "KEY",
        "UNIQUE", "INDEX", "ENGINE", "CHARSET", "COLLATE", "COMMENT", "IF", "EXISTS",
        "INT", "BIGINT", "SMALLINT", "TINYINT", "VARCHAR", "CHAR", "TEXT", "LONGTEXT",
        "MEDIUMTEXT", "BLOB", "DATETIME", "TIMESTAMP", "DATE", "TIME", "DECIMAL", "FLOAT",
        "DOUBLE", "BOOLEAN", "ENUM", "SET", "JSON", "UNSIGNED", "ZEROFILL", "ON", "UPDATE",
        "CURRENT_TIMESTAMP", "CONSTRAINT", "FOREIGN", "REFERENCES", "CASCADE", "RESTRICT",
        "FULLTEXT", "SPATIAL", "BTREE", "HASH",
    ];

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = line;

    while !remaining.is_empty() {
        let ws_len = remaining.len() - remaining.trim_start().len();
        if ws_len > 0 {
            spans.push(Span::raw(remaining[..ws_len].to_string()));
            remaining = &remaining[ws_len..];
        }

        if remaining.is_empty() {
            break;
        }

        let word_end = remaining
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(remaining.len());

        if word_end == 0 {
            let ch_len = remaining.chars().next().unwrap().len_utf8();
            let ch = &remaining[..ch_len];
            let color = if ch == "(" || ch == ")" || ch == "," || ch == ";" {
                Color::Yellow
            } else {
                Color::White
            };
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            remaining = &remaining[ch_len..];
            continue;
        }

        let word = &remaining[..word_end];
        let rest = &remaining[word_end..];

        if word.starts_with('\'') || word.starts_with('"') || word.starts_with('`') {
            let quote = word.chars().next().unwrap();
            let close = rest.find(quote).map(|i| i + 1).unwrap_or(rest.len());
            let full = format!("{}{}", word, &rest[..close]);
            spans.push(Span::styled(full, Style::default().fg(Color::Green)));
            remaining = &rest[close..];
        } else if word.chars().next().unwrap().is_ascii_digit() {
            spans.push(Span::styled(
                word.to_string(),
                Style::default().fg(Color::Magenta),
            ));
            remaining = rest;
        } else if keywords.contains(&word.to_uppercase().as_str()) {
            spans.push(Span::styled(
                word.to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            remaining = rest;
        } else {
            spans.push(Span::styled(
                word.to_string(),
                Style::default().fg(Color::White),
            ));
            remaining = rest;
        }
    }

    Line::from(spans)
}

// ---- completion popup ----

fn draw_completion(f: &mut Frame, app: &App, editor_inner: Rect) {
    let Some(popup) = app.popup.as_ref() else {
        return;
    };
    let n = popup.suggestions.len();
    if n == 0 {
        return;
    }
    let max_label = popup
        .suggestions
        .iter()
        .map(|s| s.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(30);
    let width: u16 = (max_label + 14).clamp(26, 50) as u16;
    let win_h = n.min(8);
    let height = win_h as u16 + 2;

    let (cx, cy) = editor_cursor_screen(app, editor_inner);
    let area = f.area();
    let x = cx.min(area.right().saturating_sub(width));
    let below = cy.saturating_add(1);
    let y = if below + height <= area.bottom() {
        below
    } else {
        cy.saturating_sub(height)
    };
    let rect = Rect {
        x,
        y,
        width,
        height: height.min(area.height),
    };
    if rect.height < 3 {
        return;
    }
    f.render_widget(Clear, rect);

    let start = if popup.selected >= win_h {
        popup.selected + 1 - win_h
    } else {
        0
    };
    let items: Vec<ListItem> = popup.suggestions[start..(start + win_h).min(n)]
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let i = start + idx;
            let sel_style = Style::default()
                .bg(ACCENT)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD);
            let label_style = if i == popup.selected {
                sel_style
            } else {
                Style::default().fg(Color::White)
            };
            let kind_style = if i == popup.selected {
                sel_style
            } else {
                Style::default().fg(DIM)
            };
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{:<max_label$}", s.label), label_style),
                Span::styled(format!("  {}", s.kind.label()), kind_style),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT)),
    );
    f.render_widget(list, rect);
}

// ---- export dialog ----

fn draw_export(f: &mut Frame, dlg: &crate::app::ExportDialog, area: Rect) {
    let w = 58.min(area.width);
    let h = 7.min(area.height);
    let rect = centered_fixed(area, w, h);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Export Result ")
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let char_idx = dlg
        .path
        .get(..dlg.cursor)
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let mut path_spans = vec![
        Span::raw(" "),
        Span::styled("Path: ", Style::default().fg(Color::Gray)),
    ];
    path_spans.extend(field_spans(&dlg.path, char_idx, dlg.active == 0));

    let mut fmt_spans = vec![
        Span::raw(" "),
        Span::styled("Format:", Style::default().fg(Color::Gray)),
    ];
    for (i, fmt) in ExportFormat::ALL.iter().enumerate() {
        if i > 0 {
            fmt_spans.push(Span::raw("  "));
        }
        let is_cur = *fmt == dlg.format;
        let style = if is_cur {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        fmt_spans.push(Span::styled(fmt.label().to_string(), style));
    }

    let hint = if dlg.truncate {
        "Tab switch | < > format | Enter export all | Esc cancel"
    } else {
        "Tab switch | < > format | Enter export | Esc cancel"
    };
    let lines = vec![
        Line::from(path_spans),
        Line::from(""),
        Line::from(fmt_spans),
        Line::from(""),
        Line::from(Span::styled(
            hint,
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- confirm modal ----

fn draw_confirm(f: &mut Frame, confirm: &Confirm) {
    let area = f.area();
    let msg_w = confirm.message.chars().count() as u16;
    let w = (msg_w + 6).clamp(44, area.width);
    let h = 5.min(area.height);
    let rect = centered_fixed(area, w, h);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Confirm ")
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let lines = vec![
        Line::from(confirm.message.clone()).alignment(Alignment::Center),
        Line::from(""),
        Line::from(Span::styled(
            "Enter/y confirm | Esc/n cancel",
            Style::default().fg(DIM),
        ))
        .alignment(Alignment::Center),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- history panel ----

fn draw_history(f: &mut Frame, app: &App) {
    let area = f.area();
    let entries = app.filtered_history();
    let h = (entries.len() as u16 + 4).clamp(8, 22).min(area.height);
    let rect = centered_fixed(area, 78.min(area.width), h);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Query History (F3) ")
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let cols = Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let filter_pos = app.history_filter.chars().count();
    let filter_line = Line::from(
        {
            let mut spans = vec![Span::styled("filter: ", Style::default().fg(DIM))];
            spans.extend(field_spans(&app.history_filter, filter_pos, true));
            spans
        },
    );
    f.render_widget(Paragraph::new(filter_line), cols[0]);

    let list_area = cols[1];
    let visible = list_area.height as usize;
    let max_start = entries.len().saturating_sub(visible);
    let start = app
        .history_selected
        .saturating_sub(visible.saturating_sub(1))
        .min(max_start);
    let width = (list_area.width as usize).saturating_sub(2);
    for (row, i) in entries.iter().enumerate().skip(start).take(visible) {
        let text: String = app.history[*i]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let text = if text.chars().count() > width {
            let mut s: String = text.chars().take(width.saturating_sub(1)).collect();
            s.push('\u{2026}');
            s
        } else {
            text
        };
        let selected = row == app.history_selected;
        let style = if selected {
            Style::default().bg(ACCENT).fg(Color::Black)
        } else {
            Style::default().fg(Color::Gray)
        };
        let row_rect = Rect {
            y: list_area.y + (row - start) as u16,
            height: 1,
            ..list_area
        };
        f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), row_rect);
    }

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Enter load | Ctrl+D delete | Esc close",
            Style::default().fg(DIM),
        ))),
        cols[2],
    );
}

// ---- help ----

fn draw_help(f: &mut Frame) {
    let area = f.area();

    let key = |k: &str| Span::styled(format!("{k:<15}"), Style::default().fg(Color::Yellow));
    let desc = |d: &str| Span::styled(d.to_string(), Style::default().fg(Color::Gray));
    let entry = |k: &str, d: &str| Line::from(vec![key(k), desc(d)]);
    let blank = || Line::from("");
    let section = |t: &str| {
        Line::from(Span::styled(
            t.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
    };

    let left = vec![
        section("Editor"),
        entry("F9", "run query"),
        entry("Enter", "new line"),
        entry("Ctrl+Space", "show completions"),
        entry("Tab", "accept suggestion / switch pane"),
        entry("Ctrl+P / Ctrl+N", "history (or navigate suggestions)"),
        entry("Shift+arrows", "select text (Ctrl+A select all)"),
        entry("Ctrl+Z / Ctrl+Y", "undo / redo"),
        entry("Ctrl+C/X/V", "copy / cut / paste selection"),
        blank(),
        section("Results"),
        entry("j/k arrows", "select row"),
        entry("Left/Right", "move column cursor"),
        entry("Enter / v", "view full cell"),
        entry("n", "load next batch of rows"),
        entry("y", "copy cell to clipboard"),
        entry("Y", "copy row (TSV) to clipboard"),
        entry("[ / ]", "switch result set"),
        entry("Tab", "switch to sidebar"),
        entry("Esc/i", "back to editor"),
        blank(),
        section("Sidebar"),
        entry("j/k arrows", "select item"),
        entry("\u{2190}/\u{2192}", "collapse / expand database"),
        entry("Enter", "use database / insert table name"),
        entry("F2", "show table detail popup"),
        entry("Tab/Esc", "back to editor"),
    ];
    let right = vec![
        section("Cell Viewer"),
        entry("j/k PgUp/PgDn", "scroll (g/G top / bottom)"),
        entry("\u{2190}/\u{2192} h/l", "previous / next column"),
        entry("J / K", "next / previous row"),
        entry("p", "toggle pretty JSON"),
        entry("y", "copy cell text"),
        entry("Esc/q/Enter", "close popup"),
        blank(),
        section("Table Detail"),
        entry("j/k arrows", "scroll content"),
        entry("y", "copy to clipboard"),
        entry("Esc/q", "close popup"),
        blank(),
        section("Tabs"),
        entry("Ctrl+T", "new query tab"),
        entry("Ctrl+W", "close current tab"),
        entry("Alt+\u{2190}/\u{2192}", "prev / next tab (Ctrl+PgUp/PgDn)"),
        entry("Alt+1..9", "jump to tab"),
        blank(),
        section("General"),
        entry("F3", "query history panel"),
        entry("Ctrl+D", "disconnect"),
        entry("Ctrl+E", "export result"),
        entry("Ctrl+F", "refresh schema"),
        entry("Ctrl+Q/Ctrl+C", "quit"),
        entry("Esc", "close dialogs / cancel query"),
    ];

    const COL_W: u16 = 52;
    let two_cols = area.width >= COL_W * 2 + 4;
    let (w, h) = if two_cols {
        (COL_W * 2 + 3, left.len().max(right.len()) as u16 + 2)
    } else {
        (COL_W.min(area.width), (left.len() + right.len() + 1) as u16 + 2)
    };
    let rect = centered_fixed(area, w.min(area.width), h.min(area.height));
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help (F1 / ?) ")
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if two_cols {
        let cols = Layout::horizontal([
            Constraint::Length(COL_W),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);
        f.render_widget(Paragraph::new(left), cols[0]);
        f.render_widget(Paragraph::new(right), cols[2]);
    } else {
        let mut lines = left;
        lines.push(blank());
        lines.extend(right);
        f.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DbEvent, QueryResult};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn fit_cell_truncates_by_display_width() {
        assert_eq!(fit_cell("hello", 10), "hello");
        assert_eq!(fit_cell("hello world", 6), "hello\u{2026}");
        assert_eq!(fit_cell("中文字符", 5), "中文\u{2026}");
        assert_eq!(fit_cell("a\nb", 10), "a\u{23ce}");
        assert_eq!(fit_cell("abcdef\nb", 4), "abc\u{2026}");
        assert_eq!(fit_cell("line\r\nnext", 10), "line\u{23ce}");
    }

    #[test]
    fn wrap_text_hard_wraps_and_keeps_blank_lines() {
        assert_eq!(wrap_text("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap_text("ab\n\ncd", 10), vec!["ab", "", "cd"]);
        assert_eq!(wrap_text("中文字", 4), vec!["中文", "字"]);
        assert_eq!(wrap_text("", 4), vec![""]);
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_truncated_grid_and_cell_viewer() {
        let mut app = App::new().unwrap();
        app.config = Default::default();
        app.mode = Mode::Query;
        app.focus = Focus::Results;
        app.busy = None;
        let long = "x".repeat(120);
        app.on_db_event(DbEvent::QueryDone {
            results: vec![QueryResult {
                columns: vec!["id".into(), "body".into()],
                rows: vec![vec![
                    mysql::Value::Int(1),
                    mysql::Value::Bytes(format!("{{\"k\":\"{long}\"}}").into_bytes()),
                ]],
                truncated: false,
                elapsed: std::time::Duration::ZERO,
                source: None,
                has_more: false,
            }],
            message: "1 row".into(),
        });

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen = buffer_text(&term);
        assert!(screen.contains('\u{2026}'), "long cell should be truncated with an ellipsis:\n{screen}");
        assert!(!screen.contains("Cell: body"));

        app.selected_col = 1;
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen = buffer_text(&term);
        assert!(screen.contains("Cell: body (row 1/1)"), "{screen}");
        assert!(screen.contains("\"k\": \"xxxx"), "pretty JSON expected:\n{screen}");
        assert!(screen.contains("p raw"), "{screen}");
        let view = app.cell_view.as_ref().unwrap();
        assert!(view.page > 0, "renderer must record the page height");
    }

    #[test]
    fn results_title_advertises_next_batch() {
        let mut app = App::new().unwrap();
        app.config = Default::default();
        app.mode = Mode::Query;
        app.focus = Focus::Results;
        app.busy = None;
        app.on_db_event(DbEvent::QueryDone {
            results: vec![QueryResult {
                columns: vec!["id".into()],
                rows: vec![vec![mysql::Value::Int(1)], vec![mysql::Value::Int(2)]],
                truncated: false,
                elapsed: std::time::Duration::ZERO,
                source: Some("SELECT id FROM t LIMIT 2".into()),
                has_more: true,
            }],
            message: "2 rows".into(),
        });
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen = buffer_text(&term);
        assert!(screen.contains("2 rows x 1 cols | n: next 2"), "{screen}");
        assert!(screen.contains("n next batch"), "{screen}");

        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('n'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.on_db_event(DbEvent::PageDone {
            rows: vec![vec![mysql::Value::Int(3)]],
            has_more: false,
            elapsed: std::time::Duration::ZERO,
        });
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen = buffer_text(&term);
        assert!(screen.contains("3 rows x 1 cols "), "{screen}");
        assert!(!screen.contains("n: next"), "{screen}");
        assert!(!screen.contains("truncated"), "{screen}");
    }
}
