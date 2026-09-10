//! All TUI rendering functions — layout, widgets, overlays.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::prompts::QUICK_PROMPTS;
use crate::store::TaskStatus;

use super::app::{App, InputField, InputMode, REFRESH_INTERVAL};

// ---------------------------------------------------------------------------
// Main layout
// ---------------------------------------------------------------------------

pub fn ui(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Main vertical layout: header(1) | body | action bar(1) | status bar(1)
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // action bar
            Constraint::Length(1), // status / notification bar
        ])
        .split(size);

    render_header(frame, app, main_chunks[0]);

    // Body: kanban columns (left) | detail sidebar (right)
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(main_chunks[1]);

    render_columns(frame, app, body_chunks[0]);
    render_detail_sidebar(frame, app, body_chunks[1]);

    render_action_bar(frame, app, main_chunks[2]);
    render_status_bar(frame, app, main_chunks[3]);

    // Overlays (drawn on top of everything)
    if app.show_help {
        render_help_overlay(frame, size);
    }

    match &app.input_mode {
        InputMode::NewTask { .. } => render_new_task_form(frame, app, size),
        InputMode::EditTask { .. } => render_edit_task_form(frame, app, size),
        InputMode::SendMessage { .. } => render_send_message(frame, app, size),
        InputMode::Confirm { .. } => render_confirm_dialog(frame, app, size),
        InputMode::QuickPrompts => render_quick_prompts(frame, app, size),
        InputMode::Normal => {}
    }
}

