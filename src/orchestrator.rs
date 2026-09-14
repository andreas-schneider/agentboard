use crate::agent;
use crate::setup;
use crate::store::{Task, TaskStatus, TaskStore};
use crate::tmux;
use crate::worktree;
use anyhow::{Context, Result};

/// Resolve the harness that actually owns a task session.
///
/// A live process is authoritative, which also handles users manually
/// replacing the agent inside a tmux session. Persisted identity covers idle,
/// exited, and restored sessions. Legacy backlog tasks fall back to the agent
/// selected for this Agentboard invocation.
fn harness_for_task(store: &TaskStore, task: &Task) -> Box<dyn agent::AgentHarness> {
    if let Some(session) = &task.tmux_session {
        if let Ok(pid) = tmux::pane_pid(session) {
            if let Some(name) = tmux::detect_agent_descendant(pid, agent::SUPPORTED_AGENTS) {
                if task.agent_cli.as_deref() != Some(name) {
                    let _ = store.update_agent_cli(&task.id, name);
                }
                return agent::harness_for(name).expect("known agent must have a harness");
            }
        }
    }

    task.agent_cli
        .as_deref()
        .and_then(agent::harness_for)
        .unwrap_or_else(agent::default_harness)
}

// ---------------------------------------------------------------------------
// Completion detection
// ---------------------------------------------------------------------------

/// Describes why a running task was detected as complete.
#[derive(Debug, Clone)]
pub enum CompletionReason {
    /// The tmux session no longer exists.
    SessionDied,
    /// The agent finished and is sitting at its idle prompt.
    IdlePrompt,
    /// The agent process exited (detected via exit marker or process tree).
    AgentExited,
}

/// A single completion event emitted by [`check_completions`].
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: String,
    pub title: String,
    pub reason: CompletionReason,
}

impl CompletionEvent {
    /// Whether this represents a successful completion (agent finished) vs a
    /// failure (session died unexpectedly).
    pub fn is_success(&self) -> bool {
        !matches!(self.reason, CompletionReason::SessionDied)
    }

    /// Human-readable notification message.
    pub fn message(&self) -> String {
        let short_id = &self.task_id[..self.task_id.len().min(8)];
        let title = truncate(self.title.as_str(), 30);
        match self.reason {
            CompletionReason::SessionDied => {
                format!("🔔 Session died for task {} — {}", short_id, title)
            }
            CompletionReason::IdlePrompt => {
                format!("🔔 ⚠ Agent done, needs review — {} ({})", short_id, title)
            }
            CompletionReason::AgentExited => {
                format!("🔔 ⚠ Agent exited, needs review — {} ({})", short_id, title)
            }
        }
    }
}

