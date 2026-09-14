# Review findings

- [x] **[blocking]** (src/orchestrator.rs:610 — Operability & Failure Modes) — Starting a task could leave a live tmux session and newly created Git worktree behind when the final task update failed. Fixed by killing the session and cleaning the task worktree before returning the persistence error; direct user-owned working directories remain untouched.
- [x] **[praise]** — The change keeps meta tasks on the ordinary task lifecycle, persists the meta flag with an idempotent migration, escapes task-ID prefixes for SQL `LIKE`, and covers the new prompt/context and storage behavior with focused tests.

Validation: `cargo test` — 92 passed; `git diff --check` — passed.