// ---------------------------------------------------------------------------
// Header bar
// ---------------------------------------------------------------------------

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let backlog_count = app.tasks_in_column(&TaskStatus::Backlog).len();
    let running_count = app.tasks_in_column(&TaskStatus::Running).len();
    let blocked_count = app.tasks_in_column(&TaskStatus::Blocked).len();
    let done_count = app.tasks_in_column(&TaskStatus::Done).len();

    let mut spans = vec![
        Span::styled(
            " AGENTBOARD ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];

    // Show active repo filter
    if let Some(ref repo) = app.repo_filter {
        let repo_name = repo.rsplit('/').next().unwrap_or(repo);
        spans.push(Span::styled(
            format!(" {} ", repo_name),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " (f cycle, F clear)",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::raw("  "));
    }

    spans.extend([
        Span::styled(
            format!("▪ {} backlog", backlog_count),
            Style::default().fg(Color::Gray),
        ),
        Span::raw("  "),
        Span::styled(
            format!("● {} running", running_count),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  "),
        if blocked_count > 0 {
            Span::styled(
                format!("⚠ {} blocked", blocked_count),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
        if blocked_count > 0 {
            Span::raw("  ")
        } else {
            Span::raw("")
        },
        Span::styled(
            format!("✓ {} done", done_count),
            Style::default().fg(Color::Cyan),
        ),
    ]);

    let header = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(header, area);
}

// ---------------------------------------------------------------------------
// Kanban columns
// ---------------------------------------------------------------------------

fn render_columns(frame: &mut Frame, app: &App, area: Rect) {
    let constraints: Vec<Constraint> = app
        .columns
        .iter()
        .map(|_| Constraint::Ratio(1, app.columns.len() as u32))
        .collect();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, status) in app.columns.iter().enumerate() {
        let col_tasks = app.tasks_in_column(status);
        let count = col_tasks.len();
        let is_selected_col = i == app.selected_column;

        let header = format!(" {} ({}) ", status, count);

        let border_style = if is_selected_col {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(
                header,
                Style::default()
                    .fg(color_for_status(status))
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(chunks[i]);
        frame.render_widget(block, chunks[i]);

        let items: Vec<ListItem> = col_tasks
            .iter()
            .enumerate()
            .map(|(j, task)| {
                let short_id: String = task.id.chars().take(8).collect();
                let max_title_len = inner.width.saturating_sub(2) as usize;
                let title_display = truncate_title(&task.title, max_title_len);

                let is_selected = is_selected_col && j == app.selected_row;

                // Session alive indicator for running tasks
                let alive_indicator = if task.status == TaskStatus::Running {
                    match app.is_session_alive(task) {
                        Some(true) => " ●",
                        Some(false) => " ○",
                        None => "",
                    }
                } else if task.status == TaskStatus::Blocked {
                    " ⚠"
                } else {
                    ""
                };

                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(color_for_status(status))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let id_style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(color_for_status(status))
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let lines = vec![
                    Line::from(vec![
                        Span::styled(title_display, style),
                        Span::styled(
                            alive_indicator.to_string(),
                            Style::default().fg(Color::Green),
                        ),
                    ]),
                    Line::from(Span::styled(format!("[{}]", short_id), id_style)),
                ];

                ListItem::new(lines)
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, inner);
    }
}

// ---------------------------------------------------------------------------
// Detail sidebar
// ---------------------------------------------------------------------------

fn render_detail_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " Task Detail ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let task = match app.selected_task() {
        Some(t) => t,
        None => {
            let empty = Paragraph::new("No task selected.\n\nPress 'n' to create a new task.")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty, inner);
            return;
        }
    };

    // Split inner area: metadata (top) | live preview (bottom)
    let detail_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(0)])
        .split(inner);

    // -- Metadata section --
    let short_id: String = task.id.chars().take(8).collect();
    let alive = app.is_session_alive(task);
    let alive_str = match alive {
        Some(true) => "● alive",
        Some(false) => "○ dead",
        None => "",
    };

    let repo_display = shorten_path(
        &task.repo_path,
        detail_chunks[0].width.saturating_sub(8) as usize,
    );

    let mut meta_lines = vec![
        Line::from(vec![
            Span::styled("Title: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &task.title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("ID:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(&short_id, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("State: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                task.status.to_string(),
                Style::default()
                    .fg(color_for_status(&task.status))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(alive_str, Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled("Repo:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(repo_display, Style::default().fg(Color::White)),
        ]),
    ];

    if let Some(ref branch) = task.branch_name {
        meta_lines.push(Line::from(vec![
            Span::styled("Branch:", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(branch.as_str(), Style::default().fg(Color::Magenta)),
        ]));
    }

    if let Some(ref wt) = task.worktree_path {
        let wt_display = shorten_path(wt, detail_chunks[0].width.saturating_sub(8) as usize);
        meta_lines.push(Line::from(vec![
            Span::styled("Tree:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(wt_display, Style::default().fg(Color::White)),
        ]));
    }

    // Time info
    if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&task.created_at) {
        let local = created.with_timezone(&chrono::Local);
        meta_lines.push(Line::from(vec![
            Span::styled("Time:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                local.format("%Y-%m-%d %H:%M").to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    // Separator
    meta_lines.push(Line::from(Span::styled(
        "─".repeat(detail_chunks[0].width as usize),
        Style::default().fg(Color::DarkGray),
    )));

    let meta = Paragraph::new(meta_lines);
    frame.render_widget(meta, detail_chunks[0]);

    // -- Live preview section --
    let preview_block = Block::default().title(Span::styled(
        " Live Preview [/] scroll ",
        Style::default().fg(Color::DarkGray),
    ));

    let preview_inner = preview_block.inner(detail_chunks[1]);
    frame.render_widget(preview_block, detail_chunks[1]);

    if app.detail_lines.is_empty() {
        let no_output = if task.status == TaskStatus::Backlog {
            "Task not started yet."
        } else if task.tmux_session.is_none() {
            "No session attached."
        } else {
            "(no output captured)"
        };
        let p = Paragraph::new(no_output).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, preview_inner);
    } else {
        let preview = Paragraph::new(app.detail_lines.as_str())
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false });

        // The capture contains the newest session output, so the live view
        // should show its tail by default.  Calculate this after wrapping so
        // narrow sidebars and terminal resizes still land on the actual last
        // rendered line.
        let total_lines = preview.line_count(preview_inner.width);
        let max_scroll = total_lines
            .saturating_sub(preview_inner.height as usize)
            .min(u16::MAX as usize) as u16;
        let scroll = if app.detail_at_bottom {
            max_scroll
        } else {
            max_scroll.saturating_sub(app.detail_scroll)
        };
        let preview = preview.scroll((scroll, 0));
        frame.render_widget(preview, preview_inner);
    }
}

// ---------------------------------------------------------------------------
// Action bar — contextual actions for the selected task
// ---------------------------------------------------------------------------

fn render_action_bar(frame: &mut Frame, app: &App, area: Rect) {
    let actions = app.available_actions();

    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, (key, desc)) in actions.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(
            format!("[{}]", key),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {}", desc)));
    }

    let bar = Paragraph::new(Line::from(spans))
        .style(Style::default().fg(Color::White).bg(Color::DarkGray));
    frame.render_widget(bar, area);
}

// ---------------------------------------------------------------------------
// Status / notification bar
// ---------------------------------------------------------------------------

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let content = match &app.notification {
        Some(n) => Paragraph::new(format!(" {}", n.message)).style(n.style),
        None => {
            let filtered = app.filtered_tasks();
            let total = filtered.len();
            let running = filtered
                .iter()
                .filter(|t| t.status == TaskStatus::Running)
                .count();
            let filter_hint = if app.repo_filter.is_some() {
                format!(" (filtered, {} total)", app.tasks.len())
            } else {
                String::new()
            };
            Paragraph::new(format!(
                " {} tasks{}, {} running │ Auto-refresh: {}s │ f filter repo │ ? help",
                total,
                filter_hint,
                running,
                REFRESH_INTERVAL.as_secs()
            ))
            .style(Style::default().fg(Color::DarkGray))
        }
    };
    frame.render_widget(content, area);
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let overlay = centered_rect(60, 80, area);
    frame.render_widget(Clear, overlay);

    let help_text = vec![
        Line::from(Span::styled(
            " Agentboard Keybindings ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  NAVIGATION",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  h/l ←/→    ", Style::default().fg(Color::Cyan)),
            Span::raw("Move between columns"),
        ]),
        Line::from(vec![
            Span::styled("  j/k ↓/↑    ", Style::default().fg(Color::Cyan)),
            Span::raw("Move between tasks"),
        ]),
        Line::from(vec![
            Span::styled("  Tab/S-Tab   ", Style::default().fg(Color::Cyan)),
            Span::raw("Cycle columns"),
        ]),
        Line::from(vec![
            Span::styled("  [ / ]       ", Style::default().fg(Color::Cyan)),
            Span::raw("Scroll live preview up/down"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  TASK ACTIONS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  Enter       ", Style::default().fg(Color::Cyan)),
            Span::raw("Start (backlog) / Attach (running/blocked/done)"),
        ]),
        Line::from(vec![
            Span::styled("  s           ", Style::default().fg(Color::Cyan)),
            Span::raw("Start selected backlog task"),
        ]),
        Line::from(vec![
            Span::styled("  e           ", Style::default().fg(Color::Cyan)),
            Span::raw("Edit backlog task"),
        ]),
        Line::from(vec![
            Span::styled("  m           ", Style::default().fg(Color::Cyan)),
            Span::raw("Send message to running agent"),
        ]),
        Line::from(vec![
            Span::styled("  p           ", Style::default().fg(Color::Cyan)),
            Span::raw("Quick prompts (review, PR, test, …)"),
        ]),
        Line::from(vec![
            Span::styled("  K           ", Style::default().fg(Color::Cyan)),
            Span::raw("Kill running task"),
        ]),
        Line::from(vec![
            Span::styled("  D           ", Style::default().fg(Color::Cyan)),
            Span::raw("Mark task as done"),
        ]),
        Line::from(vec![
            Span::styled("  R           ", Style::default().fg(Color::Cyan)),
            Span::raw("Restart blocked/done task"),
        ]),
        Line::from(vec![
            Span::styled("  x           ", Style::default().fg(Color::Cyan)),
            Span::raw("Delete task (with confirmation)"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  GENERAL",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("  n           ", Style::default().fg(Color::Cyan)),
            Span::raw("Create new task"),
        ]),
        Line::from(vec![
            Span::styled("  r           ", Style::default().fg(Color::Cyan)),
            Span::raw("Refresh task list"),
        ]),
        Line::from(vec![
            Span::styled("  f           ", Style::default().fg(Color::Cyan)),
            Span::raw("Filter by repo (cycle)"),
        ]),
        Line::from(vec![
            Span::styled("  F           ", Style::default().fg(Color::Cyan)),
            Span::raw("Clear repo filter"),
        ]),
        Line::from(vec![
            Span::styled("  ?           ", Style::default().fg(Color::Cyan)),
            Span::raw("Toggle this help"),
        ]),
        Line::from(vec![
            Span::styled("  q / Esc     ", Style::default().fg(Color::Cyan)),
            Span::raw("Quit"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let help = Paragraph::new(help_text).block(block);
    frame.render_widget(help, overlay);
}

// ---------------------------------------------------------------------------
// New task form overlay
// ---------------------------------------------------------------------------

fn render_new_task_form(frame: &mut Frame, app: &App, area: Rect) {
    let overlay = centered_rect(60, 50, area);
    frame.render_widget(Clear, overlay);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " New Task (Tab: next field, Enter: create, Esc: cancel) ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    if let InputMode::NewTask {
        ref fields,
        active_field,
    } = app.input_mode
    {
        render_task_form_fields(frame, fields, active_field, inner);
    }
}

/// Word-wrap `text` to `width` columns.
///
/// Returns one entry per visual line: `(line_text, start_byte)` where
/// `start_byte` is the byte offset of the first character of that line within
/// `text`. Assumes single-column-width characters (matches the rest of the
/// form's char-count-based layout). Breaks on explicit `\n`, prefers breaking
/// at the last whitespace before `width`, and hard-breaks words longer than
/// `width`.
///
/// The cursor placement uses this exact same result, so the rendered text and
/// the cursor can never disagree about where a line breaks.
fn wrap_text(text: &str, width: usize) -> Vec<(String, usize)> {
    let mut lines: Vec<(String, usize)> = Vec::new();
    if width == 0 {
        lines.push((text.to_string(), 0));
        return lines;
    }

    let mut line = String::new();
    let mut line_start_byte = 0usize;
    let mut line_char_count = 0usize;
    // Byte offset + char index of the last whitespace seen in the current line.
    let mut last_ws: Option<(usize, usize)> = None;

    for (byte_idx, ch) in text.char_indices() {
        if ch == '\n' {
            lines.push((std::mem::take(&mut line), line_start_byte));
            line_start_byte = byte_idx + ch.len_utf8();
            line_char_count = 0;
            last_ws = None;
            continue;
        }

        if line_char_count == width {
            // Need to wrap. Prefer breaking at the last whitespace so we word-wrap.
            if let Some((ws_byte, ws_char_idx)) = last_ws {
                // Split the accumulated line at the whitespace. Characters after
                // the whitespace carry over to the next line.
                let split_at: usize = line
                    .char_indices()
                    .nth(ws_char_idx)
                    .map_or(line.len(), |(i, _)| i);
                let carry: String = line[split_at..]
                    .chars()
                    .skip(1) // drop the whitespace itself
                    .collect();
                line.truncate(split_at);
                lines.push((std::mem::take(&mut line), line_start_byte));
                // Next line starts right after the whitespace char.
                line_start_byte = ws_byte + 1;
                line = carry;
                line_char_count = line.chars().count();
                last_ws = None;
            } else {
                // No whitespace to break on: hard-break the long word.
                lines.push((std::mem::take(&mut line), line_start_byte));
                line_start_byte = byte_idx;
                line_char_count = 0;
                last_ws = None;
            }
        }

        if ch.is_whitespace() {
            last_ws = Some((byte_idx, line_char_count));
        }
        line.push(ch);
        line_char_count += 1;
    }

    lines.push((line, line_start_byte));
    lines
}

/// Map a byte offset within the field value to a `(row, col)` visual position,
/// given the wrapped lines produced by [`wrap_text`].
fn cursor_row_col(wrapped: &[(String, usize)], cursor_pos: usize, width: usize) -> (usize, usize) {
    // Find the last line whose start byte is <= cursor_pos.
    let mut row = 0usize;
    for (idx, (_, start_byte)) in wrapped.iter().enumerate() {
        if *start_byte <= cursor_pos {
            row = idx;
        } else {
            break;
        }
    }

    let (line_text, start_byte) = &wrapped[row];
    // Column = number of characters between the line start and the cursor.
    let col = if cursor_pos >= *start_byte {
        line_text
            .char_indices()
            .take_while(|(i, _)| start_byte + i < cursor_pos)
            .count()
    } else {
        0
    };

    // If the cursor sits exactly at width (just past the last visible column of
    // a full line that will wrap on the next char), clamp so it stays visible.
    let col = col.min(width);
    (row, col)
}

/// Shared field rendering for new-task and edit-task forms.
fn render_task_form_fields(
    frame: &mut Frame,
    fields: &[InputField],
    active_field: usize,
    inner: Rect,
) {
    // Title and Repo get a fixed single-line height; Details gets remaining space.
    let constraints = vec![
        Constraint::Length(3), // Title
        Constraint::Length(3), // Repo
        Constraint::Min(5),    // Details — taller, with word-wrap
        Constraint::Length(0), // absorb any leftover
    ];
    let field_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, field) in fields.iter().enumerate() {
        let is_active = i == active_field;
        let is_details = i == 2;

        let border_color = if is_active {
            Color::Yellow
        } else {
            Color::DarkGray
        };

        let field_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                format!(" {} ", field.label),
                Style::default().fg(if is_active {
                    Color::Yellow
                } else {
                    Color::White
                }),
            ));

        // Inner width available for text (subtract left+right border).
        let inner_width = field_chunks[i].width.saturating_sub(2) as usize;

        if is_details {
            // Multi-line field.  We wrap the text ourselves (instead of relying
            // on Paragraph's `Wrap`) so the cursor position uses the exact same
            // wrapping algorithm as the rendered text — otherwise the cursor
            // drifts by one column per wrapped line (word-wrap vs char-wrap
            // mismatch).
            let show_placeholder = field.value.is_empty() && !is_active;
            let style = if show_placeholder {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };

            if show_placeholder {
                let p = Paragraph::new(Span::styled(field.placeholder, style))
                    .block(field_block)
                    .wrap(Wrap { trim: false });
                frame.render_widget(p, field_chunks[i]);
            } else if inner_width > 0 {
                let wrapped = wrap_text(&field.value, inner_width);
                let lines: Vec<Line> = wrapped
                    .iter()
                    .map(|(text, _)| Line::from(Span::styled(text.clone(), style)))
                    .collect();
                let p = Paragraph::new(lines).block(field_block);
                frame.render_widget(p, field_chunks[i]);

                if is_active {
                    let (cursor_row, cursor_col) =
                        cursor_row_col(&wrapped, field.cursor_pos, inner_width);
                    #[allow(clippy::cast_possible_truncation)]
                    let cursor_x = field_chunks[i].x + 1 + cursor_col as u16;
                    #[allow(clippy::cast_possible_truncation)]
                    let cursor_y = field_chunks[i].y + 1 + cursor_row as u16;
                    frame.set_cursor_position((cursor_x, cursor_y));
                }
            } else {
                frame.render_widget(field_block, field_chunks[i]);
            }
        } else {
            // Single-line field with horizontal scroll.
            let display_value = if field.value.is_empty() && !is_active {
                Span::styled(field.placeholder, Style::default().fg(Color::DarkGray))
            } else {
                // Scroll so the cursor is always visible.  We work in char
                // indices (not byte offsets) so multi-byte characters don't
                // cause panics from slicing mid-codepoint.
                let cursor_char_idx = field.value[..field.cursor_pos].chars().count();
                let total_chars = field.value.chars().count();

                let scroll_chars = if cursor_char_idx <= inner_width {
                    0
                } else {
                    cursor_char_idx.saturating_sub(inner_width)
                };
                let end_chars = total_chars.min(scroll_chars + inner_width);

                // Convert char indices back to byte offsets for slicing.
                let scroll_byte = field
                    .value
                    .char_indices()
                    .nth(scroll_chars)
                    .map_or(field.value.len(), |(i, _)| i);
                let end_byte = field
                    .value
                    .char_indices()
                    .nth(end_chars)
                    .map_or(field.value.len(), |(i, _)| i);

                Span::styled(
                    &field.value[scroll_byte..end_byte],
                    Style::default().fg(Color::White),
                )
            };

            let p = Paragraph::new(Line::from(display_value)).block(field_block);
            frame.render_widget(p, field_chunks[i]);

            if is_active {
                // Cursor position relative to the visible window (in chars).
                let cursor_char_idx = field.value[..field.cursor_pos].chars().count();
                let scroll_chars = if cursor_char_idx <= inner_width {
                    0
                } else {
                    cursor_char_idx.saturating_sub(inner_width)
                };
                let visible_cursor = cursor_char_idx - scroll_chars;
                #[allow(clippy::cast_possible_truncation)]
                let cursor_x = field_chunks[i].x + 1 + visible_cursor as u16;
                let cursor_y = field_chunks[i].y + 1;
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Edit task form overlay (same layout as new task, different title/color)
// ---------------------------------------------------------------------------

fn render_edit_task_form(frame: &mut Frame, app: &App, area: Rect) {
    let overlay = centered_rect(60, 50, area);
    frame.render_widget(Clear, overlay);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Edit Task (Tab: next field, Enter: save, Esc: cancel) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    if let InputMode::EditTask {
        ref fields,
        active_field,
        ..
    } = app.input_mode
    {
        render_task_form_fields(frame, fields, active_field, inner);
    }
}

// ---------------------------------------------------------------------------
// Send message overlay
// ---------------------------------------------------------------------------

fn render_send_message(frame: &mut Frame, app: &App, area: Rect) {
    let overlay = centered_rect(70, 15, area);
    frame.render_widget(Clear, overlay);

    let task_label = match app.selected_task() {
        Some(t) => format!(" Send to {} (Enter: send, Esc: cancel) ", &t.id[..8]),
        None => " Send Message ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(Span::styled(
            task_label,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));

    if let InputMode::SendMessage {
        ref input,
        cursor_pos,
    } = app.input_mode
    {
        let p = Paragraph::new(input.as_str())
            .block(block)
            .style(Style::default().fg(Color::White));
        frame.render_widget(p, overlay);

        #[allow(clippy::cast_possible_truncation)]
        let cursor_x = overlay.x + 1 + cursor_pos as u16;
        let cursor_y = overlay.y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

// ---------------------------------------------------------------------------
// Confirm dialog overlay
// ---------------------------------------------------------------------------

fn render_confirm_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let overlay = centered_rect(50, 15, area);
    frame.render_widget(Clear, overlay);

    if let InputMode::Confirm { ref prompt, .. } = app.input_mode {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(Span::styled(
                " Confirm ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));

        let p = Paragraph::new(prompt.as_str())
            .block(block)
            .style(Style::default().fg(Color::White));
        frame.render_widget(p, overlay);
    }
}

// ---------------------------------------------------------------------------
// Quick prompts overlay
// ---------------------------------------------------------------------------

fn render_quick_prompts(frame: &mut Frame, app: &App, area: Rect) {
    let overlay = centered_rect(55, 65, area);
    frame.render_widget(Clear, overlay);

    let task_label = app
        .selected_task()
        .map(|t| format!(" → {} ", truncate_title(&t.title, 30)))
        .unwrap_or_default();

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            " Quick Prompts ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            task_label,
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    for qp in QUICK_PROMPTS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  [{}]  ", qp.key),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                qp.label,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        // Show a short preview of the prompt text
        let preview: String = qp.prompt.chars().take(70).collect();
        let preview = if qp.prompt.chars().count() > 70 {
            format!("{}…", preview.trim())
        } else {
            preview.trim().to_string()
        };
        lines.push(Line::from(Span::styled(
            format!("        {}", preview),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "  Esc to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(
            " ⚡ Quick Prompts ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));

    let p = Paragraph::new(lines).block(block);
    frame.render_widget(p, overlay);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn color_for_status(status: &TaskStatus) -> Color {
    match status {
        TaskStatus::Backlog => Color::Gray,
        TaskStatus::Running => Color::Green,
        TaskStatus::Blocked => Color::Yellow,
        TaskStatus::Done => Color::Cyan,
    }
}

/// Center a rectangle with the given percentage width and height inside `r`.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Truncate a title to max_len characters, adding ellipsis if needed.
pub fn truncate_title(title: &str, max_len: usize) -> String {
    let char_count = title.chars().count();
    if char_count <= max_len {
        title.to_string()
    } else {
        let truncated: String = title.chars().take(max_len.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

/// Shorten a path to fit within max_len characters.
///
/// Tries `~` substitution for the home directory first, then truncates from
/// the left with an `…` prefix. Uses char-based indexing throughout to avoid
/// panics on non-ASCII paths or very narrow terminals.
fn shorten_path(path: &str, max_len: usize) -> String {
    if max_len == 0 {
        return "…".to_string();
    }
    let char_count = path.chars().count();
    if char_count <= max_len {
        return path.to_string();
    }
    // Try to replace home dir with ~
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            let shortened = format!("~{}", rest);
            if shortened.chars().count() <= max_len {
                return shortened;
            }
        }
    }
    // Last resort: truncate from the left (char-safe)
    let skip = char_count - max_len + 1; // +1 for the … prefix
    let truncated: String = path.chars().skip(skip).collect();
    format!("…{}", truncated)
}

#[cfg(test)]
mod tests {
    use super::{cursor_row_col, wrap_text};

    /// Reconstruct a flat char list from wrapped lines by walking each line at
    /// its start byte. Used to sanity-check that cursor mapping lands on the
    /// right visual cell.
    fn char_at(wrapped: &[(String, usize)], row: usize, col: usize) -> Option<char> {
        wrapped.get(row).and_then(|(text, _)| text.chars().nth(col))
    }

    #[test]
    fn wrap_breaks_at_word_boundary_not_mid_word() {
        // width 10: "hello" (5) + " " + "world" (5) = 11 chars -> must wrap
        // before "world" rather than at column 10 mid-word.
        let w = wrap_text("hello world", 10);
        assert_eq!(w[0].0, "hello");
        assert_eq!(w[1].0, "world");
    }

    #[test]
    fn wrap_hard_breaks_long_word() {
        let w = wrap_text("aaaaaaaaaaaa", 5); // 12 a's, width 5
        assert_eq!(w[0].0, "aaaaa");
        assert_eq!(w[1].0, "aaaaa");
        assert_eq!(w[2].0, "aa");
    }

    #[test]
    fn cursor_tracks_char_across_wrapped_lines() {
        // This is the regression: on line 2+, cursor must not drift.
        // Includes a short first line ("aaa") caused by a long following word,
        // which is exactly what made the old formula drift.
        for text in ["the quick brown fox jumps", "aaa bbbbbbbbbb ccc dd eeeee"] {
            let width = 10;
            let wrapped = wrap_text(text, width);

            // For every cursor position that lands on a real (non-dropped) char,
            // the (row,col) must point at that exact character.
            for (byte_idx, ch) in text.char_indices() {
                if ch.is_whitespace() {
                    continue; // whitespace at wrap points is dropped from next line
                }
                let (row, col) = cursor_row_col(&wrapped, byte_idx, width);
                assert_eq!(
                    char_at(&wrapped, row, col),
                    Some(ch),
                    "text {text:?}: cursor at byte {byte_idx} (char '{ch}') mapped to wrong cell (row {row}, col {col})",
                );
            }
        }
    }

    #[test]
    fn cursor_at_end_is_after_last_char() {
        let text = "the quick brown fox jumps";
        let width = 10;
        let wrapped = wrap_text(text, width);
        let (row, col) = cursor_row_col(&wrapped, text.len(), width);
        // Cursor should be on the last line, one past the last visible char.
        assert_eq!(row, wrapped.len() - 1);
        assert_eq!(col, wrapped.last().unwrap().0.chars().count());
    }

    #[test]
    fn old_naive_formula_would_have_drifted() {
        // Demonstrates the bug the fix addresses: naive char/width math puts the
        // cursor at a different column than the word-wrapped render whenever a
        // line ends short (a word too long to fit gets pushed to the next line).
        let text = "aaa bbbbbbbbbb ccc";
        let width = 10;
        let wrapped = wrap_text(text, width);
        // Layout: ["aaa", "bbbbbbbbbb", " ccc"] — line 0 is short.

        let byte_idx = text.rfind('c').unwrap(); // last 'c'
        let (row, col) = cursor_row_col(&wrapped, byte_idx, width);

        let chars_before = text[..byte_idx].chars().count();
        let naive_row = chars_before / width;
        let naive_col = chars_before % width;

        // Correct mapping points at 'c'.
        assert_eq!(char_at(&wrapped, row, col), Some('c'));
        // Naive mapping disagrees (drift), proving the fix is necessary.
        assert!(
            (naive_row, naive_col) != (row, col),
            "expected naive formula to drift, but it matched: correct=({row},{col}) naive=({naive_row},{naive_col})",
        );
    }

    #[test]
    fn empty_and_single_char() {
        assert_eq!(wrap_text("", 10), vec![(String::new(), 0)]);
        assert_eq!(cursor_row_col(&wrap_text("", 10), 0, 10), (0, 0));

        let w = wrap_text("x", 10);
        assert_eq!(cursor_row_col(&w, 0, 10), (0, 0));
        assert_eq!(cursor_row_col(&w, 1, 10), (0, 1));
    }
}
