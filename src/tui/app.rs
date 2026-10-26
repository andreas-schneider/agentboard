//! Application state, input modes, and notifications for the TUI.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};

use crate::config;
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
        is_meta: bool,
        previous_working_directory: Option<String>,
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
    /// Configure visible board lanes and their order.
    ConfigureLanes {
        lanes: Vec<LaneSetting>,
        selected: usize,
    },
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
pub struct LaneSetting {
    pub status: TaskStatus,
    pub visible: bool,
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
    /// The tmux session that `detail_lines` was captured from. Used to drop
    /// stale captures delivered by the worker after the selection moved on.
    pub detail_session: Option<String>,
    /// Number of lines to scroll up from the bottom when the user has
    /// manually moved away from the live tail.
    pub detail_scroll: u16,
    pub detail_at_bottom: bool,

    // Auto-refresh
    pub last_refresh: Instant,
    pub last_detail_capture: Instant,

    // Session alive cache (checked periodically, not every frame)
    pub session_alive: std::collections::HashMap<String, bool>,

    // Background worker that performs all blocking tmux / process work off the
    // UI thread. `None` in unit tests, where no board loop is running.
    pub worker: Option<super::worker::WorkerHandle>,

    // Grace period: tracks when each task was started/restarted so we can
    // suppress premature completion detection.  Keyed by task ID.
    pub task_started_at: std::collections::HashMap<String, Instant>,

    // Notifications
    pub notification: Option<Notification>,

    // Input mode
    pub input_mode: InputMode,

    // Help overlay
    pub show_help: bool,

    // Whether the hidden "Keep" lane is currently shown as a column.
    pub show_keep: bool,

    // Working-directory filter: when Some, only matching tasks are shown.
    pub repo_filter: Option<String>,
}

