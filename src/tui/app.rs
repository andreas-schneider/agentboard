//! Application state, input modes, and notifications for the TUI.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};

use crate::orchestrator;
use crate::store::{Task, TaskStatus, TaskStore};
use crate::tmux;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
pub const NOTIFICATION_DURATION: Duration = Duration::from_secs(4);
pub const COMPLETION_NOTIFICATION_DURATION: Duration = Duration::from_secs(10);
pub const DETAIL_CAPTURE_LINES: usize = 40;

/// Grace period after a task is started/restarted during which completion
/// detection is suppressed. This gives the agent CLI time to launch and
/// replace the shell process in the tmux pane — without this, the
/// "pane-at-shell" detector fires immediately because the shell hasn't been
/// replaced yet.
pub const TASK_START_GRACE_PERIOD: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Input mode — what the keyboard is currently driving
// ---------------------------------------------------------------------------

pub enum InputMode {
    /// Normal navigation and actions.
    Normal,
    /// Creating a new task: multi-field form.
    NewTask {
        fields: Vec<InputField>,
        active_field: usize,
    },
    /// Editing an existing backlog task: multi-field form.
    EditTask {
        task_id: String,
        fields: Vec<InputField>,
        active_field: usize,
    },
    /// Sending a message/command to a running agent.
    SendMessage { input: String, cursor_pos: usize },
    /// Confirming a destructive action (delete, kill).
    Confirm {
        prompt: String,
        action: PendingAction,
    },
    /// Quick prompt palette — press a number to send a pre-defined prompt.
    QuickPrompts,
}

#[derive(Clone)]
pub struct InputField {
    pub label: &'static str,
    pub value: String,
    pub placeholder: &'static str,
    /// Cursor position in the value string (byte offset, always on a char boundary).
    pub cursor_pos: usize,
}

#[derive(Clone)]
pub enum PendingAction {
    DeleteTask(String),
    KillTask(String),
}

// ---------------------------------------------------------------------------
// Notification
// ---------------------------------------------------------------------------

pub struct Notification {
    pub message: String,
    pub style: Style,
    pub expires: Instant,
}

impl Notification {
    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            style: Style::default().fg(Color::Green),
            expires: Instant::now() + NOTIFICATION_DURATION,
        }
    }

    pub fn warn(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            style: Style::default().fg(Color::Yellow),
            expires: Instant::now() + NOTIFICATION_DURATION,
        }
    }

    #[allow(dead_code)]
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            style: Style::default().fg(Color::Red),
            expires: Instant::now() + NOTIFICATION_DURATION,
        }
    }

    /// A prominent notification for agent completion events. Lasts longer
    /// and uses bold styling to catch the user's eye.
    pub fn completion(msg: impl Into<String>, success: bool) -> Self {
        Self {
            message: msg.into(),
            style: if success {
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            },
            expires: Instant::now() + COMPLETION_NOTIFICATION_DURATION,
        }
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() > self.expires
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    pub tasks: Vec<Task>,
    pub selected_column: usize,
    pub selected_row: usize,
    pub columns: Vec<TaskStatus>,
    pub should_quit: bool,
    pub store: TaskStore,

    // Detail sidebar
    pub detail_lines: String,
    /// Number of lines to scroll up from the bottom when the user has
    /// manually moved away from the live tail.
    pub detail_scroll: u16,
    pub detail_at_bottom: bool,

    // Auto-refresh
    pub last_refresh: Instant,
    pub last_detail_capture: Instant,

    // Session alive cache (checked periodically, not every frame)
    pub session_alive: std::collections::HashMap<String, bool>,

    // Grace period: tracks when each task was started/restarted so we can
    // suppress premature completion detection.  Keyed by task ID.
    pub task_started_at: std::collections::HashMap<String, Instant>,

    // Notifications
    pub notification: Option<Notification>,

    // Input mode
    pub input_mode: InputMode,

    // Help overlay
    pub show_help: bool,

    // Repo filter: when Some, only tasks matching this repo_path are shown
    pub repo_filter: Option<String>,
}

