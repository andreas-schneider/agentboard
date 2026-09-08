# Worktree setup

Agentboard can run initialization steps whenever it creates a task worktree.

## Repository setup

Create `.agentboard/setup.toml` in the source repository:

```toml
steps = [
  "pnpm install --frozen-lockfile",
  "pnpm build",
  "ln -s \"$HOME/shared-data\" ./data"
]
```

The file may be committed or kept uncommitted in the source checkout. An
uncommitted file is read before the worktree is created.

Setup runs automatically when a task starts. Skip it for a particular task
with:

```bash
ab start <id> --skip-setup
```

Setup runs inside the task's tmux session before the agent starts. Its output
is visible in the live preview. If setup fails, the worktree and session remain
available for inspection.

## Machine-specific setup

Store setup that should not be committed in:

```text
~/.agentboard/setup/<repo-id>.toml
```

`<repo-id>` is the canonical repository path with `/` replaced by `__`, for
example `home__me__src__myapp.toml`. The repository setup file takes
precedence over the machine-specific file.
