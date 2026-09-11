//! Agent harness abstraction.
//!
//! An agent harness knows how to produce the shell command that spawns an AI
//! agent inside an already-running tmux session (via `send-keys`). The
//! orchestrator is responsible for creating the tmux session and capturing
//! output; the harness only cares about the command string.

use crate::config;

/// Marker printed after the agent command exits, followed by the exit code.
/// The orchestrator watches captured pane output for this pattern.
pub const EXIT_MARKER: &str = "AB_AGENT_EXIT:";
pub const SUPPORTED_AGENTS: &[&str] = &["kiro-cli", "codex", "copilot"];

/// Trait implemented by every agent backend (kiro-cli, codex, aider, etc.).
pub trait AgentHarness {
    /// Return the command string to spawn the agent with the given prompt in a
    /// tmux session.  The command will be sent to an already-created tmux
    /// session via `send-keys`.
    ///
    /// The returned command is wrapped so that after completion it prints
    /// `AB_AGENT_EXIT:<code>` where `<code>` is the exit status.
    fn spawn_command(&self, prompt: &str) -> String;

    /// Return the command that starts the interactive agent without sending
    /// it a task. This is used before a follow-up message when the previous
    /// agent process has exited but its tmux shell is still alive.
    fn start_command(&self) -> String {
        self.spawn_command("")
    }

    /// Name of this harness (e.g., `"kiro-cli"`).
    #[allow(dead_code)]
    fn name(&self) -> &str;

    /// Patterns that indicate the agent has finished its task and is idle,
    /// waiting for new input.  Some agents (e.g. kiro-cli) don't exit after
    /// a task — they return to an interactive prompt.  The orchestrator scans
    /// the last few lines of tmux output for any of these patterns.
    ///
    /// Return an empty slice if the agent exits on task completion (use the
    /// exit-marker approach instead).
    fn idle_patterns(&self) -> &[&str] {
        &[]
    }

    /// Patterns that indicate the agent is actively working and should NOT be
    /// considered idle, even if idle_patterns appear in the scrollback.
    ///
    /// When any of these patterns are found in the visible pane, the agent is
    /// treated as busy regardless of other signals.
    fn active_patterns(&self) -> &[&str] {
        &[]
    }

    /// Whether the visible pane shows the agent waiting for input.
    ///
    /// Agents whose composer is always visible can override this and use a
    /// status indicator instead of `idle_patterns`.
    fn is_idle(&self, visible: &str) -> bool {
        let is_active = self
            .active_patterns()
            .iter()
            .any(|pattern| contains_case_insensitive(visible, pattern));
        !is_active
            && self
                .idle_patterns()
                .iter()
                .any(|pattern| contains_case_insensitive(visible, pattern))
    }

    /// The process name to look for in the process tree to verify the agent is
    /// still running. Used as a fallback when pane text detection is ambiguous.
    fn process_name(&self) -> &str {
        ""
    }

    /// Return the command string to re-launch the agent after an interruption
    /// (e.g., reboot, restart, or resuming a Blocked task). The agent should
    /// resume its previous conversation.
    ///
    /// The configured `resume_args` are used and the original prompt is not
    /// re-sent. Each agent/profile defines its own resume behavior.
    fn resume_command(&self, original_prompt: &str) -> String {
        let _ = original_prompt;
        wrap_with_exit_marker(&configured_command(self.name(), true, None))
    }
}

/// Wrap a raw command string so it prints the exit marker when done.
fn wrap_with_exit_marker(cmd: &str) -> String {
    // Run the command, capture its exit code, print the marker, then
    // preserve the exit code so the shell shows the right status.
    format!("{cmd}; __ab_ec=$?; echo ''; echo '{EXIT_MARKER}'$__ab_ec; (exit $__ab_ec)")
}

/// Quote a value for use as one shell argument.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Resolve the effective configuration for an agent, falling back to the
/// built-in defaults if the user configuration is missing or invalid.
fn resolve_config(agent: &str) -> config::EffectiveConfig {
    config::load()
        .and_then(|file| config::effective(&file, Some(agent)))
        .unwrap_or_else(|error| {
            eprintln!("warning: ignoring invalid Agentboard configuration: {error}");
            config::effective(&config::FileConfig::default(), Some(agent))
                .expect("built-in agent configuration must be valid")
        })
}

