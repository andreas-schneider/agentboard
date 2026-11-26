---
name: agentboard-board
description: Manage Agentboard tasks with the ab CLI when the user asks to inspect, create, schedule, start, edit, or update tasks on the board. Use from any Agentboard agent session, including project tasks; do not use for ordinary source-code work that does not involve the board.
---

# Agentboard board

Use `ab` commands to work with the Agentboard board. Do not run `ab board` inside an agent session; it opens the human TUI. The current directory may be a project's task worktree, not the Agentboard source checkout. Keep working on the current task there unless the user asks you to switch tasks.

Use `ab --help` and `ab <command> --help` for exact syntax. Common commands:

- `ab list` lists tasks and statuses.
- `ab show <id>` shows task details, scheduled start or launch error, and recent output.
- `ab new "title" [-d "description"] [--dir <path>] [--meta] [--at "time"]` creates a backlog task. Use `--meta` for a task primarily about managing the board.
- `ab schedule <id> --at "time"` schedules or reschedules a backlog task. Use `ab schedule <id> --clear` to cancel its scheduled start.
- `ab start <id>` starts a backlog task immediately, including one with a future or failed scheduled start.
- `ab edit <id> [-t "title"] [-d "description"] [--dir <path>]` edits a backlog task.
- `ab done <id>` marks a task Done; `ab kill <id>` stops its session.

For `--at`, use a local time such as `tomorrow 09:00`, a delay such as `in 30m`, or an ISO 8601 timestamp with a UTC offset when the timezone is specified. Verify the resulting time with `ab show`.

Task IDs accept unique prefixes. Inspect the relevant task with `ab show` before changing it. Preserve its working directory unless the user asks to move it. When creating a project task, provide its project directory explicitly with `--dir`; a Git directory gets a separate task worktree.

Agentboard completion normally moves a task to Blocked for review. Mark it Done only when its requested work is complete. Do not kill sessions, delete tasks, change Agentboard configuration, or make bulk board changes without the user's authorization. Never kill or detach the session you are currently running in as a side effect of board work.
