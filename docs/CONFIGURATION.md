# Configuration

Agentboard works with embedded defaults, but a user configuration makes the
selected agent and safety profile explicit. Create it with:

```bash
ab config init
ab config show
ab config check
```

The file is stored in the platform's standard user configuration directory:
`~/.config/agentboard/config.toml` on Linux. `init` does not overwrite an
existing file.

## Complete configuration

`ab config init` generates this file, so you do not need to copy it manually:

```toml
# Agentboard configuration
# This file is user-owned and is never read from a repository.
agent = "kiro-cli"
profile = "interactive"

[agents.kiro-cli]
program = "kiro-cli"

[agents.codex]
program = "codex"
# model = "gpt-5-codex"

[agents.copilot]
program = "copilot"
# model = "auto"

# The prompt is appended automatically. These are complete argument lists.
[profiles.interactive.kiro-cli]
args = ["chat"]
resume_args = ["chat", "--resume"]

[profiles.interactive.codex]
args = ["--cd", ".", "--ask-for-approval", "on-request", "--sandbox", "workspace-write"]
resume_args = ["resume", "--last"]

[profiles.interactive.copilot]
args = ["-C", ".", "--interactive"]
resume_args = ["-C", ".", "--continue"]

# Use "unattended" only when you explicitly trust the agent and repository.
[profiles.unattended.kiro-cli]
args = ["chat", "--trust-all-tools"]
resume_args = ["chat", "--trust-all-tools", "--resume"]

[profiles.unattended.codex]
args = ["--cd", ".", "--ask-for-approval", "never", "--sandbox", "danger-full-access"]
resume_args = ["resume", "--last"]

[profiles.unattended.copilot]
args = ["-C", ".", "--allow-all", "--no-ask-user", "--interactive"]
resume_args = ["-C", ".", "--allow-all", "--no-ask-user", "--continue"]
```

`agent` and `profile` select the defaults. Each `[agents.<name>]` entry defines
the executable and optional model. Each `[profiles.<profile>.<agent>]` entry
defines the complete arguments for starting and resuming that agent. The task
prompt is appended automatically; arguments are passed as individual values,
not as a shell command.

Agentboard currently supports `kiro-cli`, `codex`, and GitHub Copilot CLI
(`copilot`). Some terminal interaction
and completion detection remains specific to each supported CLI.

## One-time overrides

Global flags override the configuration for one command:

```bash
ab --agent codex board
ab --agent codex --model gpt-5.6-luna board
ab --agent copilot board
ab --profile interactive board
ab --yolo board
```

`--yolo` is shorthand for `--profile unattended`. The unattended profile may
disable the selected CLI's approval and sandbox protections. A worktree only
isolates Git changes; it is not a security boundary.

The selected executable must be installed, authenticated, and available on
`PATH`. Agentboard checks this before it starts a task and reports which
configuration entry to update when the executable cannot be found.

Agentboard records the selected agent when a task starts. Live tmux sessions
are inspected for their actual agent process, so tasks using different CLIs can
be monitored on the same board. If a legacy task has no recorded agent, a live
process is used to backfill it; otherwise the current default is the fallback.
