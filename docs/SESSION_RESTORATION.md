# Session restoration

> Experimental feature: session restoration is not well tested yet. Use it
> with that limitation in mind and verify important work after an interruption.

Agentboard survives reboots and tmux server restarts. When you open `ab board`,
it automatically detects tasks that were Running but whose tmux sessions are
gone, and restores them:

1. **Recreates the tmux session** in the existing worktree (your code changes are safe on disk)
2. **Replays scrollback** from a continuously saved session log — scroll up to see what the agent was doing before the interruption
3. **Re-launches the configured agent** with its resume behavior so it can pick up where it left off

You'll see a notification like "🔄 Restored 3 interrupted tasks" and all tasks
resume as Running.

Session output is continuously logged via tmux `pipe-pane` to
`<worktree>/.agentboard/session.log`. If no log is available (e.g., tasks
started before this feature), the agent still restarts — just without visible
scrollback history.

Tasks that can't be restored (e.g., the worktree was deleted) are moved to
Blocked so you can decide what to do.
