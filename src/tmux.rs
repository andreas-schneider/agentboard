use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

pub const AGENT_WINDOW: &str = "agent";
pub const SHELL_WINDOW: &str = "shell";

/// Return the tmux session name for a given task id: `ab-<first 8 chars>`.
pub fn session_name(task_id: &str) -> String {
    let prefix = &task_id[..task_id.len().min(8)];
    format!("ab-{prefix}")
}

pub fn agent_target(session: &str) -> String {
    format!("{session}:{AGENT_WINDOW}")
}

/// Create a detached tmux session in the given working directory.
///
/// After creation, waits for the shell inside the pane to display its prompt,
/// indicating it has fully initialised and is ready to accept input. Without
/// this, a subsequent `send_command` can race the shell initialisation — tmux
/// delivers the characters to the pty but the shell may discard or mishandle
/// them if it hasn't finished setting up readline.
pub fn create_session(session: &str, working_dir: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            AGENT_WINDOW,
            "-c",
            working_dir,
        ])
        .output()
        .context("failed to spawn tmux new-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-session failed: {stderr}");
    }

    // Wait for the shell inside the new pane to be ready. We poll the
    // visible pane content until a shell prompt appears — that means the
    // shell has fully initialised and is ready to accept send-keys input.
    wait_for_shell_ready(&agent_target(session))?;
    create_shell_window(session, working_dir)?;
    configure_agentboard_bindings()?;

    Ok(())
}

/// Poll until the tmux pane shows a shell prompt, indicating the shell has
/// fully initialised and is ready to accept input.
///
/// Previous approach checked `pane_current_command` which reports "bash" as
/// soon as the process starts — long before `.bashrc` finishes and readline
/// is ready. This caused a race where `send-keys` text would appear in the
/// terminal but never execute because the shell wasn't listening yet.
///
/// Now we capture the visible pane content and look for common prompt
/// indicators (`$`, `#`, `%`, `>`). This ensures the shell has finished
/// initialisation and is truly waiting for input.
fn wait_for_shell_ready(target: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let poll_interval = Duration::from_millis(50);

    while Instant::now() < deadline {
        if let Ok(content) = capture_visible_pane_target(target) {
            // Look for a prompt character at or near the end of the visible
            // content. We check the last non-empty line for common prompt
            // endings: `$ `, `# `, `% `, or `> `.  We also accept these
            // characters at the very end of the line (no trailing space).
            if let Some(last_line) = content.lines().rev().find(|l| !l.trim().is_empty()) {
                let trimmed = last_line.trim_end();
                if trimmed.ends_with('$')
                    || trimmed.ends_with('#')
                    || trimmed.ends_with('%')
                    || trimmed.ends_with('>')
                    || trimmed.ends_with("$ ")
                    || trimmed.ends_with("# ")
                    || trimmed.ends_with("% ")
                    || trimmed.ends_with("> ")
                {
                    return Ok(());
                }
            }
        }
        thread::sleep(poll_interval);
    }

    // Timed out — proceed anyway; the shell might still work but we don't
    // want to block indefinitely.
    Ok(())
}

/// Ensure sessions created by older versions gain the named shell window.
pub fn ensure_task_windows(session: &str, working_dir: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_name}\t#{window_id}",
        ])
        .output()
        .context("failed to list tmux task windows")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux list-windows failed: {stderr}");
    }

    let mut has_agent = false;
    let mut has_shell = false;
    let mut first_window_id = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.splitn(2, '\t');
        let name = parts.next().unwrap_or_default();
        let id = parts.next().unwrap_or_default();
        if first_window_id.is_none() && !id.is_empty() {
            first_window_id = Some(id.to_string());
        }
        has_agent |= name == AGENT_WINDOW;
        has_shell |= name == SHELL_WINDOW;
    }

    if !has_agent {
        let window_id = first_window_id
            .ok_or_else(|| anyhow::anyhow!("tmux session '{session}' has no windows"))?;
        let output = Command::new("tmux")
            .args(["rename-window", "-t", &window_id, AGENT_WINDOW])
            .output()
            .context("failed to name the agent tmux window")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("tmux rename-window failed: {stderr}");
        }
    }

    if !has_shell {
        create_shell_window(session, working_dir)?;
    }
    configure_agentboard_bindings()?;
    Ok(())
}

