# TUI and session keybindings

## Agentboard board

### Navigation

| Key | Action |
|-----|--------|
| `h`/`l` or `←`/`→` | Move between columns |
| `j` or `↓`/`↑` | Move between tasks (`k` sends a task to Keep) |
| `Tab`/`Shift+Tab` | Cycle columns |
| `[`/`]` | Scroll live preview |

### Task actions

| Key | Action |
|-----|--------|
| `Enter` | Start (backlog) / Attach or auto-resume (running/blocked/done/keep) |
| `s` | Start selected backlog task |
| `e` | Edit backlog task (title, description, working directory) |
| `m` | Send a message to an agent (running/blocked) |
| `p` | Open the quick prompts palette |
| `K` | Kill a running task (with confirmation) |
| `D` | Mark a task as done |
| `R` | Restart a blocked/done/keep task |
| `k` | Send the selected task to the Keep lane |
| `v` | Show or hide the Keep lane |
| `x` | Delete a task (with confirmation) |

The **Keep** lane is a parked column for unfinished work that is neither
blocked nor done — for example a sketch of an idea worth revisiting later. It
is hidden by default; press `v` to reveal it and `v` again to hide it. Press
`k` to send the selected task there from any other lane. Keep sorts like Done
(most recently updated first), and a kept task can be revealed and resumed
later with `Enter` or restarted with `R`.

### General

| Key | Action |
|-----|--------|
| `n` | Create a new task |

While creating a task, `Tab`/arrow keys navigate through the fields and the
optional Meta task toggle. Press `Enter` or `Space` when the toggle is focused
to activate it. Meta tasks add Agentboard board/task-management context. Their
Working directory defaults to a generated Agentboard meta-workspace but remains
editable; a Git directory still uses a task worktree.

| `r` | Refresh the task list |
| `f` | Cycle the working-directory filter |
| `F` | Clear the working-directory filter |
| `?` | Show the help overlay |
| `q`/`Esc` | Quit |

## Attached agent sessions

Agent sessions run inside tmux. The tmux prefix is `Ctrl+b`; press the keys
sequentially, not simultaneously:

| Key sequence | Action |
|--------------|--------|
| `Ctrl+b`, then `d` | Detach from the session and return to the board |
| `Ctrl+b`, then `[` | Enter tmux copy mode to navigate and inspect session output |

Each task session has two tmux windows: `agent` (window `0`) and `shell`
(window `1`), both rooted in the task worktree. Use `Ctrl+b`, then `0` or `n`
to view the agent, and `Ctrl+b`, then `1` or `p` to view the shell. The
mnemonic shortcuts `Ctrl+b`, then `a` (agent) or `s` (shell) do the same.

Press `q` or `Esc` to leave copy mode.

## Quick prompts

Press `p` on a running or blocked task to open the prompt palette, then press
a key to send instantly:

| Key | Prompt | What it does |
|-----|--------|--------------|
| `1` | Review | Review changes, use REVIEW/REVIEW.md if present, fix issues |
| `2` | Commit, push & PR | Commit, choose branch name, push, create/update PR |
| `3` | Run tests | Run test suite, fix failures, write missing tests |
| `4` | Lint & format | Run linter/formatter, fix warnings |
| `5` | Explain changes | Summarize all changes, files, decisions, trade-offs |
| `6` | Continue | Pick up where the agent left off |
| `7` | Commit only | Stage and commit without pushing |
| `8` | Undo last change | Revert last commit or restore uncommitted files |

Sending a prompt to a blocked task moves it back to running.
