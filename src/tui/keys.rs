//! Keyboard input handling for each input mode.

use std::io::{self, Stdout};

use anyhow::{Context, Result};
use crossterm::event::KeyCode;
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::orchestrator;
use crate::prompts::QUICK_PROMPTS;
use crate::store::TaskStatus;
use crate::tmux;

use super::app::{App, InputField, InputMode, Notification, PendingAction};
use super::restore_terminal;

// ---------------------------------------------------------------------------
// Normal mode
// ---------------------------------------------------------------------------

pub fn handle_normal_key(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    code: KeyCode,
) -> Result<()> {
    // If help overlay is showing, any key closes it
    if app.show_help {
        app.show_help = false;
        return Ok(());
    }

    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.should_quit = true;
        }

        // Navigation
        KeyCode::Char('h') | KeyCode::Left => {
            if app.selected_column > 0 {
                app.selected_column -= 1;
                app.selected_row = 0;
                app.capture_detail();
            }
        }
        KeyCode::Char('l') | KeyCode::Right => {
            if app.selected_column < app.columns.len() - 1 {
                app.selected_column += 1;
                app.selected_row = 0;
                app.capture_detail();
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let count = app.current_column_tasks().len();
            if count > 0 && app.selected_row < count - 1 {
                app.selected_row += 1;
                app.capture_detail();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selected_row > 0 {
                app.selected_row -= 1;
                app.capture_detail();
            }
        }

        // Tab cycles columns forward, Shift+Tab backward (BackTab)
        KeyCode::Tab => {
            app.selected_column = (app.selected_column + 1) % app.columns.len();
            app.selected_row = 0;
            app.capture_detail();
        }
        KeyCode::BackTab => {
            app.selected_column = if app.selected_column == 0 {
                app.columns.len() - 1
            } else {
                app.selected_column - 1
            };
            app.selected_row = 0;
            app.capture_detail();
        }

        // Scroll detail panel
        KeyCode::Char(']') => {
            if !app.detail_at_bottom {
                app.detail_scroll = app.detail_scroll.saturating_sub(3);
                if app.detail_scroll == 0 {
                    app.detail_at_bottom = true;
                }
            }
        }
        KeyCode::Char('[') => {
            app.detail_at_bottom = false;
            app.detail_scroll = app.detail_scroll.saturating_add(3);
        }

        // Enter: context-dependent action
        KeyCode::Enter => {
            if let Some(task) = app.selected_task() {
                match task.status {
                    TaskStatus::Running | TaskStatus::Blocked | TaskStatus::Done => {
                        let session_alive = task
                            .tmux_session
                            .as_ref()
                            .is_some_and(|s| tmux::session_exists(s));

                        if session_alive {
                            let session = task.tmux_session.as_ref().unwrap().clone();
                            if let Some(ref worktree) = task.worktree_path {
                                tmux::ensure_task_windows(&session, worktree)?;
                            }
                            restore_terminal(terminal)?;
                            let _ = tmux::attach_session(&session);
                            enable_raw_mode().context("failed to re-enable raw mode")?;
                            execute!(io::stdout(), EnterAlternateScreen)
                                .context("failed to re-enter alternate screen")?;
                            terminal.clear().context("failed to clear terminal")?;
                            app.refresh_now()?;
                        } else {
                            // No live session — resume the configured harness and attach.
                            // Works for Running (orphaned), Blocked, and Done tasks.
                            let task_clone = task.clone();
                            // Starting the agent on a Done task means it's working
                            // again, so move it into Running — it will show up in the
                            // Running column and completion detection will move it to
                            // Blocked when the agent finishes. Running/Blocked keep
                            // their current status.
                            let desired_status = match task.status {
                                TaskStatus::Done => TaskStatus::Running,
                                ref other => other.clone(),
                            };
                            match orchestrator::resume_task(&app.store, &task_clone, desired_status)
                            {
                                Ok(updated) => {
                                    // Grace period prevents completion detection from
                                    // firing before the agent has launched.
                                    if updated.status == TaskStatus::Running {
                                        app.task_started_at
                                            .insert(updated.id.clone(), std::time::Instant::now());
                                    }
                                    app.refresh_now()?;
                                    if let Some(ref session) = updated.tmux_session {
                                        if tmux::session_exists(session) {
                                            let session = session.clone();
                                            if let Some(ref worktree) = updated.worktree_path {
                                                tmux::ensure_task_windows(&session, worktree)?;
                                            }
                                            restore_terminal(terminal)?;
                                            let _ = tmux::attach_session(&session);
                                            enable_raw_mode()
                                                .context("failed to re-enable raw mode")?;
                                            execute!(io::stdout(), EnterAlternateScreen)
                                                .context("failed to re-enter alternate screen")?;
                                            terminal.clear().context("failed to clear terminal")?;
                                            app.refresh_now()?;
                                        } else {
                                            app.notify(Notification::warn(
                                                "Session created but not alive",
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    app.notify(Notification::warn(format!("Resume failed: {}", e)));
                                }
                            }
                        }
                    }
                    TaskStatus::Backlog => {
                        app.start_task()?;
                    }
                }
            }
        }

        // Start task
        KeyCode::Char('s') => {
            app.start_task()?;
        }

        // Restart blocked task
        KeyCode::Char('R') => {
            app.restart_task()?;
        }

        // Refresh
        KeyCode::Char('r') => {
            app.refresh_now()?;
            app.notify(Notification::info("Refreshed"));
        }

        // New task (multi-field form)
        KeyCode::Char('n') => {
            let default_repo = match &app.repo_filter {
                Some(repo) => repo.clone(),
                None => std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
            };
            let default_repo_len = default_repo.len();
            app.input_mode = InputMode::NewTask {
                fields: vec![
                    InputField {
                        label: "Title",
                        value: String::new(),
                        placeholder: "Short task description",
                        cursor_pos: 0,
                    },
                    InputField {
                        label: "Repo",
                        value: default_repo,
                        placeholder: "/path/to/repo",
                        cursor_pos: default_repo_len,
                    },
                    InputField {
                        label: "Details",
                        value: String::new(),
                        placeholder: "Detailed prompt for the agent (optional)",
                        cursor_pos: 0,
                    },
                ],
                active_field: 0,
            };
        }

        // Send message to running agent
        KeyCode::Char('m') => {
            if let Some(task) = app.selected_task() {
                if task.status == TaskStatus::Running || task.status == TaskStatus::Blocked {
                    app.input_mode = InputMode::SendMessage {
                        input: String::new(),
                        cursor_pos: 0,
                    };
                } else {
                    app.notify(Notification::warn("Can only message Running/Blocked tasks"));
                }
            }
        }

        // Quick prompts palette
        KeyCode::Char('p') => {
            if let Some(task) = app.selected_task() {
                if task.status == TaskStatus::Running || task.status == TaskStatus::Blocked {
                    app.input_mode = InputMode::QuickPrompts;
                } else {
                    app.notify(Notification::warn(
                        "Can only send prompts to Running/Blocked tasks",
                    ));
                }
            }
        }

        // Kill task
        KeyCode::Char('K') => {
            if let Some(task) = app.selected_task() {
                if task.status == TaskStatus::Running {
                    let id = task.id.clone();
                    app.input_mode = InputMode::Confirm {
                        prompt: format!("Kill task {}? (y/n)", &id[..8]),
                        action: PendingAction::KillTask(id),
                    };
                } else {
                    app.notify(Notification::warn("Task is not running"));
                }
            }
        }

        // Mark done
        KeyCode::Char('D') => {
            app.mark_done()?;
        }

        // Edit backlog task
        KeyCode::Char('e') => {
            if let Some(task) = app.selected_task() {
                if task.status == TaskStatus::Backlog {
                    let task = task.clone();
                    let title_len = task.title.len();
                    let repo_len = task.repo_path.len();
                    let desc_len = task.description.len();
                    app.input_mode = InputMode::EditTask {
                        task_id: task.id.clone(),
                        fields: vec![
                            InputField {
                                label: "Title",
                                value: task.title.clone(),
                                placeholder: "Short task description",
                                cursor_pos: title_len,
                            },
                            InputField {
                                label: "Repo",
                                value: task.repo_path.clone(),
                                placeholder: "/path/to/repo",
                                cursor_pos: repo_len,
                            },
                            InputField {
                                label: "Details",
                                value: task.description.clone(),
                                placeholder: "Detailed prompt for the agent (optional)",
                                cursor_pos: desc_len,
                            },
                        ],
                        active_field: 0,
                    };
                } else {
                    app.notify(Notification::warn("Can only edit Backlog tasks"));
                }
            }
        }

        // Delete task
        KeyCode::Char('x') => {
            if let Some(task) = app.selected_task() {
                let id = task.id.clone();
                app.check_and_delete_task(&id)?;
            }
        }

        // Help overlay
        KeyCode::Char('?') => {
            app.show_help = true;
        }

        // Repo filter: cycle through repos
        KeyCode::Char('f') => {
            app.cycle_repo_filter();
            let msg = match &app.repo_filter {
                Some(repo) => {
                    let name = repo.rsplit('/').next().unwrap_or(repo);
                    format!("Filter: {}", name)
                }
                None => "Filter: all repos".to_string(),
            };
            app.notify(Notification::info(msg));
        }

        // Repo filter: clear
        KeyCode::Char('F') => {
            app.clear_repo_filter();
            app.notify(Notification::info("Filter cleared — showing all repos"));
        }

        _ => {}
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// New task form
// ---------------------------------------------------------------------------

pub fn handle_new_task_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Tab | KeyCode::Down => {
            if let InputMode::NewTask {
                ref fields,
                ref mut active_field,
            } = app.input_mode
            {
                *active_field = (*active_field + 1) % fields.len();
            }
        }
        KeyCode::BackTab | KeyCode::Up => {
            if let InputMode::NewTask {
                ref fields,
                ref mut active_field,
            } = app.input_mode
            {
                *active_field = if *active_field == 0 {
                    fields.len() - 1
                } else {
                    *active_field - 1
                };
            }
        }
        KeyCode::Enter => {
            // Extract field values and create task
            if let InputMode::NewTask { ref fields, .. } = app.input_mode {
                let title = fields[0].value.trim().to_string();
                let repo = fields[1].value.trim().to_string();
                let description = fields[2].value.trim().to_string();
                if !title.is_empty() {
                    app.input_mode = InputMode::Normal;
                    app.create_new_task(&title, &description, &repo)?;
                } else {
                    app.notify(Notification::warn("Title cannot be empty"));
                }
            }
        }
        _ => {
            if let InputMode::NewTask {
                ref mut fields,
                active_field,
            } = app.input_mode
            {
                handle_field_key(&mut fields[active_field], code);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Edit task form (same field navigation as new task, but saves to existing)
// ---------------------------------------------------------------------------

pub fn handle_edit_task_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Tab | KeyCode::Down => {
            if let InputMode::EditTask {
                ref fields,
                ref mut active_field,
                ..
            } = app.input_mode
            {
                *active_field = (*active_field + 1) % fields.len();
            }
        }
        KeyCode::BackTab | KeyCode::Up => {
            if let InputMode::EditTask {
                ref fields,
                ref mut active_field,
                ..
            } = app.input_mode
            {
                *active_field = if *active_field == 0 {
                    fields.len() - 1
                } else {
                    *active_field - 1
                };
            }
        }
        KeyCode::Enter => {
            if let InputMode::EditTask {
                ref task_id,
                ref fields,
                ..
            } = app.input_mode
            {
                let task_id = task_id.clone();
                let title = fields[0].value.trim().to_string();
                let repo = fields[1].value.trim().to_string();
                let description = fields[2].value.trim().to_string();
                if !title.is_empty() {
                    app.input_mode = InputMode::Normal;
                    app.edit_task(&task_id, &title, &description, &repo)?;
                } else {
                    app.notify(Notification::warn("Title cannot be empty"));
                }
            }
        }
        _ => {
            if let InputMode::EditTask {
                ref mut fields,
                active_field,
                ..
            } = app.input_mode
            {
                handle_field_key(&mut fields[active_field], code);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Send message
// ---------------------------------------------------------------------------

pub fn handle_send_message_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Enter => {
            if let InputMode::SendMessage { ref input, .. } = app.input_mode {
                let msg = input.trim().to_string();
                if !msg.is_empty() {
                    app.input_mode = InputMode::Normal;
                    app.send_message_to_agent(&msg)?;
                    return Ok(());
                }
            }
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Left => {
            if let InputMode::SendMessage {
                ref input,
                ref mut cursor_pos,
            } = app.input_mode
            {
                if *cursor_pos > 0 {
                    *cursor_pos = prev_char_boundary(input, *cursor_pos);
                }
            }
        }
        KeyCode::Right => {
            if let InputMode::SendMessage {
                ref input,
                ref mut cursor_pos,
            } = app.input_mode
            {
                if *cursor_pos < input.len() {
                    *cursor_pos = next_char_boundary(input, *cursor_pos);
                }
            }
        }
        KeyCode::Home => {
            if let InputMode::SendMessage {
                ref mut cursor_pos, ..
            } = app.input_mode
            {
                *cursor_pos = 0;
            }
        }
        KeyCode::End => {
            if let InputMode::SendMessage {
                ref input,
                ref mut cursor_pos,
            } = app.input_mode
            {
                *cursor_pos = input.len();
            }
        }
        KeyCode::Backspace => {
            if let InputMode::SendMessage {
                ref mut input,
                ref mut cursor_pos,
            } = app.input_mode
            {
                if *cursor_pos > 0 {
                    let prev = prev_char_boundary(input, *cursor_pos);
                    input.drain(prev..*cursor_pos);
                    *cursor_pos = prev;
                }
            }
        }
        KeyCode::Delete => {
            if let InputMode::SendMessage {
                ref mut input,
                ref mut cursor_pos,
            } = app.input_mode
            {
                if *cursor_pos < input.len() {
                    let next = next_char_boundary(input, *cursor_pos);
                    input.drain(*cursor_pos..next);
                }
            }
        }
        KeyCode::Char(c) => {
            if let InputMode::SendMessage {
                ref mut input,
                ref mut cursor_pos,
            } = app.input_mode
            {
                input.insert(*cursor_pos, c);
                *cursor_pos += c.len_utf8();
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Confirm dialog
// ---------------------------------------------------------------------------

pub fn handle_confirm_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let InputMode::Confirm { ref action, .. } = app.input_mode {
                let action = action.clone();
                app.input_mode = InputMode::Normal;
                match action {
                    PendingAction::DeleteTask(id) => app.delete_task(&id)?,
                    PendingAction::KillTask(id) => {
                        app.kill_task(&id)?;
                    }
                }
            }
        }
        _ => {
            // Any other key cancels
            app.input_mode = InputMode::Normal;
            app.notify(Notification::info("Cancelled"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Quick prompts palette
// ---------------------------------------------------------------------------

pub fn handle_quick_prompt_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => {
            app.input_mode = InputMode::Normal;
        }
        KeyCode::Char(c) if c.is_ascii_digit() || c.is_ascii_lowercase() => {
            if let Some(qp) = QUICK_PROMPTS.iter().find(|qp| qp.key == c) {
                app.input_mode = InputMode::Normal;
                app.send_message_to_agent(qp.prompt)?;
                app.notify(Notification::info(format!("⚡ Sent: {}", qp.label,)));
            }
        }
        _ => {
            app.input_mode = InputMode::Normal;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared field editing helpers
// ---------------------------------------------------------------------------

/// Handle a key event for a single InputField (cursor movement, insert, delete).
fn handle_field_key(field: &mut InputField, code: KeyCode) {
    match code {
        KeyCode::Left => {
            if field.cursor_pos > 0 {
                field.cursor_pos = prev_char_boundary(&field.value, field.cursor_pos);
            }
        }
        KeyCode::Right => {
            if field.cursor_pos < field.value.len() {
                field.cursor_pos = next_char_boundary(&field.value, field.cursor_pos);
            }
        }
        KeyCode::Home => {
            field.cursor_pos = 0;
        }
        KeyCode::End => {
            field.cursor_pos = field.value.len();
        }
        KeyCode::Backspace => {
            if field.cursor_pos > 0 {
                let prev = prev_char_boundary(&field.value, field.cursor_pos);
                field.value.drain(prev..field.cursor_pos);
                field.cursor_pos = prev;
            }
        }
        KeyCode::Delete => {
            if field.cursor_pos < field.value.len() {
                let next = next_char_boundary(&field.value, field.cursor_pos);
                field.value.drain(field.cursor_pos..next);
            }
        }
        KeyCode::Char(c) => {
            field.value.insert(field.cursor_pos, c);
            field.cursor_pos += c.len_utf8();
        }
        _ => {}
    }
}

/// Find the byte offset of the previous character boundary.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.saturating_sub(1);
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Find the byte offset of the next character boundary.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}