/// Scan running tasks for agent completion.
///
/// Four-tier detection (checked in order per task):
///
///  1. **Session dead** — tmux session no longer exists → Blocked.
///  2. **Exit marker** — the pane contains the `AB_AGENT_EXIT:` marker,
///     meaning the wrapped command has finished.
///  3. **Idle-prompt detection** — capture the *visible* pane and look for
///     the agent's known idle-prompt pattern (e.g. kiro-cli's "ask a
///     question…").  If *active patterns* (e.g. "Kiro is working") are also
///     present in the visible pane, skip — the agent is still busy and the
///     idle text is just scrollback.
///  4. **Process tree fallback** — check whether the pane's root process
///     still has an agent descendant (e.g. `kiro-cli` or `codex`). If no agent
///     process is found, it has exited. This replaces the broken
///     `pane_current_command` check which always returns "bash" for some
///     agent terminal sessions.
///
/// For each detected completion, the task is moved to `Blocked` in the store
/// and a [`CompletionEvent`] is returned.
///
/// `grace_task_ids` is the set of task IDs that were recently started/restarted
/// and should skip Tier 2–4 detection (the agent CLI hasn't had time to
/// launch yet, so idle-prompt and process checks would false-positive).
/// Tier 1 (session dead) is always checked regardless of grace period.
pub fn check_completions(
    store: &TaskStore,
    running_tasks: &[Task],
    session_alive: &std::collections::HashMap<String, bool>,
    grace_task_ids: &std::collections::HashSet<String>,
) -> Vec<CompletionEvent> {
    let mut events = Vec::new();

    // DB-level grace period — a secondary safety net independent of
    // tui::app::TASK_START_GRACE_PERIOD (which tracks in-memory Instants).
    // This covers tasks started by *other* `ab` processes whose in-memory
    // tracker we don't have access to.  Intentionally longer than the TUI
    // constant to be conservative.
    const DB_GRACE_PERIOD_SECS: i64 = 30;

    let now_utc = chrono::Utc::now();

    for task in running_tasks {
        // Setup runs in the task's tmux session before the agent is launched.
        // A dependency install or build may legitimately have no agent process
        // for several minutes, so do not classify that period as completion.
        let execution_path = task.worktree_path.as_deref().unwrap_or(&task.repo_path);
        if std::path::Path::new(execution_path)
            .join(".agentboard/setup.running")
            .exists()
        {
            continue;
        }

        let harness = harness_for_task(store, task);
        let process_needle = harness.process_name();

        if let Some(ref session) = task.tmux_session {
            // Tier 1: Session dead entirely — always check, even during grace period.
            let alive = session_alive.get(session).copied().unwrap_or(true);
            if !alive {
                let _ = store.update_status(&task.id, TaskStatus::Blocked);
                events.push(CompletionEvent {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    reason: CompletionReason::SessionDied,
                });
                continue;
            }

            // Tier 2–4: Skip during grace period.
            if grace_task_ids.contains(&task.id) {
                continue;
            }

            if let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&task.updated_at) {
                let age = now_utc.signed_duration_since(updated_at);
                if age.num_seconds() < DB_GRACE_PERIOD_SECS {
                    continue;
                }
            }

            // Capture the visible pane content once — used by Tiers 2, 3.
            let visible = tmux::capture_visible_pane(session).unwrap_or_default();

            // Tier 2: Exit marker — the wrapped command printed AB_AGENT_EXIT:
            if visible.contains(agent::EXIT_MARKER) {
                let _ = store.update_status(&task.id, TaskStatus::Blocked);
                events.push(CompletionEvent {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    reason: CompletionReason::AgentExited,
                });
                continue;
            }

            // Tier 3: Idle-prompt detection with agent-specific state logic.
            //
            // Codex keeps its composer glyph visible while working, so each
            // harness decides how the visible pane represents an idle state.
            if harness.is_idle(&visible) {
                let _ = store.update_status(&task.id, TaskStatus::Blocked);
                events.push(CompletionEvent {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    reason: CompletionReason::IdlePrompt,
                });
                continue;
            }

            // Tier 4: Process tree fallback — check if the agent process is
            // still running as a descendant of the pane's root process.
            //
            // This replaces the broken is_pane_at_shell() check. kiro-cli-term
            // always shows "bash" as pane_current_command, so we instead walk
            // the process tree to find a kiro-cli descendant.
            if !process_needle.is_empty() {
                if let Ok(pid) = tmux::pane_pid(session) {
                    if !tmux::has_agent_descendant(pid, process_needle) {
                        let _ = store.update_status(&task.id, TaskStatus::Blocked);
                        events.push(CompletionEvent {
                            task_id: task.id.clone(),
                            title: task.title.clone(),
                            reason: CompletionReason::AgentExited,
                        });
                    }
                }
            }
        }
    }

    events
}

// ---------------------------------------------------------------------------
// Blocked → Running resumption detection
// ---------------------------------------------------------------------------

/// Check blocked (or done) tasks for signs that the agent is working again.
///
/// When a user attaches to a tmux session directly and gives the agent more
/// work (bypassing `send_message`), or presses Enter to start the agent on a
/// Done task, the task stays Blocked/Done because agentboard never saw the
/// interaction. This function detects that the agent is active again and moves
/// the task back to Running.
///
/// Returns the IDs of tasks that were resumed.
pub fn check_blocked_resumptions(
    store: &TaskStore,
    blocked_tasks: &[Task],
    session_alive: &std::collections::HashMap<String, bool>,
) -> Vec<String> {
    let mut resumed = Vec::new();

    for task in blocked_tasks {
        let harness = harness_for_task(store, task);
        if let Some(ref session) = task.tmux_session {
            // Only check tasks whose session is still alive.
            let alive = session_alive.get(session).copied().unwrap_or(false);
            if !alive {
                continue;
            }

            if let Ok(visible) = tmux::capture_visible_pane(session) {
                if harness.is_active(&visible) {
                    let _ = store.update_status(&task.id, TaskStatus::Running);
                    resumed.push(task.id.clone());
                }
            }
        }
    }

    resumed
}

