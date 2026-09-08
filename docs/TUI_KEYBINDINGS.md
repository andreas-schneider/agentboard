# TUI and session keybindings

## Agentboard board

### Navigation

| Key | Action |
|-----|--------|
| `h`/`l` or `←`/`→` | Move between columns |
| `j`/`k` or `↓`/`↑` | Move between tasks |
| `Tab`/`Shift+Tab` | Cycle columns |
| `[`/`]` | Scroll live preview |

### Task actions

| Key | Action |
|-----|--------|
| `Enter` | Start (backlog) / Attach or auto-resume (running/blocked/done) |
| `s` | Start selected backlog task |
| `e` | Edit backlog task (title, description, repo) |
| `m` | Send a message to an agent (running/blocked) |
| `p` | Open the quick prompts palette |
| `K` | Kill a running task (with confirmation) |
| `D` | Mark a task as done |
| `R` | Restart a blocked/done task |
| `x` | Delete a task (with confirmation) |

### General

| Key | Action |
|-----|--------|
| `n` | Create a new task |
| `r` | Refresh the task list |
| `f` | Cycle the repository filter |
| `F` | Clear the repository filter |
| `?` | Show the help overlay |
| `q`/`Esc` | Quit |

## Attached agent sessions

Agent sessions run inside tmux. The tmux prefix is `Ctrl+b`; press the keys
sequentially, not simultaneously:

| Key sequence | Action |
|--------------|--------|
| `Ctrl+b`, then `d` | Detach from the session and return to the board |
| `Ctrl+b`, then `[` | Enter tmux copy mode to navigate and inspect session output |

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