/// Install mnemonic aliases for the two Agentboard windows while preserving
/// tmux's normal behavior in other sessions.
///
/// tmux key bindings are server-wide, so the fallback commands deliberately
/// mirror tmux's defaults: `a` sends the prefix through and `s` opens the
/// session chooser. In an `ab-*` session the keys instead select the named
/// task windows.
fn configure_agentboard_bindings() -> Result<()> {
    let bindings = [
        ("a", "select-window -t :agent", "send-prefix"),
        ("s", "select-window -t :shell", "choose-session"),
    ];

    for (key, agentboard_command, fallback_command) in bindings {
        let condition = "#{m:ab-*,#{session_name}}";
        let output = Command::new("tmux")
            .args([
                "bind-key",
                "-T",
                "prefix",
                key,
                "if-shell",
                "-F",
                condition,
                agentboard_command,
                fallback_command,
            ])
            .output()
            .context("failed to configure tmux window shortcuts")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("tmux bind-key failed: {stderr}");
        }
    }
    Ok(())
}

fn create_shell_window(session: &str, working_dir: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args([
            "new-window",
            "-d",
            "-t",
            session,
            "-n",
            SHELL_WINDOW,
            "-c",
            working_dir,
        ])
        .output()
        .context("failed to create tmux shell window")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-window failed: {stderr}");
    }
    Ok(())
}

/// Send literal text to a tmux session without pressing Enter.
///
/// Uses `tmux send-keys -l` to disable key-name interpretation, so strings like
/// `"Enter"`, `"Escape"`, or `"C-c"` are typed literally instead of being
/// interpreted as special keys. The `--` prevents text starting with `-` from
/// being parsed as flags.
pub fn send_text(session: &str, text: &str) -> Result<()> {
    let target = agent_target(session);
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", "--", text])
        .output()
        .context("failed to spawn tmux send-keys")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux send-keys failed: {stderr}");
    }

    Ok(())
}

/// Send a command to a tmux session (types it literally and presses Enter).
///
/// First sends the text via `send-keys -l` (literal mode, no key-name
/// interpretation), then sends `Enter` as a named key in a separate call.
///
/// We deliberately avoid appending `\n` to the literal text because
/// `send-keys -l` delivers a literal newline character (0x0A) to the pty.
/// While plain shells accept that, TUI applications (e.g. kiro-cli) often
/// only respond to the Enter key (carriage-return / 0x0D) which tmux
/// generates when it processes the `Enter` key name.
pub fn send_command(session: &str, command: &str) -> Result<()> {
    send_text(session, command)?;
    press_key(session, "Enter")
}

/// Send a named key (e.g. `Enter`, `Escape`, `C-c`) to a tmux session.
///
/// Unlike [`send_text`], this does **not** use `-l`, so the argument is
/// interpreted as a tmux key name.
fn press_key(session: &str, key: &str) -> Result<()> {
    let target = agent_target(session);
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, key])
        .output()
        .context("failed to spawn tmux send-keys")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux send-keys failed: {stderr}");
    }

    Ok(())
}

/// Attach to a tmux session, handing control to the user.
/// Returns when the user detaches (Ctrl+B D).
pub fn attach_session(session: &str) -> Result<()> {
    let target = agent_target(session);
    let status = Command::new("tmux")
        .args(["attach-session", "-t", &target])
        .status()
        .context("failed to spawn tmux attach-session")?;

    if !status.success() {
        anyhow::bail!("tmux attach-session exited with status {status}");
    }

    Ok(())
}

/// Kill a tmux session.
///
/// Returns `Ok(())` if the session was killed or if it did not exist (already
/// dead / not found). Propagates errors for other failures such as tmux not
/// being installed or permission issues.
pub fn kill_session(session: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .output()
        .context("failed to spawn tmux kill-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_lower = stderr.to_lowercase();
        // "session not found" (and similar) means the session is already gone — not an error.
        if stderr_lower.contains("session not found")
            || stderr_lower.contains("no such session")
            || stderr_lower.contains("can't find session")
        {
            return Ok(());
        }
        anyhow::bail!("tmux kill-session failed: {stderr}");
    }

    Ok(())
}