/// Build the shell command string from an already-resolved configuration.
///
/// This is pure (no filesystem or environment access), so it can be tested
/// against a known configuration without depending on the ambient user
/// configuration file.
fn format_command(cfg: &config::EffectiveConfig, resume: bool, prompt: Option<&str>) -> String {
    let mut args = if resume {
        cfg.resume_args.clone()
    } else {
        cfg.args.clone()
    };
    if let Some(prompt) = prompt {
        args.push(prompt.into());
    }
    let mut parts = vec![shell_quote(&cfg.program)];
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn configured_command(agent: &str, resume: bool, prompt: Option<&str>) -> String {
    format_command(&resolve_config(agent), resume, prompt)
}

fn contains_case_insensitive(text: &str, pattern: &str) -> bool {
    text.to_lowercase().contains(&pattern.to_lowercase())
}

/// Harness for the `kiro-cli chat` interface.
///
/// kiro-cli uses a positional argument for the initial prompt and runs as an
/// interactive TUI.  Key flags:
///   - `--trust-all-tools` / `-a`: auto-approve every tool invocation (only in
///     the explicit `unattended` profile).
///   - The prompt is the positional `[INPUT]` argument, passed as a
///     single-quoted shell string so that double quotes and other special
///     characters in the prompt are preserved.
pub struct KiroCliHarness;

impl AgentHarness for KiroCliHarness {
    fn spawn_command(&self, prompt: &str) -> String {
        let raw_cmd = configured_command("kiro-cli", false, Some(prompt));
        wrap_with_exit_marker(&raw_cmd)
    }

    fn name(&self) -> &str {
        "kiro-cli"
    }

    fn start_command(&self) -> String {
        wrap_with_exit_marker(&configured_command("kiro-cli", false, None))
    }

    fn idle_patterns(&self) -> &[&str] {
        // kiro-cli shows this prompt when it finishes a task and waits for the
        // next instruction.  We look for this in captured pane output.
        &["ask a question or describe a task"]
    }

    fn active_patterns(&self) -> &[&str] {
        // kiro-cli shows this status bar text while it is actively working.
        // Its presence means the agent is busy — do NOT mark as idle even if
        // idle_patterns appear in the scrollback above.
        &["Kiro is working"]
    }

    fn process_name(&self) -> &str {
        // The key process to look for in the tree. kiro-cli-term spawns bash,
        // which spawns kiro-cli. If kiro-cli is gone, the agent has exited.
        "kiro-cli"
    }
}

/// Harness for the OpenAI Codex CLI.
///
/// Codex accepts an initial prompt as a positional argument and remains in
/// its interactive terminal UI, which lets agentboard attach to it and send
/// follow-up messages.  Approval prompts are disabled so a task can run
/// unattended. Agentboard already creates and selects a dedicated Git
/// worktree for each task, so Codex is given that current directory explicitly
/// and its own filesystem sandbox is disabled. This avoids layering Codex's
/// workspace boundary on top of agentboard's worktree isolation.
pub struct CodexHarness;

impl AgentHarness for CodexHarness {
    fn spawn_command(&self, prompt: &str) -> String {
        let raw_cmd = configured_command("codex", false, Some(prompt));
        wrap_with_exit_marker(&raw_cmd)
    }

    fn name(&self) -> &str {
        "codex"
    }

    fn start_command(&self) -> String {
        wrap_with_exit_marker(&configured_command("codex", false, None))
    }

    fn process_name(&self) -> &str {
        "codex"
    }

    fn idle_patterns(&self) -> &[&str] {
        // The composer glyph (`›`) is visible both while Codex is working and
        // while it is waiting, so it is not an idle signal.
        &[]
    }

    fn active_patterns(&self) -> &[&str] {
        // The status line distinguishes a working Codex session from an idle
        // composer. Keep both strings because capitalization has changed
        // across Codex releases.
        &["Working (", "esc to interrupt"]
    }

    fn is_idle(&self, visible: &str) -> bool {
        // Codex keeps its composer visible for the entire session. Its
        // transient Working/interrupt status is the reliable distinction.
        !self
            .active_patterns()
            .iter()
            .any(|pattern| contains_case_insensitive(visible, pattern))
    }
}

/// Harness for the GitHub Copilot CLI interactive interface.
///
/// Copilot starts an interactive session with `--interactive <PROMPT>` and
/// persists sessions per working directory. `--continue` therefore resumes
/// the most recent session in Agentboard's task worktree.
pub struct CopilotHarness;

impl AgentHarness for CopilotHarness {
    fn spawn_command(&self, prompt: &str) -> String {
        let raw_cmd = configured_command("copilot", false, Some(prompt));
        wrap_with_exit_marker(&raw_cmd)
    }

    fn name(&self) -> &str {
        "copilot"
    }

    fn start_command(&self) -> String {
        // `--interactive` requires a prompt, so do not use the normal spawn
        // arguments when reopening a session solely to send a follow-up.
        let mut cfg = resolve_config("copilot");
        cfg.args.retain(|arg| arg != "--interactive" && arg != "-i");
        wrap_with_exit_marker(&format_command(&cfg, false, None))
    }

    fn process_name(&self) -> &str {
        "copilot"
    }

    fn active_patterns(&self) -> &[&str] {
        // Copilot's composer remains on screen while it works. Its working
        // status is the reliable signal that a task is not waiting for input.
        &["Working", "esc to interrupt"]
    }

    fn is_idle(&self, visible: &str) -> bool {
        !self
            .active_patterns()
            .iter()
            .any(|pattern| contains_case_insensitive(visible, pattern))
    }
}

/// Return a harness for a supported agent name.
pub fn harness_for(agent: &str) -> Option<Box<dyn AgentHarness>> {
    match agent {
        "kiro-cli" => Some(Box::new(KiroCliHarness)),
        "codex" => Some(Box::new(CodexHarness)),
        "copilot" => Some(Box::new(CopilotHarness)),
        _ => None,
    }
}

/// Return the configured agent harness. Kiro remains the built-in default.
pub fn default_harness() -> Box<dyn AgentHarness> {
    let agent = config::load()
        .ok()
        .and_then(|cfg| config::effective(&cfg, None).ok())
        .map(|cfg| cfg.agent)
        .or_else(|| std::env::var("AGENTBOARD_AGENT").ok());
    agent
        .as_deref()
        .and_then(harness_for)
        .unwrap_or_else(|| Box::new(KiroCliHarness))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_command_plain_prompt() {
        let cfg = config::builtin_effective("kiro-cli", "interactive").unwrap();
        let cmd = wrap_with_exit_marker(&format_command(&cfg, false, Some("do the thing")));
        assert!(cmd.starts_with("'kiro-cli' 'chat' 'do the thing'"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn spawn_command_preserves_double_quotes() {
        let harness = KiroCliHarness;
        let cmd = harness.spawn_command(r#"say "hello""#);
        assert!(cmd.contains(r#"'say "hello"'"#));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn spawn_command_escapes_single_quotes() {
        let harness = KiroCliHarness;
        let cmd = harness.spawn_command("it's a test");
        assert!(cmd.contains("'it'\\''s a test'"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn name_returns_kiro_cli() {
        let harness = KiroCliHarness;
        assert_eq!(harness.name(), "kiro-cli");
    }

    #[test]
    fn default_harness_is_kiro_cli() {
        let h = default_harness();
        let expected = match std::env::var("AGENTBOARD_AGENT").as_deref() {
            Ok("codex") => "codex",
            Ok("copilot") => "copilot",
            _ => "kiro-cli",
        };
        assert_eq!(h.name(), expected);
    }

    #[test]
    fn harness_lookup_supports_every_known_agent() {
        for name in SUPPORTED_AGENTS {
            assert_eq!(harness_for(name).unwrap().name(), *name);
        }
        assert!(harness_for("unknown").is_none());
    }

    #[test]
    fn codex_spawn_command_uses_agentboard_worktree_and_full_access_mode() {
        let cfg = config::builtin_effective("codex", "interactive").unwrap();
        let cmd =
            wrap_with_exit_marker(&format_command(&cfg, false, Some("implement the feature")));
        assert!(cmd.starts_with(
            "'codex' '--cd' '.' '--ask-for-approval' 'on-request' '--sandbox' 'workspace-write'"
        ));
        assert!(cmd.contains("'implement the feature'"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn codex_name_and_process_name() {
        let harness = CodexHarness;
        assert_eq!(harness.name(), "codex");
        assert_eq!(harness.process_name(), "codex");
        assert!(harness.idle_patterns().is_empty());
        assert_eq!(
            harness.active_patterns(),
            &["Working (", "esc to interrupt"]
        );
    }

    #[test]
    fn codex_idle_state_ignores_always_visible_composer() {
        let harness = CodexHarness;
        assert!(harness.is_idle("› task text\n\n  gpt-5.6-luna medium"));
        assert!(!harness.is_idle("› task text\n\n• Working (5s • esc to interrupt)"));
    }

    #[test]
    fn codex_spawn_command_escapes_prompt() {
        let harness = CodexHarness;
        let cmd = harness.spawn_command("it's $HOME `date`");
        assert!(cmd.contains("'it'\\''s $HOME `date`'"));
    }

    #[test]
    fn codex_resume_uses_last_session_subcommand() {
        let cfg = config::builtin_effective("codex", "interactive").unwrap();
        let cmd = wrap_with_exit_marker(&format_command(&cfg, true, None));
        assert!(cmd.starts_with("'codex' 'resume' '--last'"));
        assert!(!cmd.contains("ignored original prompt"));
    }

    #[test]
    fn copilot_spawn_and_resume_use_interactive_worktree_session() {
        let cfg = config::builtin_effective("copilot", "interactive").unwrap();
        let spawn = wrap_with_exit_marker(&format_command(&cfg, false, Some("implement it")));
        assert!(spawn.starts_with("'copilot' '-C' '.' '--interactive' 'implement it'"));

        let resume = wrap_with_exit_marker(&format_command(&cfg, true, None));
        assert!(resume.starts_with("'copilot' '-C' '.' '--continue'"));
    }

    #[test]
    fn copilot_harness_reports_working_state_and_escapes_prompt() {
        let harness = CopilotHarness;
        assert_eq!(harness.name(), "copilot");
        assert_eq!(harness.process_name(), "copilot");
        assert!(harness.is_idle("› Tell Copilot what to do"));
        assert!(!harness.is_idle("Working (3s) · esc to interrupt"));
        assert!(harness
            .spawn_command("it's $HOME")
            .contains("'it'\\''s $HOME'"));
    }

    #[test]
    fn copilot_start_command_omits_interactive_flag_without_a_prompt() {
        let harness = CopilotHarness;
        let command = harness.start_command();
        assert!(command.starts_with("'copilot' '-C' '.'"));
        assert!(!command.contains("--interactive"));
    }

    #[test]
    fn wrap_includes_exit_code_capture() {
        let wrapped = wrap_with_exit_marker("my_cmd");
        assert!(wrapped.starts_with("my_cmd; __ab_ec=$?;"));
        assert!(wrapped.contains("AB_AGENT_EXIT:"));
    }

    #[test]
    fn resume_command_uses_resume_flag() {
        let cfg = config::builtin_effective("kiro-cli", "interactive").unwrap();
        let cmd = wrap_with_exit_marker(&format_command(&cfg, true, None));
        assert!(cmd.contains("'kiro-cli' 'chat' '--resume'"));
        // Should NOT contain the original prompt (--resume loads from .kiro/)
        assert!(!cmd.contains("Fix the login bug"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn start_commands_launch_interactive_agents_without_a_prompt() {
        let kiro_cfg = config::builtin_effective("kiro-cli", "interactive").unwrap();
        let kiro = wrap_with_exit_marker(&format_command(&kiro_cfg, false, None));
        assert_eq!(kiro, "'kiro-cli' 'chat'; __ab_ec=$?; echo ''; echo 'AB_AGENT_EXIT:'$__ab_ec; (exit $__ab_ec)");

        let codex_cfg = config::builtin_effective("codex", "interactive").unwrap();
        let codex = wrap_with_exit_marker(&format_command(&codex_cfg, false, None));
        assert!(codex.starts_with(
            "'codex' '--cd' '.' '--ask-for-approval' 'on-request' '--sandbox' 'workspace-write'"
        ));
        assert!(codex.contains("; __ab_ec=$?"));
        assert!(!codex.contains("workspace-write ''"));
    }

    #[test]
    fn spawn_command_preserves_backticks() {
        let harness = KiroCliHarness;
        let cmd = harness.spawn_command("run `command` now");
        // Backticks should appear inside single quotes, preventing shell expansion
        assert!(cmd.contains("'run `command` now'"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn spawn_command_preserves_dollar_vars() {
        let harness = KiroCliHarness;
        let cmd = harness.spawn_command("check $HOME and ${VAR}");
        // Dollar signs inside single quotes are literal — no shell expansion
        assert!(cmd.contains("'check $HOME and ${VAR}'"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn spawn_command_handles_newlines() {
        let harness = KiroCliHarness;
        let cmd = harness.spawn_command("line one\nline two");
        // The newline should be preserved inside single quotes
        assert!(cmd.contains("'line one\nline two'"));
        assert!(cmd.contains(EXIT_MARKER));
    }

    #[test]
    fn spawn_command_handles_mixed_metacharacters() {
        let harness = KiroCliHarness;
        let cmd = harness.spawn_command("it's $HOME `date` \"hello\"");
        // Single quotes in the prompt get escaped via the '\'' technique;
        // all other metacharacters ($, backticks, double quotes) stay inside
        // single-quoted segments and are preserved literally.
        assert!(cmd.contains("'it'\\''s $HOME `date` \"hello\"'"));
        assert!(cmd.contains(EXIT_MARKER));
    }
}