impl App {
    pub fn new(store: TaskStore, repo_filter: Option<String>) -> Result<Self> {
        let tasks = store.list_tasks()?;
        let now = Instant::now();
        Ok(Self {
            tasks,
            selected_column: 0,
            selected_row: 0,
            columns: vec![
                TaskStatus::Backlog,
                TaskStatus::Running,
                TaskStatus::Blocked,
                TaskStatus::Done,
            ],
            should_quit: false,
            store,
            detail_lines: String::new(),
            detail_scroll: 0,
            detail_at_bottom: true,
            last_refresh: now,
            last_detail_capture: now - REFRESH_INTERVAL, // force initial capture
            session_alive: std::collections::HashMap::new(),
            task_started_at: std::collections::HashMap::new(),
            notification: None,
            input_mode: InputMode::Normal,
            show_help: false,
            repo_filter,
        })
    }

    // -- Task queries -------------------------------------------------------

    pub fn tasks_in_column(&self, status: &TaskStatus) -> Vec<&Task> {
        let mut tasks: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| {
                t.status == *status && self.repo_filter.as_ref().is_none_or(|f| t.repo_path == *f)
            })
            .collect();

        // Each lane has an order that matches its purpose. `updated_at` is
        // changed when a task enters Running, Blocked, or Done, so it also
        // records when the task most recently became relevant in that lane.
        match status {
            // Keep long-waiting work visible instead of continually pushing it
            // down as new backlog items arrive.
            TaskStatus::Backlog => tasks.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            }),
            // Surface the task with the most recent activity, problem, or
            // completion first.
            TaskStatus::Running | TaskStatus::Blocked | TaskStatus::Done => {
                tasks.sort_by(|a, b| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| a.id.cmp(&b.id))
                });
            }
        }

        tasks
    }

    /// Filtered view of all tasks (respects repo_filter).
    pub fn filtered_tasks(&self) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| self.repo_filter.as_ref().is_none_or(|f| t.repo_path == *f))
            .collect()
    }

    /// Return sorted list of unique repo paths across all tasks.
    pub fn unique_repos(&self) -> Vec<String> {
        let mut repos: Vec<String> = self
            .tasks
            .iter()
            .map(|t| t.repo_path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        repos.sort();
        repos
    }

    /// Cycle repo_filter to the next repo (or first if currently None).
    pub fn cycle_repo_filter(&mut self) {
        let repos = self.unique_repos();
        if repos.is_empty() {
            return;
        }
        self.repo_filter = match &self.repo_filter {
            None => Some(repos[0].clone()),
            Some(current) => {
                match repos.iter().position(|r| r == current) {
                    Some(i) if i + 1 < repos.len() => Some(repos[i + 1].clone()),
                    // Wrap around: last repo → no filter
                    _ => None,
                }
            }
        };
        self.selected_row = 0;
        self.capture_detail();
    }

    /// Clear repo filter back to showing all tasks.
    pub fn clear_repo_filter(&mut self) {
        if self.repo_filter.is_some() {
            self.repo_filter = None;
            self.selected_row = 0;
            self.capture_detail();
        }
    }

    pub fn current_column_tasks(&self) -> Vec<&Task> {
        self.tasks_in_column(&self.columns[self.selected_column])
    }

    pub fn selected_task(&self) -> Option<&Task> {
        let col_tasks = self.current_column_tasks();
        col_tasks.get(self.selected_row).copied()
    }

    pub fn clamp_row(&mut self) {
        let count = self.current_column_tasks().len();
        if count == 0 {
            self.selected_row = 0;
        } else if self.selected_row >= count {
            self.selected_row = count - 1;
        }
    }

    // -- Notifications ------------------------------------------------------

    pub fn notify(&mut self, n: Notification) {
        self.notification = Some(n);
    }

    pub fn tick_notification(&mut self) {
        if let Some(ref n) = self.notification {
            if n.is_expired() {
                self.notification = None;
            }
        }
    }

    // -- Session alive checks -----------------------------------------------

    pub fn check_sessions(&mut self) {
        self.session_alive.clear();
        for task in &self.tasks {
            if let Some(ref session) = task.tmux_session {
                let alive = tmux::session_exists(session);
                self.session_alive.insert(session.clone(), alive);
            }
        }
    }

    pub fn is_session_alive(&self, task: &Task) -> Option<bool> {
        task.tmux_session
            .as_ref()
            .and_then(|s| self.session_alive.get(s).copied())
    }

    // -- Auto-refresh -------------------------------------------------------

    /// Periodic refresh: reload tasks, check sessions, detect completions.
    /// Returns `true` if the terminal bell should ring (agent completed).
    pub fn auto_refresh(&mut self) -> Result<bool> {
        let now = Instant::now();
        let mut bell = false;

        if now.duration_since(self.last_refresh) >= REFRESH_INTERVAL {
            bell = self.refresh_now_with_activity()?;
            self.last_refresh = now;
        }

        // Auto-capture detail for selected task
        if now.duration_since(self.last_detail_capture) >= REFRESH_INTERVAL {
            self.capture_detail();
            self.last_detail_capture = now;
        }

        Ok(bell)
    }

    pub fn refresh_now(&mut self) -> Result<()> {
        self.refresh_now_with_activity().map(|_| ())
    }

    fn refresh_now_with_activity(&mut self) -> Result<bool> {
        self.tasks = self.store.list_tasks()?;
        self.clamp_row();
        self.check_sessions();
        let bell = self.detect_activity()?;
        self.capture_detail();
        self.last_refresh = Instant::now();
        self.last_detail_capture = Instant::now();
        Ok(bell)
    }

    /// Detect agent completion or resumption using the currently loaded tasks.
    /// This is shared by periodic refreshes and explicit refreshes, including
    /// the refresh performed when returning from an attached tmux session.
    fn detect_activity(&mut self) -> Result<bool> {
        let now = Instant::now();

        let running: Vec<Task> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .cloned()
            .collect();

        let grace_task_ids: std::collections::HashSet<String> = self
            .task_started_at
            .iter()
            .filter(|(_, started)| now.duration_since(**started) < TASK_START_GRACE_PERIOD)
            .map(|(id, _)| id.clone())
            .collect();

        let events = orchestrator::check_completions(
            &self.store,
            &running,
            &self.session_alive,
            &grace_task_ids,
        );

        let resumable: Vec<Task> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Blocked || t.status == TaskStatus::Done)
            .cloned()
            .collect();

        let resumed =
            orchestrator::check_blocked_resumptions(&self.store, &resumable, &self.session_alive);

        if !events.is_empty() {
            for event in &events {
                self.task_started_at.remove(&event.task_id);
            }

            let event = &events[0];
            self.notify(Notification::completion(
                event.message(),
                event.is_success(),
            ));
        } else if !resumed.is_empty() {
            let short_id = &resumed[0][..resumed[0].len().min(8)];
            self.notify(Notification::info(format!(
                "Agent resumed working — {}",
                short_id,
            )));
        }

        if !events.is_empty() || !resumed.is_empty() {
            self.tasks = self.store.list_tasks()?;
            self.clamp_row();
            self.check_sessions();
        }

        Ok(!events.is_empty())
    }

    // -- Detail capture -----------------------------------------------------

    pub fn capture_detail(&mut self) {
        self.detail_lines = match self.selected_task() {
            Some(task) => {
                if let Some(ref session) = task.tmux_session {
                    if self.is_session_alive(task) != Some(false) {
                        tmux::capture_last_lines(session, DETAIL_CAPTURE_LINES)
                            .unwrap_or_else(|_| "(could not capture output)".to_string())
                    } else {
                        "(session ended)".to_string()
                    }
                } else {
                    String::new()
                }
            }
            None => String::new(),
        };
        self.detail_scroll = 0;
        self.detail_at_bottom = true;
    }

    // -- Actions (delegate to orchestrator) ---------------------------------

    pub fn start_task(&mut self) -> Result<()> {
        let task = match self.selected_task() {
            Some(t) if t.status == TaskStatus::Backlog => t.clone(),
            Some(t) => {
                self.notify(Notification::warn(format!(
                    "Can't start a {} task",
                    t.status
                )));
                return Ok(());
            }
            None => return Ok(()),
        };
        let updated = orchestrator::start_task(&self.store, &task)?;
        self.task_started_at
            .insert(updated.id.clone(), Instant::now());
        self.refresh_now()?;
        self.notify(Notification::info(format!(
            "Started → {} on branch {}",
            &updated.id[..8],
            updated.branch_name.as_deref().unwrap_or("?"),
        )));
        Ok(())
    }

    pub fn restart_task(&mut self) -> Result<()> {
        let task = match self.selected_task() {
            Some(t) if t.status == TaskStatus::Blocked || t.status == TaskStatus::Done => t.clone(),
            Some(t) => {
                self.notify(Notification::warn(format!(
                    "Can only restart Blocked/Done tasks (this is {})",
                    t.status
                )));
                return Ok(());
            }
            None => return Ok(()),
        };
        let updated = orchestrator::restart_task(&self.store, &task)?;
        self.task_started_at
            .insert(updated.id.clone(), Instant::now());
        self.refresh_now()?;
        self.notify(Notification::info(format!(
            "Restarted task {}",
            &updated.id[..8]
        )));
        Ok(())
    }

    pub fn kill_task(&mut self, id: &str) -> Result<()> {
        let task = match self.store.get_task(id) {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let updated = orchestrator::kill_task(&self.store, &task)?;
        self.refresh_now()?;
        self.notify(Notification::warn(format!(
            "Killed task {}",
            &updated.id[..8]
        )));
        Ok(())
    }

    pub fn mark_done(&mut self) -> Result<()> {
        let task = match self.selected_task() {
            Some(t) => t.clone(),
            None => return Ok(()),
        };
        let updated = orchestrator::done_task(&self.store, &task)?;
        self.refresh_now()?;
        self.notify(Notification::info(format!(
            "Task {} marked done",
            &updated.id[..8]
        )));
        Ok(())
    }

    pub fn delete_task(&mut self, id: &str) -> Result<()> {
        let task = self.store.get_task(id)?;
        let short = task.id[..8].to_string();
        orchestrator::delete_task(&self.store, &task)?;
        self.refresh_now()?;
        self.notify(Notification::warn(format!(
            "Deleted task {} (branch + worktree removed)",
            short
        )));
        Ok(())
    }

    pub fn check_and_delete_task(&mut self, id: &str) -> Result<()> {
        let task = self.store.get_task(id)?;
        let info = orchestrator::pre_delete_check(&task);

        if info.has_unmerged_work {
            // Show warning, user must confirm again
            let branch = task.branch_name.as_deref().unwrap_or("unknown");
            self.input_mode = InputMode::Confirm {
                prompt: format!(
                    "⚠ Branch '{}' has unmerged work! Delete anyway? (y/n)",
                    branch
                ),
                action: PendingAction::DeleteTask(id.to_string()),
            };
        } else {
            // No unmerged work, delete directly
            self.delete_task(id)?;
        }
        Ok(())
    }

    pub fn send_message_to_agent(&mut self, message: &str) -> Result<()> {
        let task = match self.selected_task() {
            Some(t) if t.status == TaskStatus::Running || t.status == TaskStatus::Blocked => {
                t.clone()
            }
            _ => {
                self.notify(Notification::warn(
                    "Select a Running or Blocked task to message",
                ));
                return Ok(());
            }
        };

        orchestrator::send_message(&self.store, &task, message)?;

        let truncated_msg = if message.chars().count() > 40 {
            let t: String = message.chars().take(39).collect();
            format!("{}…", t)
        } else {
            message.to_string()
        };
        self.notify(Notification::info(format!(
            "Sent to {}: {}",
            &task.id[..8],
            truncated_msg,
        )));

        if task.status == TaskStatus::Blocked {
            self.task_started_at.insert(task.id.clone(), Instant::now());
            self.refresh_now()?;
        }

        Ok(())
    }

    pub fn edit_task(
        &mut self,
        id: &str,
        title: &str,
        description: &str,
        repo_path: &str,
    ) -> Result<()> {
        let repo = if repo_path.is_empty() {
            std::env::current_dir()
                .context("failed to get cwd")?
                .to_string_lossy()
                .to_string()
        } else {
            repo_path.to_string()
        };
        let desc = if description.is_empty() {
            title
        } else {
            description
        };
        self.store.update_task_details(id, title, desc, &repo)?;
        self.refresh_now()?;
        self.notify(Notification::info(format!(
            "Updated task: {} ({})",
            title,
            &id[..id.len().min(8)]
        )));
        Ok(())
    }

    pub fn create_new_task(
        &mut self,
        title: &str,
        description: &str,
        repo_path: &str,
    ) -> Result<()> {
        let repo = if repo_path.is_empty() {
            std::env::current_dir()
                .context("failed to get cwd")?
                .to_string_lossy()
                .to_string()
        } else {
            repo_path.to_string()
        };
        let desc = if description.is_empty() {
            title
        } else {
            description
        };
        let task = self.store.create_task(title, desc, &repo)?;
        self.refresh_now()?;
        self.notify(Notification::info(format!(
            "Created task: {} ({})",
            task.title,
            &task.id[..8]
        )));
        Ok(())
    }

    // -- Contextual actions for the selected task ---------------------------

    pub fn available_actions(&self) -> Vec<(&'static str, &'static str)> {
        match self.selected_task() {
            None => vec![
                ("n", "new task"),
                ("f", "filter"),
                ("?", "help"),
                ("q", "quit"),
            ],
            Some(task) => {
                let mut actions: Vec<(&str, &str)> = Vec::new();
                match task.status {
                    TaskStatus::Backlog => {
                        actions.push(("Enter/s", "start"));
                        actions.push(("e", "edit"));
                        actions.push(("x", "delete"));
                    }
                    TaskStatus::Running => {
                        actions.push(("Enter", "attach"));
                        actions.push(("m", "send msg"));
                        actions.push(("p", "prompts"));
                        actions.push(("K", "kill"));
                        actions.push(("D", "done"));
                    }
                    TaskStatus::Blocked => {
                        actions.push(("Enter", "attach"));
                        actions.push(("m", "send msg"));
                        actions.push(("p", "prompts"));
                        actions.push(("D", "done"));
                        actions.push(("R", "restart"));
                        actions.push(("x", "delete"));
                    }
                    TaskStatus::Done => {
                        actions.push(("Enter", "attach"));
                        actions.push(("R", "restart"));
                        actions.push(("x", "delete"));
                    }
                }
                actions.push(("n", "new"));
                actions.push(("f", "filter"));
                actions.push(("?", "help"));
                actions.push(("q", "quit"));
                actions
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: TaskStatus, created_at: &str, updated_at: &str) -> Task {
        Task {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            status,
            repo_path: "/tmp/repo".to_string(),
            worktree_path: None,
            branch_name: None,
            tmux_session: None,
            agent_cli: None,
            created_at: created_at.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn tasks_are_sorted_for_their_lane_not_the_global_store_order() {
        let backlog_new = task(
            "backlog-new",
            TaskStatus::Backlog,
            "2026-01-03T00:00:00Z",
            "2026-01-03T00:00:00Z",
        );
        let backlog_old = task(
            "backlog-old",
            TaskStatus::Backlog,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        );
        let running_old = task(
            "running-old",
            TaskStatus::Running,
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
        );
        let running_recent = task(
            "running-recent",
            TaskStatus::Running,
            "2026-01-02T00:00:00Z",
            "2026-01-04T00:00:00Z",
        );
        let blocked_old = task(
            "blocked-old",
            TaskStatus::Blocked,
            "2026-01-01T00:00:00Z",
            "2026-01-03T00:00:00Z",
        );
        let blocked_recent = task(
            "blocked-recent",
            TaskStatus::Blocked,
            "2026-01-02T00:00:00Z",
            "2026-01-05T00:00:00Z",
        );
        let done_old = task(
            "done-old",
            TaskStatus::Done,
            "2026-01-01T00:00:00Z",
            "2026-01-04T00:00:00Z",
        );
        let done_recent = task(
            "done-recent",
            TaskStatus::Done,
            "2026-01-02T00:00:00Z",
            "2026-01-06T00:00:00Z",
        );

        let app = App {
            tasks: vec![
                backlog_new,
                backlog_old,
                running_old,
                running_recent,
                blocked_old,
                blocked_recent,
                done_old,
                done_recent,
            ],
            selected_column: 0,
            selected_row: 0,
            columns: vec![],
            should_quit: false,
            store: TaskStore::open_in_memory().unwrap(),
            detail_lines: String::new(),
            detail_scroll: 0,
            detail_at_bottom: true,
            last_refresh: Instant::now(),
            last_detail_capture: Instant::now(),
            session_alive: Default::default(),
            task_started_at: Default::default(),
            notification: None,
            input_mode: InputMode::Normal,
            show_help: false,
            repo_filter: None,
        };

        let ids = |status| {
            app.tasks_in_column(&status)
                .into_iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>()
        };

        assert_eq!(ids(TaskStatus::Backlog), ["backlog-old", "backlog-new"]);
        assert_eq!(ids(TaskStatus::Running), ["running-recent", "running-old"]);
        assert_eq!(ids(TaskStatus::Blocked), ["blocked-recent", "blocked-old"]);
        assert_eq!(ids(TaskStatus::Done), ["done-recent", "done-old"]);
    }
}
