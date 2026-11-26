# CLI reference

The board is the primary interface:

```bash
ab board [--repo /path/to/repository]
```

The remaining commands are useful for scripts or workflows that do not use the
TUI.

| Command | Description |
|---------|-------------|
| `ab new "title" [--dir /path] [-d "details"] [--meta] [--at "time"]` | Create a backlog task; the working directory defaults to the current directory. Git directories get a worktree; other directories run directly. `--meta` supplies board/task-management context and, when no directory is supplied, an Agentboard-managed directory. `--repo` remains an alias for `--dir`. |
| `ab schedule <id> (--at "time" or --clear)` | Set, change, or clear a scheduled start for a backlog task |
| `ab start <id>` | Start a task; Git working directories get a worktree, while other directories run directly in an agent session |
| `ab list` | List tasks |
| `ab show <id>` | Show task details and recent agent output |
| `ab attach <id>` | Attach to a task's tmux session |
| `ab edit <id> [-t title] [-d details] [--dir path]` | Edit a backlog task. `--repo` remains an alias for `--dir`. |
| `ab kill <id>` | Stop a running task's agent session |
| `ab done <id>` | Mark a task done while preserving its session and worktree |

Task IDs accept unique prefixes.

## Global options

These options may be used with `board`, `start`, and other commands:

| Option | Description |
|--------|-------------|
| `--agent <kiro-cli|codex|copilot>` | Override the configured agent |
| `--model <model>` | Override the configured model |
| `--profile <interactive|unattended>` | Override the configured execution profile |
| `--yolo` | Use the unattended profile; conflicts with `--profile` |

See [Configuration](CONFIGURATION.md) for persistent defaults and safety
details. Run `ab --help` or `ab <command> --help` for the authoritative command
syntax.

## Scheduled starts

```bash
ab new "Run the nightly checks" --at "tomorrow 09:00"
ab schedule <id> --at "in 30m"
ab schedule <id> --at "2027-02-15T09:00:00+01:00"
ab schedule <id> --clear
```

Use `ab show <id>` to inspect the scheduled time, agent, profile, and any
launch error. `ab start <id>` starts a scheduled task immediately or retries a
failed launch using the saved agent configuration. A successful start,
deletion, or move out of Backlog clears the schedule.

Agentboard saves schedules in its local database and starts a background
scheduler when you first schedule a task. The scheduler keeps running when the
board closes. If your machine sleeps, tasks that came due start after it wakes.
After a reboot or if the scheduler stops, opening the board restarts it and
processes overdue tasks. To start tasks without reopening the board after a
reboot, run `ab scheduler` through your user service manager at login with the
same home directory and a PATH that includes tmux and your agent CLI. You can
also invoke `ab scheduler --once` from a recurring user timer. Background
scheduler diagnostics are written to `~/.agentboard/scheduler.log`.