/// Check whether a tmux session exists.
pub fn session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Wait until an agent process appears in a session's pane process tree.
///
/// Starting an interactive CLI is asynchronous. Sending the follow-up prompt
/// immediately can land in the shell or be consumed while the CLI is still
/// initialising, so callers must wait for the agent before sending text.
pub fn wait_for_agent(session: &str, process_name: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let poll_interval = Duration::from_millis(50);

    while Instant::now() < deadline {
        if !session_exists(session) {
            anyhow::bail!("tmux session '{session}' exited while starting agent");
        }

        if let Ok(pid) = pane_pid(session) {
            if has_agent_descendant(pid, process_name) {
                return Ok(());
            }
        }
        thread::sleep(poll_interval);
    }

    anyhow::bail!("agent '{process_name}' did not become ready in tmux session '{session}'")
}

/// Capture the last `n` lines of output from a tmux session pane.
/// Trailing whitespace and blank lines are trimmed.
pub fn capture_last_lines(session: &str, n: usize) -> Result<String> {
    let target = agent_target(session);
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", &target, "-p", "-S", &format!("-{n}")])
        .output()
        .context("failed to spawn tmux capture-pane")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux capture-pane failed: {stderr}");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim_end().to_string())
}

/// Return the name of the command currently running in the session's active pane.
/// For example `"bash"`, `"zsh"`, `"kiro-cli"`, `"node"`, etc.
///
/// NOTE: For kiro-cli, this always returns `"bash"` because kiro-cli-term is a
/// pseudo-terminal emulator that spawns bash internally. Use [`pane_pid`] +
/// process tree inspection instead.
#[allow(dead_code)]
pub fn pane_current_command(session: &str) -> Result<String> {
    let target = agent_target(session);
    let output = Command::new("tmux")
        .args(["list-panes", "-t", &target, "-F", "#{pane_current_command}"])
        .output()
        .context("failed to spawn tmux list-panes")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux list-panes failed: {stderr}");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim().to_string())
}

/// Return the PID of the first process in the session's active pane.
///
/// For agentboard sessions this is typically the `kiro-cli-term` process.
pub fn pane_pid(session: &str) -> Result<u32> {
    let target = agent_target(session);
    let output = Command::new("tmux")
        .args(["list-panes", "-t", &target, "-F", "#{pane_pid}"])
        .output()
        .context("failed to spawn tmux list-panes (pane_pid)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux list-panes (pane_pid) failed: {stderr}");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .parse::<u32>()
        .context("failed to parse pane PID")
}

/// Check whether the pane is back at a shell prompt (the agent command has finished).
/// Returns true if the current command is a known shell (bash, zsh, sh, fish, etc.).
///
/// WARNING: This is unreliable for kiro-cli because `kiro-cli-term` always
/// reports `bash` as its pane_current_command. Prefer [`has_agent_descendant`]
/// for kiro-cli detection.
#[allow(dead_code)]
pub fn is_pane_at_shell(session: &str) -> bool {
    match pane_current_command(session) {
        Ok(cmd) => {
            let cmd_lower = cmd.to_lowercase();
            matches!(
                cmd_lower.as_str(),
                "bash" | "zsh" | "sh" | "fish" | "dash" | "tcsh" | "csh" | "ksh"
            )
        }
        Err(_) => false,
    }
}