impl App {
    pub fn new(store: TaskStore, repo_filter: Option<String>) -> Result<Self> {
        let tasks = store.list_tasks()?;
        let now = Instant::now();
        let columns = config::board_lanes(&config::load()?)?;
        let show_keep = columns.contains(&TaskStatus::Keep);
        Ok(Self {
            tasks,
            selected_column: 0,
            selected_row: 0,
            columns,
            should_quit: false,
            store,
            detail_lines: String::new(),
            detail_session: None,
            detail_scroll: 0,
            detail_at_bottom: true,
            last_refresh: now,
            last_detail_capture: now - REFRESH_INTERVAL, // force initial capture
            session_alive: std::collections::HashMap::new(),
            worker: None,
            task_started_at: std::collections::HashMap::new(),
            notification: None,
            input_mode: InputMode::Normal,
            show_help: false,
            show_keep,
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
        // changed when a task enters Running, Blocked, Review, or Done, so it also
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
            // completion first. Keep is a parked lane that sorts like Done so
            // the most recently kept idea stays at the top.
            TaskStatus::Running
            | TaskStatus::Blocked
            | TaskStatus::Review
            | TaskStatus::Done
            | TaskStatus::Keep => {
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

    /// Refresh the session-liveness map using a single `tmux list-sessions`
    /// call rather than one `has-session` spawn per task. Used on the
    /// synchronous path (the initial reconcile before the worker is spawned);
    /// once the worker is running it owns this map and pushes updates.
    pub fn check_sessions(&mut self) {
        let live: std::collections::HashSet<String> = tmux::list_sessions().into_iter().collect();
        self.session_alive.clear();
        for task in &self.tasks {
            if let Some(ref session) = task.tmux_session {
                self.session_alive
                    .insert(session.clone(), live.contains(session));
            }
        }
    }

    pub fn is_session_alive(&self, task: &Task) -> Option<bool> {
        task.tmux_session
            .as_ref()
            .and_then(|s| self.session_alive.get(s).copied())
    }

    // -- Worker plumbing ----------------------------------------------------

    /// Attach the background worker and prime it with the current selection.
    /// Called once by the event loop after the first synchronous reconcile.
    pub fn attach_worker(&mut self, worker: super::worker::WorkerHandle) {
        self.worker = Some(worker);
        self.send_grace_to_worker();
        self.request_detail_capture(true);
    }

    /// The tmux session of the currently-selected task, if any.
    fn selected_session(&self) -> Option<String> {
        self.selected_task().and_then(|t| t.tmux_session.clone())
    }

    /// Tell the worker which session to preview. When no worker is attached
    /// (unit tests, or the pre-loop reconcile), fall back to a synchronous
    /// capture so behaviour is unchanged.
    fn request_detail_capture(&mut self, force: bool) {
        self.detail_scroll = 0;
        self.detail_at_bottom = true;
        let session = self.selected_session();

        match &self.worker {
            Some(worker) => {
                if force {
                    worker.send(super::worker::WorkerRequest::RefreshNow {
                        session: session.clone(),
                    });
                } else {
                    worker.send(super::worker::WorkerRequest::Select {
                        session: session.clone(),
                    });
                }
                // Reflect the pending selection immediately so the preview
                // doesn't show the previous task while the capture is in
                // flight; the worker's Detail update fills in the content.
                if self.detail_session != session {
                    self.detail_lines = String::new();
                    self.detail_session = session;
                }
            }
            None => self.capture_detail_sync(),
        }
    }

    /// Push the current grace-period task-id set to the worker.
    fn send_grace_to_worker(&self) {
        if let Some(worker) = &self.worker {
            let now = Instant::now();
            let ids: std::collections::HashSet<String> = self
                .task_started_at
                .iter()
                .filter(|(_, started)| now.duration_since(**started) < TASK_START_GRACE_PERIOD)
                .map(|(id, _)| id.clone())
                .collect();
            worker.send(super::worker::WorkerRequest::Grace(ids));
        }
    }

    /// Drain cached results from the worker and apply them to UI state.
    /// Returns `true` if the terminal bell should ring.
    pub fn apply_worker_updates(&mut self) -> bool {
        let updates = match &self.worker {
            Some(worker) => worker.drain(),
            None => return false,
        };

        let mut bell = false;
        let mut tasks_changed = false;

        for update in updates {
            match update {
                super::worker::WorkerUpdate::Sessions(map) => {
                    self.session_alive = map;
                }
                super::worker::WorkerUpdate::Detail { session, text } => {
                    // Discard stale captures: only apply if this is still the
                    // selected session.
                    if session == self.selected_session() {
                        self.detail_lines = text;
                        self.detail_session = session;
                    }
                }
                super::worker::WorkerUpdate::TasksChanged {
                    notification,
                    bell: b,
                } => {
                    tasks_changed = true;
                    bell = bell || b;
                    if let Some((msg, success)) = notification {
                        self.notify(Notification::completion(msg, success));
                    }
                }
            }
        }

        if tasks_changed {
            // The worker changed task state in the store; reload our view.
            if let Ok(tasks) = self.store.list_tasks() {
                self.tasks = tasks;
                self.clamp_row();
            }
            // Clear grace entries for tasks that are no longer Running so the
            // worker stops considering them.
            let running: std::collections::HashSet<String> = self
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Running)
                .map(|t| t.id.clone())
                .collect();
            self.task_started_at.retain(|id, _| running.contains(id));
            self.send_grace_to_worker();
        }

        bell
    }

    // -- Auto-refresh -------------------------------------------------------

    /// Per-frame housekeeping. With the worker attached this does no blocking
    /// work — it only drains cached results and expires grace entries. The
    /// returned flag indicates whether the terminal bell should ring.
    pub fn auto_refresh(&mut self) -> Result<bool> {
        if self.worker.is_some() {
            // Expire stale grace entries and keep the worker's view current.
            let now = Instant::now();
            let before = self.task_started_at.len();
            self.task_started_at
                .retain(|_, started| now.duration_since(*started) < TASK_START_GRACE_PERIOD);
            if self.task_started_at.len() != before {
                self.send_grace_to_worker();
            }
            return Ok(self.apply_worker_updates());
        }

        // Fallback (no worker): original synchronous behaviour.
        let now = Instant::now();
        let mut bell = false;
        if now.duration_since(self.last_refresh) >= REFRESH_INTERVAL {
            bell = self.refresh_now_with_activity()?;
            self.last_refresh = now;
        }
        if now.duration_since(self.last_detail_capture) >= REFRESH_INTERVAL {
            self.capture_detail_sync();
            self.last_detail_capture = now;
        }
        Ok(bell)
    }

    /// Force an immediate refresh. Reloads tasks synchronously (cheap, DB
    /// only) and asks the worker to re-run its tmux cycle now. On the
    /// worker-less path it performs the full synchronous cycle.
    pub fn refresh_now(&mut self) -> Result<()> {
        if self.worker.is_some() {
            self.tasks = self.store.list_tasks()?;
            self.clamp_row();
            self.send_grace_to_worker();
            self.request_detail_capture(true);
            return Ok(());
        }
        self.refresh_now_with_activity().map(|_| ())
    }

    fn refresh_now_with_activity(&mut self) -> Result<bool> {
        self.tasks = self.store.list_tasks()?;
        self.clamp_row();
        self.check_sessions();
        let bell = self.detect_activity()?;
        self.capture_detail_sync();
        self.last_refresh = Instant::now();
        self.last_detail_capture = Instant::now();
        Ok(bell)
    }

    /// Detect agent completion or resumption using the currently loaded tasks.
    /// Synchronous path only (used when no worker is attached).
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
            .filter(|t| t.status != TaskStatus::Running)
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

    /// Request a refresh of the selected task's live preview. Cheap: with the
    /// worker attached this only sends a message; the actual `tmux` capture
    /// happens off-thread and is debounced across rapid navigation.
    pub fn capture_detail(&mut self) {
        self.request_detail_capture(false);
    }

    /// Synchronous `tmux capture-pane` for the selected task. Used only on the
    /// worker-less fallback path (initial reconcile, unit tests).
    fn capture_detail_sync(&mut self) {
        let session = self.selected_session();
        self.detail_lines = match self.selected_task() {
            Some(task) => {
                if let Some(ref s) = task.tmux_session {
                    if self.is_session_alive(task) != Some(false) {
                        tmux::capture_last_lines(s, DETAIL_CAPTURE_LINES)
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
        self.detail_session = session;
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
            Some(t)
                if t.status == TaskStatus::Blocked
                    || t.status == TaskStatus::Review
                    || t.status == TaskStatus::Done
                    || t.status == TaskStatus::Keep =>
            {
                t.clone()
            }
            Some(t) => {
                self.notify(Notification::warn(format!(
                    "Can only restart Blocked/Review/Done/Keep tasks (this is {})",
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

    /// Advance a task through the human review workflow.
    pub fn advance_task(&mut self) -> Result<()> {
        let task = match self.selected_task() {
            Some(t) => t.clone(),
            None => return Ok(()),
        };
        let skipped_review =
            task.status == TaskStatus::Blocked && !self.columns.contains(&TaskStatus::Review);
        let next_status = match task.status {
            TaskStatus::Blocked if skipped_review => TaskStatus::Done,
            TaskStatus::Blocked => TaskStatus::Review,
            TaskStatus::Review | TaskStatus::Running => TaskStatus::Done,
            ref status => {
                self.notify(Notification::warn(format!(
                    "Can only advance Blocked, Review, or Running tasks (this is {})",
                    status
                )));
                return Ok(());
            }
        };
        let updated = orchestrator::set_task_status(&self.store, &task, next_status.clone())?;
        self.refresh_now()?;
        let message = if skipped_review {
            format!(
                "Task {} moved directly to Done (Review hidden)",
                &updated.id[..8]
            )
        } else {
            format!("Task {} moved to {}", &updated.id[..8], next_status)
        };
        self.notify(Notification::info(message));
        Ok(())
    }

    /// Park the selected task in the hidden "Keep" lane. A kept task keeps its
    /// worktree and session but is set aside; it can be revealed and resumed
    /// later just like a Done task.
    pub fn keep_task(&mut self) -> Result<()> {
        let task = match self.selected_task() {
            Some(t) if t.status == TaskStatus::Keep => {
                self.notify(Notification::info("Task is already in Keep"));
                return Ok(());
            }
            Some(t) => t.clone(),
            None => return Ok(()),
        };
        self.store.update_status(&task.id, TaskStatus::Keep)?;
        self.refresh_now()?;
        let where_to = if self.show_keep {
            "moved to Keep"
        } else {
            "kept (press v to show the Keep lane)"
        };
        self.notify(Notification::info(format!(
            "Task {} {}",
            &task.id[..8],
            where_to
        )));
        Ok(())
    }

    /// Reveal every built-in lane for this board session. This is a recovery
    /// path for tasks in lanes hidden by the user's board configuration.
    pub fn show_all_lanes(&mut self) {
        self.columns = TaskStatus::all().to_vec();
        self.show_keep = true;
        self.selected_column = self.selected_column.min(self.columns.len() - 1);
        self.selected_row = 0;
        self.clamp_row();
        self.capture_detail();
    }

    pub fn open_lane_config(&mut self) {
        let lanes = TaskStatus::all()
            .iter()
            .map(|status| LaneSetting {
                status: status.clone(),
                visible: self.columns.contains(status),
            })
            .collect();
        self.input_mode = InputMode::ConfigureLanes { lanes, selected: 0 };
    }

    pub fn save_lane_config(&mut self, settings: &[LaneSetting]) -> Result<()> {
        let visible: Vec<TaskStatus> = settings
            .iter()
            .filter(|setting| setting.visible)
            .map(|setting| setting.status.clone())
            .collect();
        config::save_board_lanes(&visible)?;

        self.columns = visible;
        self.show_keep = self.columns.contains(&TaskStatus::Keep);
        self.selected_column = self
            .selected_column
            .min(self.columns.len().saturating_sub(1));
        self.selected_row = 0;
        self.clamp_row();
        self.capture_detail();
        self.input_mode = InputMode::Normal;
        self.notify(Notification::info("Board lanes saved"));
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
        is_meta: bool,
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
        let task = self
            .store
            .create_task_with_meta(title, desc, &repo, is_meta)?;
        self.refresh_now()?;
        self.notify(Notification::info(format!(
            "Created {} task: {} ({})",
            if is_meta { "meta" } else { "new" },
            task.title,
            &task.id[..8]
        )));
        Ok(())
    }

    pub fn toggle_new_task_meta(&mut self) -> Result<()> {
        if let InputMode::NewTask {
            ref mut fields,
            ref mut is_meta,
            ref mut previous_working_directory,
            ..
        } = self.input_mode
        {
            let directory = &mut fields[1];
            if *is_meta {
                directory.value = previous_working_directory.take().unwrap_or_default();
                directory.cursor_pos = directory.value.len();
                *is_meta = false;
            } else {
                *previous_working_directory = Some(directory.value.clone());
                directory.value = orchestrator::suggested_meta_workspace()?;
                directory.cursor_pos = directory.value.len();
                *is_meta = true;
            }
        }
        Ok(())
    }

    // -- Contextual actions for the selected task ---------------------------

    pub fn available_actions(&self) -> Vec<(&'static str, &'static str)> {
        match self.selected_task() {
            None => vec![
                ("n", "new task"),
                ("f", "filter"),
                ("v", "lanes"),
                ("?", "help"),
                ("q", "quit"),
            ],
            Some(task) => {
                let mut actions: Vec<(&str, &str)> = Vec::new();
                match task.status {
                    TaskStatus::Backlog => {
                        actions.push(("Enter/s", "start"));
                        actions.push(("e", "edit"));
                        actions.push(("k", "keep"));
                        actions.push(("x", "delete"));
                    }
                    TaskStatus::Running => {
                        actions.push(("Enter", "attach"));
                        actions.push(("m", "send msg"));
                        actions.push(("p", "prompts"));
                        actions.push(("K", "kill"));
                        actions.push(("D", "done"));
                        actions.push(("k", "keep"));
                    }
                    TaskStatus::Blocked => {
                        actions.push(("Enter", "attach"));
                        actions.push(("m", "send msg"));
                        actions.push(("p", "prompts"));
                        let description = if self.columns.contains(&TaskStatus::Review) {
                            "send to review"
                        } else {
                            "done (review hidden)"
                        };
                        actions.push(("D", description));
                        actions.push(("R", "restart"));
                        actions.push(("k", "keep"));
                        actions.push(("x", "delete"));
                    }
                    TaskStatus::Review => {
                        actions.push(("Enter", "attach"));
                        actions.push(("D", "approve / done"));
                        actions.push(("R", "restart"));
                        actions.push(("k", "keep"));
                        actions.push(("x", "delete"));
                    }
                    TaskStatus::Done => {
                        actions.push(("Enter", "attach"));
                        actions.push(("R", "restart"));
                        actions.push(("k", "keep"));
                        actions.push(("x", "delete"));
                    }
                    TaskStatus::Keep => {
                        actions.push(("Enter", "attach"));
                        actions.push(("R", "restart"));
                        actions.push(("x", "delete"));
                    }
                }
                actions.push(("v", "lanes"));
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
            is_meta: false,
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
        let keep_old = task(
            "keep-old",
            TaskStatus::Keep,
            "2026-01-01T00:00:00Z",
            "2026-01-04T00:00:00Z",
        );
        let keep_recent = task(
            "keep-recent",
            TaskStatus::Keep,
            "2026-01-02T00:00:00Z",
            "2026-01-07T00:00:00Z",
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
                keep_old,
                keep_recent,
            ],
            selected_column: 0,
            selected_row: 0,
            columns: vec![],
            should_quit: false,
            store: TaskStore::open_in_memory().unwrap(),
            detail_lines: String::new(),
            detail_session: None,
            detail_scroll: 0,
            detail_at_bottom: true,
            last_refresh: Instant::now(),
            last_detail_capture: Instant::now(),
            session_alive: Default::default(),
            worker: None,
            task_started_at: Default::default(),
            notification: None,
            input_mode: InputMode::Normal,
            show_help: false,
            show_keep: false,
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
        // Keep sorts like Done: most recently updated first.
        assert_eq!(ids(TaskStatus::Keep), ["keep-recent", "keep-old"]);
    }

    #[test]
    fn lane_config_lists_all_lanes_and_current_visibility() {
        let mut app = App::new(TaskStore::open_in_memory().unwrap(), None).unwrap();

        app.open_lane_config();
        let InputMode::ConfigureLanes { lanes, selected } = &app.input_mode else {
            panic!("expected lane configuration mode");
        };

        assert_eq!(*selected, 0);
        assert_eq!(lanes.len(), TaskStatus::all().len());
        assert_eq!(
            lanes
                .iter()
                .find(|lane| lane.status == TaskStatus::Done)
                .unwrap()
                .visible,
            app.columns.contains(&TaskStatus::Done)
        );
        assert_eq!(
            lanes
                .iter()
                .find(|lane| lane.status == TaskStatus::Keep)
                .unwrap()
                .visible,
            app.show_keep
        );

        let names: Vec<String> = lanes.iter().map(|lane| lane.status.to_string()).collect();
        assert_eq!(
            names,
            ["Backlog", "Running", "Blocked", "Review", "Done", "Keep"]
        );
    }

    #[test]
    fn show_all_lanes_reveals_hidden_lanes_and_clamps_selection() {
        let mut app = App::new(TaskStore::open_in_memory().unwrap(), None).unwrap();

        app.selected_column = app.columns.len() - 1;
        app.show_all_lanes();
        assert!(app.show_keep);
        assert_eq!(app.columns.last(), Some(&TaskStatus::Keep));
        assert!(app.selected_column < app.columns.len());
    }

    #[test]
    fn blocked_task_advances_to_review_when_review_is_visible() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create_task("Needs review", "", "/tmp/repo").unwrap();
        store.update_status(&task.id, TaskStatus::Blocked).unwrap();
        let mut app = App::new(store, None).unwrap();
        app.columns = vec![TaskStatus::Blocked, TaskStatus::Review, TaskStatus::Done];
        app.selected_column = 0;

        app.advance_task().unwrap();

        assert_eq!(
            app.store.get_task(&task.id).unwrap().status,
            TaskStatus::Review
        );
    }

    #[test]
    fn blocked_task_skips_hidden_review_lane() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create_task("Skip review", "", "/tmp/repo").unwrap();
        store.update_status(&task.id, TaskStatus::Blocked).unwrap();
        let mut app = App::new(store, None).unwrap();
        app.columns = vec![TaskStatus::Blocked, TaskStatus::Done];
        app.selected_column = 0;

        assert!(app
            .available_actions()
            .iter()
            .any(|(key, description)| *key == "D" && *description == "done (review hidden)"));
        app.advance_task().unwrap();

        assert_eq!(
            app.store.get_task(&task.id).unwrap().status,
            TaskStatus::Done
        );
    }

    #[test]
    fn keep_task_moves_selected_task_to_keep_lane() {
        let mut app = App::new(TaskStore::open_in_memory().unwrap(), None).unwrap();
        let created = app
            .store
            .create_task("Idea sketch", "revisit later", "/tmp/repo")
            .unwrap();
        app.refresh_now().unwrap();

        // Backlog column is first; select the new task and keep it.
        app.selected_column = 0;
        app.selected_row = 0;
        app.keep_task().unwrap();

        let fetched = app.store.get_task(&created.id).unwrap();
        assert_eq!(fetched.status, TaskStatus::Keep);
    }
}
