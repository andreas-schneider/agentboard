use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use rusqlite::params;

// ---------------------------------------------------------------------------
// TaskStatus
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Backlog,
    Running,
    Blocked,
    Done,
}

impl TaskStatus {
    /// Returns the ordered list of kanban column names.
    #[allow(dead_code)]
    pub fn all_columns() -> &'static [&'static str] {
        &["Backlog", "Running", "Blocked", "Done"]
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaskStatus::Backlog => "Backlog",
            TaskStatus::Running => "Running",
            TaskStatus::Blocked => "Blocked",
            TaskStatus::Done => "Done",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for TaskStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Backlog" => Ok(TaskStatus::Backlog),
            "Running" => Ok(TaskStatus::Running),
            "Blocked" => Ok(TaskStatus::Blocked),
            // Migrate old statuses to Blocked
            "Finished" | "Failed" => Ok(TaskStatus::Blocked),
            "Done" => Ok(TaskStatus::Done),
            other => Err(anyhow!("unknown task status: {}", other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub repo_path: String,
    pub worktree_path: Option<String>,
    pub branch_name: Option<String>,
    pub tmux_session: Option<String>,
    /// Agent CLI that owns this task's session. Backlog and legacy tasks may
    /// not have one until they are started or detected from a live process.
    pub agent_cli: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// TaskStore
// ---------------------------------------------------------------------------

pub struct TaskStore {
    conn: rusqlite::Connection,
}

/// Add task-owned agent identity to databases created by older Agentboard
/// versions. SQLite has no portable `ADD COLUMN IF NOT EXISTS`, so inspect the
/// table first and keep startup migration idempotent.
fn ensure_agent_cli_column(conn: &rusqlite::Connection) -> Result<()> {
    let mut statement = conn
        .prepare("PRAGMA table_info(tasks)")
        .context("failed to inspect tasks schema")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "agent_cli") {
        conn.execute("ALTER TABLE tasks ADD COLUMN agent_cli TEXT", [])
            .context("failed to add task agent column")?;
    }
    Ok(())
}

impl TaskStore {
    /// Open (or create) the database at `~/.agentboard/agentboard.db`.
    pub fn open() -> Result<Self> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        let db_dir = home.join(".agentboard");
        std::fs::create_dir_all(&db_dir)
            .with_context(|| format!("failed to create directory {}", db_dir.display()))?;

        let db_path = db_dir.join("agentboard.db");
        let conn = rusqlite::Connection::open(&db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        )
        .context("failed to set database pragmas")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                description   TEXT NOT NULL,
                status        TEXT NOT NULL,
                repo_path     TEXT NOT NULL,
                worktree_path TEXT,
                branch_name   TEXT,
                tmux_session  TEXT,
                agent_cli     TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );",
        )
        .context("failed to create tasks table")?;

        ensure_agent_cli_column(&conn)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS deleted_tasks (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                branch_name   TEXT,
                repo_path     TEXT NOT NULL,
                deleted_at    TEXT NOT NULL,
                had_unmerged_work INTEGER NOT NULL DEFAULT 0
            );",
        )
        .context("failed to create deleted_tasks table")?;

        Ok(Self { conn })
    }

    /// Open an in-memory database for testing.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let conn =
            rusqlite::Connection::open_in_memory().context("failed to open in-memory database")?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        )
        .context("failed to set database pragmas")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                description   TEXT NOT NULL,
                status        TEXT NOT NULL,
                repo_path     TEXT NOT NULL,
                worktree_path TEXT,
                branch_name   TEXT,
                tmux_session  TEXT,
                agent_cli     TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );",
        )
        .context("failed to create tasks table")?;

        ensure_agent_cli_column(&conn)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS deleted_tasks (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                branch_name   TEXT,
                repo_path     TEXT NOT NULL,
                deleted_at    TEXT NOT NULL,
                had_unmerged_work INTEGER NOT NULL DEFAULT 0
            );",
        )
        .context("failed to create deleted_tasks table")?;

        Ok(Self { conn })
    }

    /// Create a new task with `Backlog` status and a freshly generated UUID.
    pub fn create_task(&self, title: &str, description: &str, repo_path: &str) -> Result<Task> {
        let now = chrono::Utc::now().to_rfc3339();
        let task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            description: description.to_string(),
            status: TaskStatus::Backlog,
            repo_path: repo_path.to_string(),
            worktree_path: None,
            branch_name: None,
            tmux_session: None,
            agent_cli: None,
            created_at: now.clone(),
            updated_at: now,
        };

        self.conn
            .execute(
                "INSERT INTO tasks (id, title, description, status, repo_path,
                                    worktree_path, branch_name, tmux_session,
                                    agent_cli, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    task.id,
                    task.title,
                    task.description,
                    task.status.to_string(),
                    task.repo_path,
                    task.worktree_path,
                    task.branch_name,
                    task.tmux_session,
                    task.agent_cli,
                    task.created_at,
                    task.updated_at,
                ],
            )
            .context("failed to insert task")?;

        Ok(task)
    }

    /// Return every task, ordered by `created_at`.
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, description, status, repo_path,
                        worktree_path, branch_name, tmux_session, agent_cli,
                        created_at, updated_at
                 FROM tasks
                 ORDER BY created_at",
            )
            .context("failed to prepare list query")?;

        let tasks = stmt
            .query_map([], |row| {
                Ok(Task {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    status: TaskStatus::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or(TaskStatus::Backlog),
                    repo_path: row.get(4)?,
                    worktree_path: row.get(5)?,
                    branch_name: row.get(6)?,
                    tmux_session: row.get(7)?,
                    agent_cli: row.get(8)?,
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })
            .context("failed to execute list query")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to collect tasks")?;

        Ok(tasks)
    }

    /// Get a single task by exact id or by unique prefix match.
    ///
    /// If `id` is a prefix that matches exactly one task, that task is returned.
    /// If it matches zero or more than one, an error is returned.
    pub fn get_task(&self, id: &str) -> Result<Task> {
        let pattern = format!("{}%", id);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, description, status, repo_path,
                        worktree_path, branch_name, tmux_session, agent_cli,
                        created_at, updated_at
                 FROM tasks
                 WHERE id LIKE ?1",
            )
            .context("failed to prepare get query")?;

        let tasks: Vec<Task> = stmt
            .query_map(params![pattern], |row| {
                Ok(Task {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    status: TaskStatus::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or(TaskStatus::Backlog),
                    repo_path: row.get(4)?,
                    worktree_path: row.get(5)?,
                    branch_name: row.get(6)?,
                    tmux_session: row.get(7)?,
                    agent_cli: row.get(8)?,
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })
            .context("failed to execute get query")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to collect tasks")?;

        match tasks.len() {
            0 => Err(anyhow!("no task found matching '{}'", id)),
            1 => Ok(tasks.into_iter().next().unwrap()),
            n => Err(anyhow!(
                "ambiguous task id prefix '{}' matches {} tasks",
                id,
                n
            )),
        }
    }

    /// Update only the status of a task.
    pub fn update_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self
            .conn
            .execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.to_string(), now, id],
            )
            .context("failed to update task status")?;

        if rows == 0 {
            return Err(anyhow!("no task found with id '{}'", id));
        }
        Ok(())
    }

    /// Full update of the mutable fields: worktree_path, branch_name,
    /// tmux_session, agent_cli, and status.
    pub fn update_task(&self, task: &Task) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self
            .conn
            .execute(
                "UPDATE tasks
                 SET status        = ?1,
                     worktree_path = ?2,
                     branch_name   = ?3,
                     tmux_session  = ?4,
                     agent_cli     = ?5,
                     updated_at    = ?6
                 WHERE id = ?7",
                params![
                    task.status.to_string(),
                    task.worktree_path,
                    task.branch_name,
                    task.tmux_session,
                    task.agent_cli,
                    now,
                    task.id,
                ],
            )
            .context("failed to update task")?;

        if rows == 0 {
            return Err(anyhow!("no task found with id '{}'", task.id));
        }
        Ok(())
    }

    /// Record the agent discovered in a live task session without changing
    /// `updated_at`, which is also used by completion grace-period logic.
    pub fn update_agent_cli(&self, id: &str, agent_cli: &str) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "UPDATE tasks SET agent_cli = ?1 WHERE id = ?2",
                params![agent_cli, id],
            )
            .context("failed to update task agent")?;
        if rows == 0 {
            return Err(anyhow!("no task found with id '{}'", id));
        }
        Ok(())
    }

    /// Update the user-editable fields of a task: title, description, repo_path.
    /// Only allowed for Backlog tasks (enforced by callers).
    pub fn update_task_details(
        &self,
        id: &str,
        title: &str,
        description: &str,
        repo_path: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self
            .conn
            .execute(
                "UPDATE tasks
                 SET title       = ?1,
                     description = ?2,
                     repo_path   = ?3,
                     updated_at  = ?4
                 WHERE id = ?5",
                params![title, description, repo_path, now, id],
            )
            .context("failed to update task details")?;

        if rows == 0 {
            return Err(anyhow!("no task found with id '{}'", id));
        }
        Ok(())
    }

    /// Log a task deletion for audit purposes.
    pub fn log_deletion(&self, task: &Task, had_unmerged_work: bool) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO deleted_tasks (id, title, branch_name, repo_path, deleted_at, had_unmerged_work)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    task.id,
                    task.title,
                    task.branch_name,
                    task.repo_path,
                    now,
                    had_unmerged_work as i32,
                ],
            )
            .context("failed to log task deletion")?;
        Ok(())
    }

    /// Atomically claim a Running task for restoration.
    ///
    /// Updates `updated_at` only if the task is still `Running` and its
    /// `updated_at` matches `expected_updated_at` (compare-and-swap). Returns
    /// `true` if the claim succeeded (this process won the race), `false` if
    /// another process already claimed or modified the task.
    pub fn claim_for_restore(&self, id: &str, expected_updated_at: &str) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self
            .conn
            .execute(
                "UPDATE tasks
                 SET updated_at = ?1
                 WHERE id = ?2
                   AND status = 'Running'
                   AND updated_at = ?3",
                params![now, id, expected_updated_at],
            )
            .context("failed to claim task for restore")?;
        Ok(rows > 0)
    }

    /// Delete a task by id.
    pub fn delete_task(&self, id: &str) -> Result<()> {
        let rows = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])
            .context("failed to delete task")?;

        if rows == 0 {
            return Err(anyhow!("no task found with id '{}'", id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create an in-memory store for testing.
    fn test_store() -> TaskStore {
        TaskStore::open_in_memory().expect("failed to open in-memory store")
    }

    #[test]
    fn create_and_get_roundtrip() {
        let store = test_store();
        let task = store
            .create_task("Fix login bug", "The login page crashes", "/tmp/myrepo")
            .unwrap();

        assert!(!task.id.is_empty());
        assert_eq!(task.title, "Fix login bug");
        assert_eq!(task.description, "The login page crashes");
        assert_eq!(task.status, TaskStatus::Backlog);
        assert_eq!(task.repo_path, "/tmp/myrepo");
        assert!(task.worktree_path.is_none());
        assert!(task.branch_name.is_none());
        assert!(task.tmux_session.is_none());
        assert!(task.agent_cli.is_none());
        assert!(!task.created_at.is_empty());
        assert_eq!(task.created_at, task.updated_at);

        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.id, task.id);
        assert_eq!(fetched.title, task.title);
        assert_eq!(fetched.description, task.description);
        assert_eq!(fetched.status, task.status);
        assert_eq!(fetched.repo_path, task.repo_path);
        assert_eq!(fetched.worktree_path, task.worktree_path);
        assert_eq!(fetched.branch_name, task.branch_name);
        assert_eq!(fetched.tmux_session, task.tmux_session);
        assert_eq!(fetched.agent_cli, task.agent_cli);
        assert_eq!(fetched.created_at, task.created_at);
        assert_eq!(fetched.updated_at, task.updated_at);
    }

    #[test]
    fn get_task_by_prefix() {
        let store = test_store();
        let task = store.create_task("Prefix task", "", "/tmp/repo").unwrap();

        // UUIDs are 36 chars; use first 8 as prefix
        let prefix = &task.id[..8];
        let fetched = store.get_task(prefix).unwrap();
        assert_eq!(fetched.id, task.id);
    }

    #[test]
    fn get_task_prefix_ambiguity() {
        let store = test_store();
        let t1 = store.create_task("Task A", "", "/tmp/repo").unwrap();
        let t2 = store.create_task("Task B", "", "/tmp/repo").unwrap();

        // Use a 1-char prefix: both UUIDs start with a hex character, so try
        // the first character of t1. If by extreme luck they differ, just use
        // full id (the real test is the error path).
        let prefix = &t1.id[..1];
        let result = store.get_task(prefix);

        if t2.id.starts_with(prefix) {
            // Expected: ambiguous
            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("ambiguous"),
                "expected ambiguous error, got: {}",
                err_msg
            );
        } else {
            // Extremely unlikely but possible: the 1-char prefixes differ.
            // In that case, verify a valid match was returned.
            assert!(result.is_ok());
        }
    }

    #[test]
    fn get_task_not_found() {
        let store = test_store();
        let result = store.get_task("nonexistent-id-12345");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no task found"),
            "expected 'no task found' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn list_tasks_empty() {
        let store = test_store();
        let tasks = store.list_tasks().unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn list_tasks_ordered_by_created_at() {
        let store = test_store();

        // Insert tasks with manually controlled created_at so ordering is deterministic.
        let ids: Vec<String> = (0..3)
            .map(|i| {
                let id = uuid::Uuid::new_v4().to_string();
                let ts = format!("2026-01-01T00:00:0{}+00:00", i);
                store
                    .conn
                    .execute(
                        "INSERT INTO tasks (id, title, description, status, repo_path,
                                            created_at, updated_at)
                         VALUES (?1, ?2, '', 'Backlog', '/tmp/repo', ?3, ?3)",
                        params![id, format!("Task {}", i), ts],
                    )
                    .unwrap();
                id
            })
            .collect();

        let tasks = store.list_tasks().unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, ids[0]);
        assert_eq!(tasks[1].id, ids[1]);
        assert_eq!(tasks[2].id, ids[2]);
    }

    #[test]
    fn update_status() {
        let store = test_store();
        let task = store.create_task("Status task", "", "/tmp/repo").unwrap();
        assert_eq!(task.status, TaskStatus::Backlog);

        store.update_status(&task.id, TaskStatus::Running).unwrap();

        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.status, TaskStatus::Running);
        // updated_at should have advanced
        assert!(fetched.updated_at >= task.updated_at);
    }

    #[test]
    fn update_status_nonexistent() {
        let store = test_store();
        let result = store.update_status("does-not-exist", TaskStatus::Running);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no task found"),
            "expected 'no task found' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn update_task() {
        let store = test_store();
        let mut task = store.create_task("Update me", "", "/tmp/repo").unwrap();

        task.status = TaskStatus::Running;
        task.worktree_path = Some("/tmp/repo/.agentboard-worktrees/test".into());
        task.branch_name = Some("ab/test-branch".into());
        task.tmux_session = Some("ab-test1234".into());
        task.agent_cli = Some("codex".into());

        store.update_task(&task).unwrap();

        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.status, TaskStatus::Running);
        assert_eq!(
            fetched.worktree_path.as_deref(),
            Some("/tmp/repo/.agentboard-worktrees/test")
        );
        assert_eq!(fetched.branch_name.as_deref(), Some("ab/test-branch"));
        assert_eq!(fetched.tmux_session.as_deref(), Some("ab-test1234"));
        assert_eq!(fetched.agent_cli.as_deref(), Some("codex"));
    }

    #[test]
    fn migrates_legacy_tasks_table_with_agent_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NOT NULL,
                status TEXT NOT NULL,
                repo_path TEXT NOT NULL,
                worktree_path TEXT,
                branch_name TEXT,
                tmux_session TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();

        ensure_agent_cli_column(&conn).unwrap();
        ensure_agent_cli_column(&conn).unwrap();

        let columns = conn
            .prepare("PRAGMA table_info(tasks)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "agent_cli"));
    }

    #[test]
    fn discovered_agent_update_preserves_updated_at() {
        let store = test_store();
        let task = store.create_task("Detect agent", "", "/tmp/repo").unwrap();
        store.update_agent_cli(&task.id, "copilot").unwrap();
        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.agent_cli.as_deref(), Some("copilot"));
        assert_eq!(fetched.updated_at, task.updated_at);
    }

    #[test]
    fn update_task_details() {
        let store = test_store();
        let task = store
            .create_task("Original title", "Original desc", "/tmp/original")
            .unwrap();

        store
            .update_task_details(&task.id, "New title", "New desc", "/tmp/new")
            .unwrap();

        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.title, "New title");
        assert_eq!(fetched.description, "New desc");
        assert_eq!(fetched.repo_path, "/tmp/new");
        assert!(fetched.updated_at >= task.updated_at);
    }

    #[test]
    fn delete_task() {
        let store = test_store();
        let task = store.create_task("Delete me", "", "/tmp/repo").unwrap();

        store.delete_task(&task.id).unwrap();

        let result = store.get_task(&task.id);
        assert!(result.is_err());
    }

    #[test]
    fn delete_task_nonexistent() {
        let store = test_store();
        let result = store.delete_task("does-not-exist");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("no task found"),
            "expected 'no task found' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn claim_for_restore_success() {
        let store = test_store();
        let task = store.create_task("Restore me", "", "/tmp/repo").unwrap();

        // Move to Running first
        store.update_status(&task.id, TaskStatus::Running).unwrap();
        let running = store.get_task(&task.id).unwrap();

        let claimed = store
            .claim_for_restore(&task.id, &running.updated_at)
            .unwrap();
        assert!(claimed, "claim should succeed with correct updated_at");

        // After claiming, updated_at should have changed
        let after = store.get_task(&task.id).unwrap();
        assert_ne!(after.updated_at, running.updated_at);
    }

    #[test]
    fn claim_for_restore_wrong_timestamp() {
        let store = test_store();
        let task = store.create_task("Restore me", "", "/tmp/repo").unwrap();

        store.update_status(&task.id, TaskStatus::Running).unwrap();

        let claimed = store
            .claim_for_restore(&task.id, "1999-01-01T00:00:00+00:00")
            .unwrap();
        assert!(
            !claimed,
            "claim should fail with wrong updated_at timestamp"
        );
    }

    #[test]
    fn claim_for_restore_wrong_status() {
        let store = test_store();
        let task = store.create_task("Backlog task", "", "/tmp/repo").unwrap();

        // Task is Backlog, not Running — claim should fail
        let claimed = store.claim_for_restore(&task.id, &task.updated_at).unwrap();
        assert!(!claimed, "claim should fail for non-Running task");
    }

    #[test]
    fn log_deletion() {
        let store = test_store();
        let task = store
            .create_task("Log deletion", "desc", "/tmp/repo")
            .unwrap();

        // Should not crash
        store.log_deletion(&task, false).unwrap();
        store.log_deletion(&task, true).unwrap(); // second call exercises OR REPLACE
    }
}