/// Check whether the given PID or any descendant process has a command name
/// or command line containing `needle` (case-insensitive).
///
/// Uses `/proc/<pid>/task/` and `/proc/<pid>/children` on Linux, falling back
/// to `pgrep -a -P` for portability.
///
/// This is the reliable way to check whether an agent is still running inside
/// a terminal pane, since `pane_current_command` may only return the shell or
/// a sandbox launcher rather than the agent itself.
pub fn has_agent_descendant(root_pid: u32, needle: &str) -> bool {
    // Use pgrep to find descendants matching the needle.
    // pgrep -a gives full command line, -P restricts to children.
    // We walk the tree recursively via a BFS.
    let needle_lower = needle.to_lowercase();
    let mut to_visit = vec![root_pid];
    let mut visited = std::collections::HashSet::new();

    while let Some(pid) = to_visit.pop() {
        if !visited.insert(pid) {
            continue;
        }

        // Check the root too. Depending on how the shell launches the agent,
        // the pane PID can itself be the agent or its sandbox launcher.
        if process_matches(pid, &needle_lower) {
            return true;
        }

        // Read child PIDs from /proc/<pid>/task/<pid>/children (Linux-specific,
        // requires CONFIG_PROC_CHILDREN). Fall back to pgrep if not available.
        let children_path = format!("/proc/{pid}/task/{pid}/children");
        let child_pids: Vec<u32> = if let Ok(contents) = std::fs::read_to_string(&children_path) {
            contents
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
                .collect()
        } else {
            // Fallback: use pgrep -P <pid> to list direct children
            match Command::new("pgrep")
                .args(["-P", &pid.to_string()])
                .output()
            {
                Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .filter_map(|s| s.parse::<u32>().ok())
                    .collect(),
                _ => Vec::new(),
            }
        };

        for cpid in child_pids {
            if process_matches(cpid, &needle_lower) {
                return true;
            }
            to_visit.push(cpid);
        }
    }

    false
}

/// Identify which supported agent owns a pane process tree.
///
/// Process identity is checked across the whole tree before command-line
/// substrings. That ordering matters because an agent's prompt can itself
/// mention another supported CLI. The substring pass remains as a portability
/// fallback for launchers whose executable name hides the wrapped program.
pub fn detect_agent_descendant<'a>(root_pid: u32, candidates: &'a [&str]) -> Option<&'a str> {
    let pids = descendant_pids(root_pid);

    for pid in &pids {
        for &candidate in candidates {
            if process_identity_matches(*pid, &candidate.to_lowercase()) {
                return Some(candidate);
            }
        }
    }

    for pid in pids {
        for &candidate in candidates {
            if process_matches(pid, &candidate.to_lowercase()) {
                return Some(candidate);
            }
        }
    }

    None
}

fn descendant_pids(root_pid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut to_visit = vec![root_pid];
    let mut visited = std::collections::HashSet::new();
    while let Some(pid) = to_visit.pop() {
        if !visited.insert(pid) {
            continue;
        }
        found.push(pid);
        let children_path = format!("/proc/{pid}/task/{pid}/children");
        let children = if let Ok(contents) = std::fs::read_to_string(&children_path) {
            contents
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok())
                .collect()
        } else {
            match Command::new("pgrep")
                .args(["-P", &pid.to_string()])
                .output()
            {
                Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .filter_map(|value| value.parse::<u32>().ok())
                    .collect(),
                _ => Vec::new(),
            }
        };
        to_visit.extend(children);
    }
    found
}

fn process_identity_matches(pid: u32, needle_lower: &str) -> bool {
    let comm_path = format!("/proc/{pid}/comm");
    if std::fs::read_to_string(comm_path)
        .map(|comm| comm.to_lowercase().contains(needle_lower))
        .unwrap_or(false)
    {
        return true;
    }

    let cmdline_path = format!("/proc/{pid}/cmdline");
    std::fs::read(cmdline_path)
        .ok()
        .and_then(|cmdline| {
            cmdline
                .split(|byte| *byte == 0)
                .next()
                .map(|argv0| String::from_utf8_lossy(argv0).to_lowercase())
        })
        .and_then(|argv0| {
            std::path::Path::new(&argv0)
                .file_name()
                .map(|name| name.to_string_lossy().contains(needle_lower))
        })
        .unwrap_or(false)
}

