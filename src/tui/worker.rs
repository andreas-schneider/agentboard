//! Background worker for all blocking tmux / process-tree work.
//!
//! The TUI event loop must never block on `tmux` subprocess spawns: a single
//! `capture-pane` or `has-session` fork/exec can take tens of milliseconds,
//! and the old design ran one per task on every 2-second refresh *and* one
//! `capture-pane` on every single navigation keypress. That made the cursor
//! and task switching feel sluggish whenever sessions were live.
//!
//! This module moves that work onto a dedicated thread. The UI thread only
//! sends lightweight messages (which session is selected, which tasks are in
//! their start grace period, an explicit refresh request) and drains cached
//! results without ever blocking. The worker:
//!
//! * polls session liveness with a single `tmux list-sessions` call (not one
//!   `has-session` per task),
//! * runs completion / resumption detection against the store,
//! * captures the selected task's live preview, debounced so rapid navigation
//!   coalesces into a single capture instead of one per keystroke.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::orchestrator;
use crate::store::{Task, TaskStatus, TaskStore};
use crate::tmux;

use super::app::{DETAIL_CAPTURE_LINES, REFRESH_INTERVAL};

/// Debounce window for detail capture after the selection changes. Rapid
/// navigation (holding `j`/`k`) coalesces into a single capture once the
/// selection has been stable for this long.
const DETAIL_DEBOUNCE: Duration = Duration::from_millis(120);

/// How often the worker wakes to check its timers when idle.
const POLL_SLICE: Duration = Duration::from_millis(60);

/// Messages from the UI thread to the worker.
pub enum WorkerRequest {
    /// The selected task changed; capture its session's preview (debounced).
    /// `None` means no task / no session selected — clear the preview.
    Select { session: Option<String> },
    /// Update the set of task ids currently in their start grace period.
    Grace(HashSet<String>),
    /// Force a full refresh cycle as soon as possible (e.g. after an action
    /// that changed task state, or returning from an attached session).
    RefreshNow { session: Option<String> },
    /// Shut the worker down (UI is quitting).
    Shutdown,
}

/// Cached results produced by the worker, consumed by the UI thread.
pub enum WorkerUpdate {
    /// Fresh session-liveness map. Keyed by tmux session name.
    Sessions(HashMap<String, bool>),
    /// Fresh live-preview text for the given session. `session` lets the UI
    /// discard a stale capture if the selection changed in the meantime.
    Detail {
        session: Option<String>,
        text: String,
    },
    /// The worker changed task state in the store (completions / resumptions).
    /// The UI should reload tasks from the store. Carries notification text
    /// and whether the terminal bell should ring.
    TasksChanged {
        notification: Option<(String, bool)>,
        bell: bool,
    },
}

/// Handle held by the UI thread.
pub struct WorkerHandle {
    tx: Sender<WorkerRequest>,
    rx: Receiver<WorkerUpdate>,
    join: Option<thread::JoinHandle<()>>,
}

impl WorkerHandle {
    /// Spawn the worker thread. The worker opens its own `TaskStore`
    /// connection (SQLite WAL mode supports this) so it never contends with
    /// the UI thread's connection for a lock beyond the configured busy
    /// timeout.
    pub fn spawn() -> WorkerHandle {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<WorkerRequest>();
        let (upd_tx, upd_rx) = std::sync::mpsc::channel::<WorkerUpdate>();

        let join = thread::Builder::new()
            .name("ab-tmux-worker".into())
            .spawn(move || worker_loop(req_rx, upd_tx))
            .expect("failed to spawn tmux worker thread");

        WorkerHandle {
            tx: req_tx,
            rx: upd_rx,
            join: Some(join),
        }
    }

    /// Non-blocking send of a request. Errors (worker gone) are ignored; the
    /// UI stays responsive regardless.
    pub fn send(&self, req: WorkerRequest) {
        let _ = self.tx.send(req);
    }

