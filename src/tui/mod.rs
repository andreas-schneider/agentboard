//! TUI module — kanban board interface.
//!
//! Split into:
//! - `app` — application state, input modes, notifications, domain actions
//! - `render` — all ratatui widget rendering
//! - `keys` — keyboard input handling for each mode

mod app;
mod keys;
mod render;

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::store::TaskStore;
use app::{App, InputMode};

// ---------------------------------------------------------------------------
// Terminal setup / teardown
// ---------------------------------------------------------------------------

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    // Receive a clipboard paste as one Event::Paste instead of one Key event
    // per character. In particular, this keeps pasted newlines from being
    // mistaken for form submission.
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("failed to create terminal")?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )
    .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to show cursor")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub fn run_board(repo_filter: Option<String>) -> Result<()> {
    let store = TaskStore::open()?;

    // Restore orphaned tasks (Running in DB but tmux session dead — e.g. after reboot)
    let restore_result = crate::orchestrator::restore_orphaned_tasks(&store);

    let mut app = App::new(store, repo_filter)?;

    // Show restoration notification if any tasks were restored or failed
    match restore_result {
        Ok(ref result) if !result.restored.is_empty() || !result.failed.is_empty() => {
            let restored_count = result.restored.len();
            let failed_count = result.failed.len();

            // Register restored tasks in the grace period tracker so completion
            // detection doesn't fire immediately (agent hasn't launched yet).
            let now = std::time::Instant::now();
            for task_id in &result.restored {
                app.task_started_at.insert(task_id.clone(), now);
            }

            let msg = if failed_count == 0 {
                format!(
                    "🔄 Restored {} interrupted task{}",
                    restored_count,
                    if restored_count == 1 { "" } else { "s" }
                )
            } else if restored_count == 0 {
                format!(
                    "⚠ {} task{} could not be restored (moved to Blocked)",
                    failed_count,
                    if failed_count == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "🔄 Restored {} task{}, {} failed (moved to Blocked)",
                    restored_count,
                    if restored_count == 1 { "" } else { "s" },
                    failed_count,
                )
            };

            if failed_count > 0 && restored_count == 0 {
                app.notify(app::Notification::warn(msg));
            } else {
                app.notify(app::Notification::info(msg));
            }

            // Reload tasks after restoration changed statuses
            app.refresh_now()?;
        }
        Err(e) => {
            eprintln!("[restore] Failed to check for orphaned tasks: {}", e);
        }
        _ => {}
    }

    app.check_sessions();
    app.capture_detail();
    let mut terminal = setup_terminal()?;

    let result = run_event_loop(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    result
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        // Auto-refresh tasks, session status, detail pane
        let should_bell = app.auto_refresh()?;
        app.tick_notification();

        terminal
            .draw(|frame| render::ui(frame, app))
            .context("failed to draw frame")?;

        // Ring terminal bell after draw so the notification is visible
        if should_bell {
            // BEL character — triggers terminal bell / system notification
            execute!(terminal.backend_mut(), crossterm::style::Print("\x07")).ok();
        }

        if app.should_quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(200)).context("event poll failed")? {
            match event::read().context("event read failed")? {
                Event::Paste(text) => match &app.input_mode {
                    InputMode::NewTask { .. } => keys::handle_new_task_paste(app, &text),
                    InputMode::EditTask { .. } => keys::handle_edit_task_paste(app, &text),
                    _ => {}
                },
                Event::Key(key) if key.kind == KeyEventKind::Press => match &app.input_mode {
                    InputMode::Normal => {
                        keys::handle_normal_key(terminal, app, key.code)?;
                    }
                    InputMode::NewTask { .. } => {
                        keys::handle_new_task_key(app, key)?;
                    }
                    InputMode::EditTask { .. } => {
                        keys::handle_edit_task_key(app, key)?;
                    }
                    InputMode::SendMessage { .. } => {
                        keys::handle_send_message_key(app, key.code)?;
                    }
                    InputMode::Confirm { .. } => {
                        keys::handle_confirm_key(app, key.code)?;
                    }
                    InputMode::QuickPrompts => {
                        keys::handle_quick_prompt_key(app, key.code)?;
                    }
                },
                _ => {}
            }
        }
    }
}
