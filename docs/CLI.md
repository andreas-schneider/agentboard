# CLI reference

The board is the primary interface:

```bash
ab board [--repo /path/to/repository]
```

The remaining commands are useful for scripts or workflows that do not use the
TUI.

| Command | Description |
|---------|-------------|
| `ab new "title" [--repo /path] [-d "details"]` | Create a backlog task; the repository defaults to the current directory |
| `ab start <id>` | Start a task and create its worktree and agent session |
| `ab list` | List tasks |
| `ab show <id>` | Show task details and recent agent output |
| `ab attach <id>` | Attach to a task's tmux session |
| `ab edit <id> [-t title] [-d details] [-r repo]` | Edit a backlog task |
| `ab kill <id>` | Stop a running task's agent session |
| `ab done <id>` | Mark a task done while preserving its session and worktree |

Task IDs accept unique prefixes.

## Global options

These options may be used with `board`, `start`, and other commands:

| Option | Description |
|--------|-------------|
| `--agent <kiro-cli|codex>` | Override the configured agent |
| `--model <model>` | Override the configured model |
| `--profile <interactive|unattended>` | Override the configured execution profile |
| `--yolo` | Use the unattended profile; conflicts with `--profile` |

See [Configuration](CONFIGURATION.md) for persistent defaults and safety
details. Run `ab --help` or `ab <command> --help` for the authoritative command
syntax.