    /// Drain all currently-available updates without blocking.
    pub fn drain(&self) -> Vec<WorkerUpdate> {
        let mut out = Vec::new();
        while let Ok(update) = self.rx.try_recv() {
            out.push(update);
        }
        out
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(WorkerRequest::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Internal worker state.
struct Worker {
    store: TaskStore,
    upd_tx: Sender<WorkerUpdate>,

    selected_session: Option<String>,
    grace_task_ids: HashSet<String>,
    session_alive: HashMap<String, bool>,

    last_full_refresh: Instant,
    /// When the selection last changed; `None` once the pending capture has
    /// been served.
    detail_dirty_since: Option<Instant>,
}

fn worker_loop(req_rx: Receiver<WorkerRequest>, upd_tx: Sender<WorkerUpdate>) {
    let store = match TaskStore::open() {
        Ok(s) => s,
        Err(_) => {
            // Without a store the worker can do nothing useful; exit quietly.
            // The UI keeps running and simply receives no updates.
            return;
        }
    };

    let mut worker = Worker {
        store,
        upd_tx,
        selected_session: None,
        grace_task_ids: HashSet::new(),
        session_alive: HashMap::new(),
        // Force a full refresh on the first tick.
        last_full_refresh: Instant::now() - REFRESH_INTERVAL,
        detail_dirty_since: None,
    };

    loop {
        match req_rx.recv_timeout(POLL_SLICE) {
            Ok(WorkerRequest::Shutdown) => return,
            Ok(WorkerRequest::Select { session }) => {
                if worker.selected_session != session {
                    worker.selected_session = session;
                    worker.detail_dirty_since = Some(Instant::now());
                }
            }
            Ok(WorkerRequest::Grace(ids)) => {
                worker.grace_task_ids = ids;
            }
            Ok(WorkerRequest::RefreshNow { session }) => {
                worker.selected_session = session;
                worker.last_full_refresh = Instant::now() - REFRESH_INTERVAL;
                worker.detail_dirty_since = Some(Instant::now() - DETAIL_DEBOUNCE);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        worker.tick();
    }
}

impl Worker {
    fn tick(&mut self) {
        let now = Instant::now();

        // Full refresh: session liveness + completion/resumption detection.
        if now.duration_since(self.last_full_refresh) >= REFRESH_INTERVAL {
            self.full_refresh();
            self.last_full_refresh = Instant::now();
        }

        // Debounced detail capture for the selected session.
        if let Some(since) = self.detail_dirty_since {
            if now.duration_since(since) >= DETAIL_DEBOUNCE {
                self.capture_detail();
                self.detail_dirty_since = None;
            }
        }
    }

    /// Refresh session liveness with a single `list-sessions` call, then run
    /// completion / resumption detection.
    fn full_refresh(&mut self) {
        let tasks = match self.store.list_tasks() {
            Ok(t) => t,
            Err(_) => return,
        };

        self.refresh_sessions(&tasks);
        let _ = self
            .upd_tx
            .send(WorkerUpdate::Sessions(self.session_alive.clone()));

        self.detect_activity(&tasks);
    }

    /// Build the `session_alive` map from a single `tmux list-sessions` call
    /// instead of one `has-session` spawn per task.
    fn refresh_sessions(&mut self, tasks: &[Task]) {
        let live: HashSet<String> = tmux::list_sessions().into_iter().collect();
        let mut map = HashMap::new();
        for task in tasks {
            if let Some(ref session) = task.tmux_session {
                map.insert(session.clone(), live.contains(session));
            }
        }
        self.session_alive = map;
    }

    /// Completion / resumption detection, mirroring the previous
    /// `App::detect_activity` behaviour but on the worker thread.
    fn detect_activity(&mut self, tasks: &[Task]) {
        let running: Vec<Task> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .cloned()
            .collect();

        let events = orchestrator::check_completions(
            &self.store,
            &running,
            &self.session_alive,
            &self.grace_task_ids,
        );

        let resumable: Vec<Task> = tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Running)
            .cloned()
            .collect();

        let resumed =
            orchestrator::check_blocked_resumptions(&self.store, &resumable, &self.session_alive);

        if events.is_empty() && resumed.is_empty() {
            return;
        }

        let notification = if let Some(event) = events.first() {
            Some((event.message(), event.is_success()))
        } else {
            let short = &resumed[0][..resumed[0].len().min(8)];
            Some((format!("Agent resumed working — {}", short), true))
        };

        // State changed in the store; re-read sessions so the UI's liveness
        // view stays consistent with the new task statuses.
        if let Ok(updated) = self.store.list_tasks() {
            self.refresh_sessions(&updated);
            let _ = self
                .upd_tx
                .send(WorkerUpdate::Sessions(self.session_alive.clone()));
        }

        let _ = self.upd_tx.send(WorkerUpdate::TasksChanged {
            notification,
            bell: !events.is_empty(),
        });
    }

    /// Capture the live preview for the currently-selected session.
    fn capture_detail(&self) {
        let text = match &self.selected_session {
            Some(session) => {
                let alive = self.session_alive.get(session).copied();
                if alive == Some(false) {
                    "(session ended)".to_string()
                } else {
                    tmux::capture_last_lines(session, DETAIL_CAPTURE_LINES)
                        .unwrap_or_else(|_| "(could not capture output)".to_string())
                }
            }
            None => String::new(),
        };

        let _ = self.upd_tx.send(WorkerUpdate::Detail {
            session: self.selected_session.clone(),
            text,
        });
    }
}
