---
name: agentboard-board
description: Manage Agentboard tasks with the ab CLI when the user asks to inspect, create, start, edit, or update tasks on the board. Use from any Agentboard agent session, including project tasks; do not use for ordinary source-code work that does not involve the board.
---

# Agentboard board

Use `ab` commands to work with the Agentboard board. Do not run `ab board` inside an agent session; it opens the human TUI. The current directory may be a project's task worktree, not the Agentboard source checkout. Keep working on the current task there unless the user asks you to switch tasks.

Use `ab --help` and `ab <command> --help` for exact syntax. Common commands:

- `ab list` lists tasks and statuses.
- `ab show <id>` shows task details, working directory, and recent output.
- `ab new "title" [-d "description"] [--dir <path>] [--meta]` creates a backlog task. Use `--meta` for a task primarily about managing the board.
- `ab start <id>` starts a backlog task.
- `ab edit <id> [-t "title"] [-d "description"] [--dir <path>]` edits a backlog task.
- `ab done <id>` marks a task Done; `ab kill <id>` stops its session.

Task IDs accept unique prefixes. Inspect the relevant task with `ab show` before changing it. Preserve its working directory unless the user asks to move it. When creating a project task, provide its project directory explicitly with `--dir`; a Git directory gets a separate task worktree.

Agentboard completion normally moves a task to Blocked for review. Mark it Done only when its requested work is complete. Do not kill sessions, delete tasks, change Agentboard configuration, or make bulk board changes without the user's authorization. Never kill or detach the session you are currently running in as a side effect of board work.