/// Match both the short process name and its full command line. The latter
/// matters for Codex, which can be launched through a sandbox wrapper whose
/// executable name does not identify the agent in `comm`.
fn process_matches(pid: u32, needle_lower: &str) -> bool {
    let comm_path = format!("/proc/{pid}/comm");
    if std::fs::read_to_string(comm_path)
        .map(|comm| comm.to_lowercase().contains(needle_lower))
        .unwrap_or(false)
    {
        return true;
    }

    let cmdline_path = format!("/proc/{pid}/cmdline");
    std::fs::read(cmdline_path)
        .map(|cmdline| {
            String::from_utf8_lossy(&cmdline)
                .to_lowercase()
                .contains(needle_lower)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Session logging & restoration
// ---------------------------------------------------------------------------

/// Enable continuous logging of a tmux session to a file via `pipe-pane`.
///
/// All output from the pane is appended to `log_path`. This runs in the
/// background as long as the session exists — it survives attach/detach
/// cycles. If logging is already active, calling this replaces the previous
/// pipe-pane sink.
pub fn enable_logging(session: &str, log_path: &str) -> Result<()> {
    // Validate log_path doesn't contain characters that could break shell parsing
    if log_path.contains('\0') || log_path.contains('\n') || log_path.contains('\r') {
        anyhow::bail!("log path contains invalid characters: {}", log_path);
    }

    // Ensure the parent directory exists
    if let Some(parent) = std::path::Path::new(log_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }

    let target = agent_target(session);
    let output = Command::new("tmux")
        .args([
            "pipe-pane",
            "-t",
            &target,
            "-o",
            &format!("cat >> '{}'", log_path.replace('\'', "'\\''")),
        ])
        .output()
        .context("failed to spawn tmux pipe-pane")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux pipe-pane failed: {stderr}");
    }

    Ok(())
}

/// Return the conventional log path for a task's session inside its worktree.
pub fn session_log_path(worktree_path: &str) -> String {
    format!("{}/.agentboard/session.log", worktree_path)
}

/// Strip terminal control sequences and non-printable control characters from
/// captured pane output so it is safe to paste into a shell heredoc.
///
/// The `session.log` written by `pipe-pane` is a raw capture of a full-screen
/// TUI (kiro-cli): it is dense with ANSI/CSI escape sequences, OSC sequences,
/// cursor-movement codes, bracketed-paste toggles, and — most dangerously —
/// bare control characters such as `\x03` (Ctrl-C).  If this raw content is
/// pasted into a bash heredoc during restoration, those bytes corrupt the
/// shell's input state: a stray `\x03` aborts the heredoc and cancels the
/// following command, while bracketed-paste/escape codes can swallow the
/// closing heredoc marker so the heredoc never terminates.  Either way the
/// scrollback replay breaks *and* the subsequent agent-resume command fails to
/// run.
///
/// This function removes:
///  - CSI sequences: `ESC [ ... final-byte`
///  - OSC sequences: `ESC ] ... (BEL | ESC \)`
///  - Other two-byte `ESC <byte>` escapes
///  - All remaining C0 control chars and DEL, except `\n` and `\t`
///
/// The result is plain, printable text that is safe to feed through a heredoc
/// while still preserving the human-readable log lines.
fn sanitize_for_heredoc(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        if b == 0x1b {
            // ESC — start of an escape sequence. Determine its kind.
            match bytes.get(i + 1) {
                Some(b'[') => {
                    // CSI: ESC [ params/intermediates ... final byte in 0x40..=0x7e
                    i += 2;
                    while i < bytes.len() {
                        let c = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&c) {
                            break; // final byte consumed
                        }
                    }
                }
                Some(b']') => {
                    // OSC: ESC ] ... terminated by BEL (0x07) or ST (ESC \)
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => {
                    // Other two-byte escape (e.g. ESC =, ESC >). Drop both bytes.
                    i += 2;
                }
                None => {
                    // Trailing lone ESC — drop it.
                    i += 1;
                }
            }
            continue;
        }

        // Keep newline, tab, printable ASCII, and any high bytes (0x80+) which
        // are UTF-8 continuation/lead bytes — copying them verbatim preserves
        // valid multi-byte characters. Drop all other C0 control chars
        // (including \r and \x03) and DEL (0x7f).
        if b == b'\n' || b == b'\t' || (0x20..0x7f).contains(&b) || b >= 0x80 {
            out.push(b);
        }
        i += 1;
    }

    // High bytes were copied verbatim, so valid UTF-8 stays intact; decode
    // leniently to guard against any partial sequences at the tail boundary.
    String::from_utf8_lossy(&out).into_owned()
}

/// Inject saved scrollback content into a tmux pane so it appears as history.
///
/// Reads up to `max_lines` from the end of the log file and sends them into
/// the pane's history using `tmux load-buffer` + `tmux paste-buffer` with
/// some wrapping so the pasted content doesn't execute as commands.
///
/// The approach:
/// 1. Load the log content into a tmux buffer
/// 2. Use `send-keys` to type `cat << 'AB_SCROLLBACK_EOF'` + Enter (starts a heredoc)
/// 3. Paste the buffer (content appears as text, not executed)
/// 4. Send `AB_SCROLLBACK_EOF` + Enter (closes the heredoc, `cat` prints it)
/// 5. Send `clear` to leave a clean prompt with the history visible in scrollback
///
/// This gives the user a scrollable history of the previous session's output.
///
/// The log content is sanitised via [`sanitize_for_heredoc`] before pasting —
/// see that function for why raw pane captures must never be fed to a heredoc.
pub fn inject_scrollback(session: &str, log_path: &str, max_lines: usize) -> Result<()> {
    let content = match std::fs::read_to_string(log_path) {
        Ok(c) => c,
        Err(_) => return Ok(()), // No log file — nothing to inject
    };

    if content.trim().is_empty() {
        return Ok(());
    }

    // Strip terminal control sequences / control characters. This MUST happen
    // before we take the tail and paste it, otherwise control bytes (notably
    // \x03) corrupt the heredoc and prevent the agent from launching.
    let sanitized = sanitize_for_heredoc(&content);

    // Build the tail to replay, defending against three hazards specific to
    // captured TUI logs:
    //
    //  1. Marker collision — if any replayed line happened to equal the heredoc
    //     terminator, it would close the heredoc early. Neutralise any line
    //     containing the marker.
    //  2. Pathologically long lines — a full-screen TUI writes each repaint as
    //     one enormous "line" (thousands of chars). Truncate them so the paste
    //     stays readable and small.
    //  3. Total paste size — pasting megabytes into a heredoc races the
    //     terminator send-keys and can wedge the shell at the `>` continuation
    //     prompt (so the agent never launches). Cap the total bytes.
    const MARKER: &str = "AB_SCROLLBACK_EOF";
    const MAX_LINE_BYTES: usize = 2000;
    // Keep the pasted payload small. Empirically, pasting more than ~12 KB into
    // a bash heredoc via `paste-buffer` outruns the terminator `send-keys` and
    // can leave the shell wedged at the `>` continuation prompt — which would
    // stop the agent from launching. 8 KB leaves a comfortable safety margin
    // while still giving the user a useful window of recent history.
    const MAX_TOTAL_BYTES: usize = 8 * 1024;

    let all_lines: Vec<&str> = sanitized.lines().collect();
    let start = all_lines.len().saturating_sub(max_lines);

    let mut selected: Vec<String> = Vec::new();
    let mut total_bytes = 0usize;
    // Walk from newest to oldest so the byte budget keeps the most recent
    // output, then reverse to restore chronological order.
    for line in all_lines[start..].iter().rev() {
        // Truncate over-long single lines.
        let mut s: String = if line.len() > MAX_LINE_BYTES {
            let mut cut = MAX_LINE_BYTES;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &line[..cut])
        } else {
            (*line).to_string()
        };
        // Neutralise any accidental heredoc terminator.
        if s.contains(MARKER) {
            s = s.replace(MARKER, "AB_SCROLLBACK_EOF_");
        }
        let cost = s.len() + 1; // +1 for the joining newline
        if total_bytes + cost > MAX_TOTAL_BYTES {
            break;
        }
        total_bytes += cost;
        selected.push(s);
    }
    selected.reverse();
    let tail: String = selected.join("\n");

    if tail.trim().is_empty() {
        return Ok(());
    }

    // Ensure the pasted content ends with a newline. `paste-buffer` sends the
    // buffer verbatim, so without a trailing newline the heredoc terminator we
    // send next gets concatenated onto the final content line
    // (`…lastlineAB_SCROLLBACK_EOF`) and is never recognised as the delimiter —
    // wedging the shell at the `>` continuation prompt and preventing the agent
    // from launching.
    let tail = if tail.ends_with('\n') {
        tail
    } else {
        format!("{tail}\n")
    };

    // Write to a temp file that tmux can load as a buffer.
    // Include PID for uniqueness in case multiple ab processes restore concurrently.
    let tmp_path = format!("/tmp/ab-scrollback-{}-{}", session, std::process::id());
    std::fs::write(&tmp_path, &tail)
        .with_context(|| format!("failed to write temp scrollback file {}", tmp_path))?;

    // Load into a named tmux buffer
    let buf_name = format!("ab-restore-{}", session);
    let output = Command::new("tmux")
        .args(["load-buffer", "-b", &buf_name, &tmp_path])
        .output()
        .context("failed to load tmux buffer")?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux load-buffer failed: {stderr}");
    }

    // Send a heredoc to display the content without executing it.
    //
    // First send Ctrl-C to guarantee we start from a clean prompt: if anything
    // left a partial command on the line (or a stray heredoc from a previous
    // attempt), C-c clears it so our `cat <<` opener isn't appended to garbage.
    press_key(session, "C-c")?;
    thread::sleep(Duration::from_millis(50));
    send_command(session, &format!("cat << '{MARKER}'"))?;
    thread::sleep(Duration::from_millis(50));

    // Paste the buffer
    let output = Command::new("tmux")
        .args(["paste-buffer", "-t", session, "-b", &buf_name])
        .output()
        .context("failed to paste tmux buffer")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Non-fatal — continue without scrollback
        eprintln!("tmux paste-buffer failed: {stderr}");
    }

    // Close the heredoc. Give the shell time to consume the pasted bytes first,
    // otherwise the terminator can race the paste and fail to be recognised.
    thread::sleep(Duration::from_millis(200));
    send_command(session, MARKER)?;

    // Wait for cat to finish, then clear the screen so the prompt is clean
    // but the heredoc output is in scrollback (scroll up to see it)
    thread::sleep(Duration::from_millis(100));
    send_command(session, "clear")?;

    // Clean up
    let _ = std::fs::remove_file(&tmp_path);
    let _ = Command::new("tmux")
        .args(["delete-buffer", "-b", &buf_name])
        .output();

    // Wait for shell to settle
    thread::sleep(Duration::from_millis(100));

    Ok(())
}

