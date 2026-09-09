# agentboard (`ab`)

Agentboard turns scattered terminal sessions into a task-based workflow. It
connects each agent session to a task and gives you one board to see what is
queued, running, blocked, or done. From the board, you can start, attach to,
resume, or message tasks.

Each task gets its own Git worktree and tmux session, so you can work on several
tasks without losing track of which terminal belongs to what.

## Setup

You need:

- `git`, `tmux`, and a current Rust toolchain
- A supported agent CLI: `kiro-cli` or `codex`
- The selected agent CLI authenticated and available on `PATH`

Install Agentboard from a checkout:

```bash
cargo install --path .
```

Create and validate your user configuration:

```bash
ab config init
ab config check
```

This creates `~/.config/agentboard/config.toml` on Linux, using `kiro-cli` and
the approval-based `interactive` profile by default. Edit `agent` if you want
to use `codex`. See [Configuration](docs/CONFIGURATION.md) for all options and
the complete generated file.

## Use the board

Run Agentboard from the repository you want to work on:

```bash
ab board
```

Press `n` to create a task and `Enter` to start it. Agentboard creates the
worktree, starts the agent, and shows its live progress. Tasks move through
Backlog, Running, Blocked, and Done; select a task and press `Enter` to start,
attach, or resume it as appropriate. When entering a session you can scroll with `Ctrl+b [` and leave the scroll mode with `Esc`. You can switch from the session back to the board with `Ctrl+b d`.

Essential controls:

| Key | Action |
|-----|--------|
| `n` | Create a task |
| `Enter` | Start, attach to, or resume the selected task |
| `p` | Send a quick prompt |
| `?` | Show board help |
| `q` / `Esc` | Quit the board or exit scroll mode in the session |
| `Ctrl+b`, then `d` | Detach from an agent session and return to the board |
| `Ctrl+b`, then `[` | Navigate and scroll inside an agent session |

See the
[complete TUI keybinding reference](docs/TUI_KEYBINDINGS.md) for task actions,
navigation, and quick prompts.

## Choose an agent, model, or mode

The configuration file defines the defaults. Override them for one invocation
when needed:

```bash
ab --agent codex board
ab --agent codex --model gpt-5.6-luna board
ab --yolo board
```

The default `interactive` profile retains the agent CLI's normal approvals and
sandboxing. `--yolo` selects the `unattended` profile, which may disable those
protections. A Git worktree is not a security boundary; use unattended mode
only with agents, repositories, and credentials you trust.

## More documentation

- [Configuration](docs/CONFIGURATION.md) — agents, models, profiles, and CLI arguments
- [CLI reference](docs/CLI.md) — commands for scripts and non-TUI use
- [Worktree setup](docs/WORKTREE_SETUP.md) — repository and machine-specific setup
- [TUI keybindings](docs/TUI_KEYBINDINGS.md) — all board and session controls
- [Session restoration](docs/SESSION_RESTORATION.md) — experimental and not well tested
- [Contributing](CONTRIBUTING.md) — development checks and pull requests

## Local data and support

Task metadata is stored in `~/.agentboard/agentboard.db`; session logs are
stored under each worktree in `.agentboard/session.log`. These files can
contain prompts, source code, and other sensitive data.

Agentboard sends no telemetry itself, but the agent CLI you select may
communicate with its own service.

This project is provided without a support or response-time commitment.
