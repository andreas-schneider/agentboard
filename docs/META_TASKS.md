# Meta tasks

Meta tasks are ordinary tasks on the same Agentboard board. Enable **Meta task**
when the agent should manage the board or its tasks rather than work on a
project's source code.

All tasks, including ordinary project tasks, can use the `agentboard-board`
skill when asked to work with the board. Opening `ab board` installs it into
the user skill directories for Kiro, Codex, and Copilot, before any new agent
session launches. A meta task adds a board-focused starting prompt and an
isolated default working directory; it is not required for occasional board
operations.

Create one in the TUI by focusing the final Meta task toggle in the New Task
form and pressing `Enter` or `Space`. From the CLI, use:

```bash
ab new "Review blocked tasks" --meta -d "Find blockers and update the affected tasks."
```

The task's working directory remains editable. If none is supplied, Agentboard
suggests an isolated directory under `~/.agentboard/meta-workspaces/`. Git
directories use a task worktree; non-Git directories are used directly.

## Agent context

Before Agentboard starts, restarts, resumes, or restores a meta task, it writes
`.agentboard/meta-task-context.md` in that task's actual execution directory.
The agent receives a short prompt instructing it to read this file first.

The file contains the task title and description, the non-interactive `ab` CLI
commands and task-status semantics, safety rules, and relevant tmux guidance.
It explicitly directs agents to use the CLI rather than `ab board`, whose TUI is
for human operators.