/// Capture the visible content of the pane (what's currently displayed, not
/// scrollback history). This is more reliable for detecting TUI state than
/// `capture_last_lines` because it only includes what's actually on screen.
pub fn capture_visible_pane(session: &str) -> Result<String> {
    capture_visible_pane_target(&agent_target(session))
}

fn capture_visible_pane_target(target: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", target, "-p"])
        .output()
        .context("failed to spawn tmux capture-pane (visible)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux capture-pane (visible) failed: {stderr}");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_truncates_long_id() {
        assert_eq!(session_name("abcdefgh-1234-5678"), "ab-abcdefgh");
    }

    #[test]
    fn session_name_short_id() {
        assert_eq!(session_name("abc"), "ab-abc");
    }

    #[test]
    fn agent_target_is_explicitly_named() {
        assert_eq!(agent_target("ab-12345678"), "ab-12345678:agent");
    }

    #[test]
    fn session_log_path_builds_correct_path() {
        let path = session_log_path("/home/user/repo/.agentboard-worktrees/abc123");
        assert_eq!(
            path,
            "/home/user/repo/.agentboard-worktrees/abc123/.agentboard/session.log"
        );
    }

    #[test]
    fn sanitize_strips_ctrl_c_and_carriage_return() {
        // \x03 (Ctrl-C) and \r must be removed — they are what break the heredoc.
        let input = "hello\x03world\r\n";
        assert_eq!(sanitize_for_heredoc(input), "helloworld\n");
    }

    #[test]
    fn sanitize_strips_csi_sequences() {
        // Colour + cursor-move CSI sequences should vanish, text preserved.
        let input = "\x1b[38;2;255;128;255mSh\x1b[0mell \x1b[2Kdone";
        assert_eq!(sanitize_for_heredoc(input), "Shell done");
    }

    #[test]
    fn sanitize_strips_bracketed_paste_and_private_modes() {
        // Bracketed-paste toggles (ESC [ ? 2004 h/l) and similar private modes
        // are what swallow the heredoc terminator; ensure they're removed.
        let input = "before\x1b[?2004hmid\x1b[?2004l\x1b[?25lafter";
        assert_eq!(sanitize_for_heredoc(input), "beforemidafter");
    }

    #[test]
    fn sanitize_strips_osc_sequences() {
        // OSC (e.g. window title) terminated by BEL or ST.
        let bel = "a\x1b]0;my title\x07b";
        assert_eq!(sanitize_for_heredoc(bel), "ab");
        let st = "a\x1b]0;my title\x1b\\b";
        assert_eq!(sanitize_for_heredoc(st), "ab");
    }

    #[test]
    fn sanitize_preserves_newlines_tabs_and_utf8() {
        let input = "line1\n\tindented\nunicode: ✓ 日本語 ⣴";
        assert_eq!(
            sanitize_for_heredoc(input),
            "line1\n\tindented\nunicode: ✓ 日本語 ⣴"
        );
    }

    #[test]
    fn sanitize_handles_trailing_lone_escape() {
        let input = "text\x1b";
        assert_eq!(sanitize_for_heredoc(input), "text");
    }

    #[test]
    fn sanitize_removes_the_marker_hazard_bytes() {
        // A realistic snippet resembling a kiro-cli TUI frame: after
        // sanitising, no ESC or C0 control bytes (except \n/\t) may remain.
        let input = "\x1b[2K\x1b[95mKiro is working\x1b[39m\x03\r\n\x1b[?2026h\x1b[7A done\n";
        let out = sanitize_for_heredoc(input);
        assert!(!out.bytes().any(|b| b == 0x1b || b == 0x03 || b == b'\r'));
        assert!(out.contains("Kiro is working"));
        assert!(out.contains("done"));
    }

    /// Live tmux integration test for the full scrollback-injection flow.
    ///
    /// Gated behind `AB_TMUX_ITEST=1` because it spawns a real tmux session.
    /// It writes a synthetic raw-TUI log (full of escape codes and a stray
    /// Ctrl-C), runs [`inject_scrollback`], and asserts that afterwards the
    /// shell is back at a clean prompt and can execute a subsequent command —
    /// i.e. the heredoc closed cleanly and the agent would launch.
    #[test]
    fn inject_scrollback_leaves_shell_ready_to_run_agent() {
        if std::env::var("AB_TMUX_ITEST").as_deref() != Ok("1") {
            eprintln!("skipping live tmux test (set AB_TMUX_ITEST=1 to run)");
            return;
        }

        let session = format!("ab-itest-{}", std::process::id());
        let _ = kill_session(&session);
        create_session(&session, "/tmp").expect("create session");

        // Build a synthetic raw-TUI log: escape sequences, bracketed paste,
        // a stray Ctrl-C, no trailing newline on the final line, plus a line
        // that collides with the heredoc marker.
        let mut raw = String::new();
        for i in 0..50 {
            raw.push_str(&format!(
                "\x1b[2K\x1b[95mframe {i}\x1b[39m\x1b[?2004h working\x1b[?2004l\r\n"
            ));
        }
        raw.push_str("AB_SCROLLBACK_EOF\r\n"); // marker-collision hazard
        raw.push('\x03'); // stray Ctrl-C
        raw.push_str("final line without newline");

        let log_path = format!("/tmp/ab-itest-log-{}", std::process::id());
        std::fs::write(&log_path, &raw).expect("write log");

        inject_scrollback(&session, &log_path, 500).expect("inject_scrollback");

        // Run a sentinel command; if the heredoc had wedged the shell, this
        // would be swallowed and never produce output.
        send_command(&session, "echo AB_ITEST_SENTINEL_OK").expect("send sentinel");
        thread::sleep(Duration::from_millis(600));

        let visible = capture_visible_pane(&session).unwrap_or_default();
        let scrollback = capture_last_lines(&session, 400).unwrap_or_default();

        let _ = kill_session(&session);
        let _ = std::fs::remove_file(&log_path);

        // The sentinel must have actually executed (output line present)...
        assert!(
            scrollback.contains("AB_ITEST_SENTINEL_OK"),
            "sentinel command did not run — shell was wedged.\nvisible:\n{visible}"
        );
        // ...and the shell must not be sitting at a heredoc continuation prompt.
        let last = visible
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        assert!(
            !last.trim_start().starts_with('>'),
            "shell stuck at heredoc continuation prompt: {last:?}"
        );
    }
}