// ---------------------------------------------------------------------------
// Session restoration after reboot / tmux server restart
// ---------------------------------------------------------------------------

/// Result of restoring orphaned tasks.
#[derive(Debug)]
pub struct RestoreResult {
    /// Tasks that were successfully restored (new tmux session + agent re-launched).
    pub restored: Vec<String>,
    /// Tasks that could not be restored and were moved to Blocked.
    pub failed: Vec<String>,
}

/// Detect and restore tasks that are marked Running but have no live tmux session.
///
/// This happens after a reboot or tmux server restart: the SQLite DB still says
/// Running, the worktree and branch are intact on disk, but the tmux session
/// (and the agent process inside it) is gone.
///
/// For each orphaned Running task this function:
///  1. Recreates the tmux session in the existing worktree directory
///  2. Enables continuous session logging (pipe-pane)
///  3. Injects saved scrollback from the previous session's log file
///  4. Re-launches the agent with a continuation prompt
///
/// Tasks where restoration fails (e.g., worktree deleted) are moved to Blocked.
pub fn restore_orphaned_tasks(store: &TaskStore) -> Result<RestoreResult> {
    let tasks = store.list_tasks()?;
    let mut result = RestoreResult {
        restored: Vec::new(),
        failed: Vec::new(),
    };

    for task in &tasks {
        // Only care about Running tasks
        if task.status != TaskStatus::Running {
            continue;
        }

        // Check if the tmux session is alive
        let session = match &task.tmux_session {
            Some(s) if !tmux::session_exists(s) => s.clone(),
            None => tmux::session_name(&task.id), // DB has no session name — try the conventional one
            _ => continue,                        // Session is alive, nothing to restore
        };

        // If the conventional session is alive, nothing to do
        if tmux::session_exists(&session) {
            continue;
        }

        // This task is orphaned — attempt restoration
        let short_id = &task.id[..task.id.len().min(8)];

        // Atomically claim this task for restoration. If another ab process
        // already claimed it, skip — they're handling it.
        match store.claim_for_restore(&task.id, &task.updated_at) {
            Ok(true) => {} // We won the claim
            Ok(false) => {
                // Another process claimed it or the task changed — skip
                continue;
            }
            Err(e) => {
                eprintln!(
                    "[restore] Failed to claim task {} for restore: {}",
                    short_id, e
                );
                continue;
            }
        }

        // Need a working directory
        let wt_path = match task.worktree_path.as_deref().unwrap_or(&task.repo_path) {
            path if std::path::Path::new(path).exists() => path.to_string(),
            _ => {
                // Worktree is gone — can't restore, move to Blocked
                eprintln!(
                    "[restore] Task {} has no valid worktree, moving to Blocked",
                    short_id
                );
                let _ = store.update_status(&task.id, TaskStatus::Blocked);
                result.failed.push(task.id.clone());
                continue;
            }
        };

        if task.is_meta {
            if let Err(e) = write_meta_context(&wt_path, task) {
                eprintln!(
                    "[restore] Failed to refresh meta context for {}; moving to Blocked: {}",
                    short_id, e
                );
                let _ = store.update_status(&task.id, TaskStatus::Blocked);
                result.failed.push(task.id.clone());
                continue;
            }
        }

        // 1. Create new tmux session
        if let Err(e) = tmux::create_session(&session, &wt_path) {
            eprintln!(
                "[restore] Failed to create tmux session for {}: {}",
                short_id, e
            );
            let _ = store.update_status(&task.id, TaskStatus::Blocked);
            result.failed.push(task.id.clone());
            continue;
        }

        // 2. Enable logging
        let log_path = tmux::session_log_path(&wt_path);
        if let Err(e) = tmux::enable_logging(&session, &log_path) {
            eprintln!("[restore] Failed to enable logging for {}: {}", short_id, e);
            // Non-fatal — continue with restoration
        }

        // 3. Inject scrollback from previous session
        if let Err(e) = tmux::inject_scrollback(&session, &log_path, 500) {
            eprintln!(
                "[restore] Failed to inject scrollback for {}: {}",
                short_id, e
            );
            // Non-fatal — continue without history
        }

        // 4. Re-launch the agent using the harness's resume behavior
        let harness = harness_for_task(store, task);
        let prompt = build_prompt(task);
        let cmd = harness.resume_command(&prompt);
        if let Err(e) = tmux::send_command(&session, &cmd) {
            eprintln!("[restore] Failed to spawn agent for {}: {}", short_id, e);
            let _ = tmux::kill_session(&session);
            let _ = store.update_status(&task.id, TaskStatus::Blocked);
            result.failed.push(task.id.clone());
            continue;
        }

        // 5. Update the task in the DB (ensure session name is correct)
        let mut updated = task.clone();
        updated.tmux_session = Some(session.clone());
        updated.agent_cli = Some(harness.name().to_owned());
        if let Err(e) = store.update_task(&updated) {
            eprintln!(
                "[restore] Failed to update DB for {}: {} — killing restored session",
                short_id, e
            );
            let _ = tmux::kill_session(&session);
            result.failed.push(task.id.clone());
            continue;
        }

        result.restored.push(task.id.clone());
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Task lifecycle operations
// ---------------------------------------------------------------------------

/// Return a suggested, isolated working directory for a new meta task.
pub fn suggested_meta_workspace() -> Result<String> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let path = home
        .join(".agentboard")
        .join("meta-workspaces")
        .join(uuid::Uuid::new_v4().to_string());
    Ok(path.to_string_lossy().to_string())
}

fn write_meta_context(workspace: &str, task: &Task) -> Result<()> {
    let context_dir = std::path::Path::new(workspace).join(".agentboard");
    std::fs::create_dir_all(&context_dir).context("failed to create meta context directory")?;
    let context_path = context_dir.join("meta-task-context.md");
    let contents = format!(
        "# Agentboard meta-task context\n\n\
This is task `{}`: **{}**.\n\n\
This task is about the Agentboard board and its tasks. Its working directory may be a regular folder or, when you selected a Git directory, a task worktree.\n\n\
This workspace is for board operations, not the Agentboard source repository. Do not assume the source checkout is the current directory; use the `ab` CLI and the repository path shown by `ab show` when a task explicitly requires source changes.\n\n\
## CLI\n\n\
Use the non-interactive `ab` CLI to inspect and operate on the board. Do not run `ab board`: its TUI is for human operators.\n\n\
* `ab list` — list tasks and statuses.\n\
* `ab show <id>` — inspect a task and recent output.\n\
* `ab new \"title\" [-d \"description\"] [--dir <path>] [--meta]` — create a task.\n\
* `ab start <id>` — start a backlog task.\n\
* `ab attach <id>` — attach to another task's live session.\n\
* `ab edit <id> [-t \"title\"] [-d \"description\"] [--dir <path>]` — edit a backlog task.\n\
* `ab kill <id>` — stop a running task and mark it Blocked.\n\
* `ab done <id>` — mark a task Done.\n\
* `ab config show` / `ab config check` — inspect or validate configuration.\n\n\
Task IDs accept unique prefixes. Statuses are Backlog, Running, Blocked, and Done.\n\n\
## Operating rules\n\n\
Inspect a task with `ab show` before changing it. Preserve working-directory paths when creating or editing tasks. Do not delete tasks, kill sessions, modify Agentboard configuration, or bulk-change tasks unless the user explicitly asks. Do not mark a task Done merely because it was inspected; mark it Done only when its requested work is complete.\n\n\
The current agent session is already owned by this task. It runs in tmux with an `agent` window and a `shell` window. You can inspect the session with `tmux display-message -p '#S'`, `tmux list-windows`, or `tmux capture-pane -p -S -100`. Do not kill or detach the current session unless explicitly asked.\n\n\
The authoritative command reference is `ab --help` and `ab <command> --help`; see Agentboard's `docs/CLI.md` when its source checkout is available.\n\n\
## User request\n\n{}\n",
        task.id, task.title, task.description
    );
    std::fs::write(&context_path, contents).with_context(|| {
        format!(
            "failed to write meta task context {}",
            context_path.display()
        )
    })?;
    Ok(())
}

/// Build the prompt string for the agent from a task's title and description.
///
/// The prompt must be a single line because it is sent to the agent via
/// `tmux send-keys -l`, where literal newlines would be interpreted as Enter
/// keypresses — splitting the command across multiple shell invocations.
pub fn build_prompt(task: &Task) -> String {
    if task.is_meta {
        return format!(
            "This is an Agentboard meta task. Read .agentboard/meta-task-context.md first. Use the `ab` CLI to work on the board, then handle this request: {}",
            sanitize_prompt(&task_prompt(task))
        );
    }
    task_prompt(task)
}

fn task_prompt(task: &Task) -> String {
    if task.description.is_empty() || task.description == task.title {
        sanitize_prompt(&task.title)
    } else {
        sanitize_prompt(&format!("{} — {}", task.title, task.description))
    }
}

/// Collapse any line terminators (and surrounding whitespace) into single spaces
/// so the prompt is safe to send through tmux send-keys.
fn sanitize_prompt(s: &str) -> String {
    // Normalize bare \r to \n so .lines() (which handles \n and \r\n) catches everything.
    let normalized = s.replace('\r', "\n");
    normalized
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Start a task: create worktree, tmux session, spawn agent.
/// On partial failure, cleans up any resources that were created.
/// Returns the updated task (with status=Running and all fields set).
pub fn start_task(store: &TaskStore, task: &Task) -> Result<Task> {
    start_task_with_options(store, task, false)
}

/// Resolve the directory for restarting or resuming a task.
fn restart_execution_path(task: &Task) -> Result<(String, bool)> {
    if let Some(worktree) = task
        .worktree_path
        .as_ref()
        .filter(|path| std::path::Path::new(path).exists())
    {
        return Ok((worktree.clone(), true));
    }

    if task.is_meta && !std::path::Path::new(&task.repo_path).exists() {
        std::fs::create_dir_all(&task.repo_path)
            .with_context(|| format!("failed to create meta workspace {}", task.repo_path))?;
    }

    if worktree::is_git_repository(&task.repo_path)? {
        let branch = task
            .branch_name
            .clone()
            .unwrap_or_else(|| worktree::generate_branch_name(&task.id, &task.title));
        Ok((
            worktree::create_worktree(&task.repo_path, &task.id, &branch)?,
            true,
        ))
    } else {
        Ok((task.repo_path.clone(), false))
    }
}

/// Start a task, optionally skipping repository setup.
pub fn start_task_with_options(store: &TaskStore, task: &Task, skip_setup: bool) -> Result<Task> {
    if task.is_meta && !std::path::Path::new(&task.repo_path).exists() {
        std::fs::create_dir_all(&task.repo_path)
            .with_context(|| format!("failed to create meta workspace {}", task.repo_path))?;
    }

    // Resolve configuration before creating anything, so a malformed config
    // cannot leave behind a partially initialized worktree or session. Setup
    // applies to direct workspaces too; they are not exempt from project setup.
    let setup_config = if skip_setup {
        None
    } else {
        setup::resolve(&task.repo_path)?
    };

    let (execution_path, branch) = if worktree::is_git_repository(&task.repo_path)? {
        let branch = worktree::generate_branch_name(&task.id, &task.title);
        let wt_path = worktree::create_worktree(&task.repo_path, &task.id, &branch)?;
        (wt_path, Some(branch))
    } else {
        (task.repo_path.clone(), None)
    };

    if task.is_meta {
        write_meta_context(&execution_path, task)?;
    }

    // If tmux session creation fails, clean up the worktree
    let session = tmux::session_name(&task.id);
    if let Err(e) = tmux::create_session(&session, &execution_path) {
        if branch.is_some() {
            let _ = worktree::cleanup_worktree(&task.repo_path, &execution_path);
        }
        return Err(e.context("failed to create tmux session (worktree cleaned up)"));
    }

    // If agent spawn fails, kill the tmux session and clean up worktree
    let harness = agent::default_harness();
    let prompt = build_prompt(task);
    let agent_cmd = harness.spawn_command(&prompt);
    let cmd = if skip_setup {
        agent_cmd
    } else if let Some(config) = setup_config {
        setup::command(&config, &agent_cmd)
    } else {
        agent_cmd
    };

    // Enable logging before setup starts so dependency/build output is
    // available in the session log as well as the live preview.
    let log_path = tmux::session_log_path(&execution_path);
    if let Err(e) = tmux::enable_logging(&session, &log_path) {
        eprintln!("[start] Failed to enable session logging: {}", e);
    }

    if let Err(e) = tmux::send_command(&session, &cmd) {
        let _ = tmux::kill_session(&session);
        if branch.is_some() {
            let _ = worktree::cleanup_worktree(&task.repo_path, &execution_path);
        }
        return Err(e.context("failed to spawn agent (session and worktree cleaned up)"));
    }

    // Update task in DB
    let mut updated = task.clone();
    updated.status = TaskStatus::Running;
    updated.worktree_path = if branch.is_some() {
        Some(execution_path)
    } else {
        None
    };
    updated.branch_name = branch;
    updated.tmux_session = Some(session.clone());
    updated.agent_cli = Some(harness.name().to_owned());
    if let Err(e) = store.update_task(&updated) {
        // The agent is already running, so leave no live session or newly
        // created worktree behind if persistence fails. Direct workspaces are
        // user-owned and intentionally remain untouched.
        let _ = tmux::kill_session(&session);
        if let Some(ref worktree_path) = updated.worktree_path {
            let _ = worktree::cleanup_worktree(&task.repo_path, worktree_path);
        }
        return Err(e.context("failed to persist started task (session and worktree cleaned up)"));
    }

    Ok(updated)
}

/// Restart a task with a fresh agent session using the original task prompt.
///
/// This is the "start over" escape hatch (R key). Kills the old tmux session,
/// creates a new one, and launches the agent with `spawn_command` — a brand
/// new conversation, not a resume. Use when the agent was stuck or confused
/// and resuming wouldn't help.
pub fn restart_task(store: &TaskStore, task: &Task) -> Result<Task> {
    // Clean up old session if still around
    if let Some(ref session) = task.tmux_session {
        let _ = tmux::kill_session(session);
    }

    let (wt_path, uses_worktree) = restart_execution_path(task)?;
    if task.is_meta {
        write_meta_context(&wt_path, task)?;
    }

    let session = tmux::session_name(&task.id);
    if let Err(e) = tmux::create_session(&session, &wt_path) {
        // Don't clean up worktree on restart failure — it may have user's work
        return Err(e.context("failed to create tmux session"));
    }

    let harness = harness_for_task(store, task);
    let prompt = build_prompt(task);
    let cmd = harness.spawn_command(&prompt);
    if let Err(e) = tmux::send_command(&session, &cmd) {
        let _ = tmux::kill_session(&session);
        return Err(e.context("failed to spawn agent (session cleaned up)"));
    }

    // Enable continuous session logging for restoration after reboot
    let log_path = tmux::session_log_path(&wt_path);
    if let Err(e) = tmux::enable_logging(&session, &log_path) {
        eprintln!("[restart] Failed to enable session logging: {}", e);
        // Non-fatal — task still runs, just won't have scrollback on restore
    }

    let mut updated = task.clone();
    updated.status = TaskStatus::Running;
    updated.worktree_path = uses_worktree.then_some(wt_path);
    updated.tmux_session = Some(session);
    updated.agent_cli = Some(harness.name().to_owned());
    store.update_task(&updated)?;

    Ok(updated)
}

/// Resume a task by creating a tmux session and launching the agent with the
/// harness's resume behavior to continue the previous conversation.
///
/// Used when the user presses Enter on a Blocked/Done/Running task whose
/// tmux session is dead. Harnesses that support persisted conversations can
/// resume them; others start again with the original prompt.
///
/// The caller controls the task's status via `desired_status` — Blocked
/// tasks stay Blocked (user decides next steps), Done tasks stay Done.
pub fn resume_task(store: &TaskStore, task: &Task, desired_status: TaskStatus) -> Result<Task> {
    // Clean up old session if still around
    if let Some(ref session) = task.tmux_session {
        let _ = tmux::kill_session(session);
    }

    let (wt_path, uses_worktree) = restart_execution_path(task)?;
    if task.is_meta {
        write_meta_context(&wt_path, task)?;
    }

    let session = tmux::session_name(&task.id);
    if let Err(e) = tmux::create_session(&session, &wt_path) {
        return Err(e.context("failed to create tmux session"));
    }

    let harness = harness_for_task(store, task);
    let prompt = build_prompt(task);
    let cmd = harness.resume_command(&prompt);
    if let Err(e) = tmux::send_command(&session, &cmd) {
        let _ = tmux::kill_session(&session);
        return Err(e.context("failed to spawn agent (session cleaned up)"));
    }

    // Enable continuous session logging for restoration after reboot
    let log_path = tmux::session_log_path(&wt_path);
    if let Err(e) = tmux::enable_logging(&session, &log_path) {
        eprintln!("[resume] Failed to enable session logging: {}", e);
    }

    let mut updated = task.clone();
    updated.status = desired_status;
    updated.worktree_path = uses_worktree.then_some(wt_path);
    updated.tmux_session = Some(session);
    updated.agent_cli = Some(harness.name().to_owned());
    store.update_task(&updated)?;

    Ok(updated)
}

/// Kill a running task: terminate the tmux session and mark as Blocked.
pub fn kill_task(store: &TaskStore, task: &Task) -> Result<Task> {
    let harness = harness_for_task(store, task);
    if let Some(ref session) = task.tmux_session {
        tmux::kill_session(session)?;
    }
    let mut updated = task.clone();
    updated.status = TaskStatus::Blocked;
    updated.tmux_session = None;
    updated.agent_cli = Some(harness.name().to_owned());
    store.update_task(&updated)?;
    Ok(updated)
}

/// Send a message/command to a running or blocked agent.
///
/// If the task is Blocked, it is moved back to Running (the agent got new work).
/// Returns the updated task.
pub fn send_message(store: &TaskStore, task: &Task, message: &str) -> Result<Task> {
    let session = task
        .tmux_session
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Task {} has no tmux session", &task.id[..8]))?;

    let harness = harness_for_task(store, task);

    // A completed agent often leaves its shell and tmux session alive. In
    // that state send-keys only types into the shell; it does not start a new
    // agent, so the user's prompt is silently lost. Re-launch the configured
    // CLI in the existing pane and wait until its process is visible before
    // delivering the follow-up prompt.
    if !tmux::session_exists(session) {
        anyhow::bail!("tmux session '{session}' does not exist");
    }
    // Upgrade sessions created before the named agent/shell windows were
    // introduced, including direct-folder tasks.
    tmux::ensure_task_windows(
        session,
        task.worktree_path.as_deref().unwrap_or(&task.repo_path),
    )?;
    let agent_running = tmux::pane_pid(session)
        .map(|pid| {
            !harness.process_name().is_empty()
                && tmux::has_agent_descendant(pid, harness.process_name())
        })
        .unwrap_or(false);

    if !agent_running {
        tmux::send_command(session, &harness.start_command())?;
        tmux::wait_for_agent(session, harness.process_name())?;
    }

    tmux::send_command(session, message)?;

    let mut updated = task.clone();
    updated.agent_cli = Some(harness.name().to_owned());
    if task.status == TaskStatus::Blocked {
        updated.status = TaskStatus::Running;
    }
    if updated.status != task.status || updated.agent_cli != task.agent_cli {
        store.update_task(&updated)?;
    }

    Ok(updated)
}

/// Information about a task deletion, used to drive the TUI flow.
pub struct DeleteInfo {
    pub has_unmerged_work: bool,
}

/// Check a task before deletion — returns info about unmerged work.
pub fn pre_delete_check(task: &Task) -> DeleteInfo {
    let has_unmerged = task
        .branch_name
        .as_ref()
        .map(|b| worktree::has_unmerged_work(&task.repo_path, b))
        .unwrap_or(false);
    DeleteInfo {
        has_unmerged_work: has_unmerged,
    }
}

/// Delete a task: kill session, delete branch, clean up worktree, log deletion, remove from store.
pub fn delete_task(store: &TaskStore, task: &Task) -> Result<()> {
    // Kill tmux session if running
    if let Some(ref session) = task.tmux_session {
        let _ = tmux::kill_session(session);
    }

    // Check for unmerged work (for logging)
    let had_unmerged = task
        .branch_name
        .as_ref()
        .map(|b| worktree::has_unmerged_work(&task.repo_path, b))
        .unwrap_or(false);

    // Clean up worktree first (must happen before branch delete since the worktree checks out the branch)
    if let Some(ref wt) = task.worktree_path {
        // Direct meta workspaces have no worktree_path and are intentionally
        // preserved. Git-backed meta tasks do have one and must be detached.
        let _ = worktree::cleanup_worktree(&task.repo_path, wt);
    }

    // Delete the branch
    if let Some(ref branch) = task.branch_name {
        let _ = worktree::delete_branch(&task.repo_path, branch);
    }

    // Log the deletion
    let _ = store.log_deletion(task, had_unmerged);

    // Remove from task store
    store.delete_task(&task.id)?;
    Ok(())
}

/// Mark a task as done: update status, keep session and worktree alive for inspection.
pub fn done_task(store: &TaskStore, task: &Task) -> Result<Task> {
    let harness = harness_for_task(store, task);
    let mut updated = task.clone();
    updated.status = TaskStatus::Done;
    updated.agent_cli = Some(harness.name().to_owned());
    store.update_task(&updated)?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a string to max_len characters, adding ellipsis if needed.
fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TaskStatus;

    fn test_task(title: &str, description: &str) -> Task {
        Task {
            id: "00000000-0000-0000-0000-000000000000".to_string(),
            title: title.to_string(),
            description: description.to_string(),
            status: TaskStatus::Backlog,
            repo_path: "/tmp/test".to_string(),
            is_meta: false,
            worktree_path: None,
            branch_name: None,
            tmux_session: None,
            agent_cli: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn build_prompt_title_only_when_description_empty() {
        let task = test_task("Fix login bug", "");
        assert_eq!(build_prompt(&task), "Fix login bug");
    }

    #[test]
    fn build_prompt_title_only_when_description_matches_title() {
        let task = test_task("Fix login bug", "Fix login bug");
        assert_eq!(build_prompt(&task), "Fix login bug");
    }

    #[test]
    fn meta_prompt_references_context_file_without_inlining_board_contract() {
        let mut task = test_task("Review board", "Inspect the backlog");
        task.is_meta = true;

        let prompt = build_prompt(&task);
        assert!(prompt.contains(".agentboard/meta-task-context.md"));
        assert!(prompt.contains("Review board"));
        assert!(prompt.contains("Inspect the backlog"));
        assert!(!prompt.contains("ab list"));
    }

    #[test]
    fn meta_context_file_contains_detailed_board_contract() {
        let dir = tempfile::tempdir().unwrap();
        let task = test_task("Review board", "Inspect the backlog");
        write_meta_context(dir.path().to_str().unwrap(), &task).unwrap();
        let context =
            std::fs::read_to_string(dir.path().join(".agentboard/meta-task-context.md")).unwrap();
        assert!(context.contains("ab list"));
        assert!(context.contains("ab show <id>"));
        assert!(context.contains("tmux capture-pane"));
        assert!(context.contains("not the Agentboard source repository"));
        assert!(context.contains("[--dir <path>] [--meta]"));
        assert!(context.contains("Do not run `ab board`"));
        assert!(!context.contains("* `ab board`"));
        assert!(context.contains("Inspect the backlog"));
    }

    #[test]
    fn build_prompt_joins_title_and_description() {
        let task = test_task("Fix login bug", "The password check is broken");
        assert_eq!(
            build_prompt(&task),
            "Fix login bug — The password check is broken"
        );
    }

    #[test]
    fn build_prompt_strips_newlines_from_description() {
        let task = test_task("Fix login", "line one\nline two\n\nline three");
        let prompt = build_prompt(&task);
        assert!(!prompt.contains('\n'), "prompt must not contain newlines");
        assert_eq!(prompt, "Fix login — line one line two line three");
    }

    #[test]
    fn build_prompt_strips_newlines_from_title() {
        let task = test_task("Fix\nlogin\nbug", "");
        let prompt = build_prompt(&task);
        assert!(!prompt.contains('\n'));
        assert_eq!(prompt, "Fix login bug");
    }

    #[test]
    fn sanitize_prompt_collapses_whitespace() {
        assert_eq!(sanitize_prompt("  hello \n  world  \n\n  "), "hello world");
    }

    #[test]
    fn sanitize_prompt_handles_crlf_and_bare_cr() {
        assert_eq!(sanitize_prompt("a\r\nb\rc"), "a b c");
    }
}
