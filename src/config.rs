//! User configuration for agent command selection and safety profiles.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::store::TaskStatus;

const TEMPLATE: &str = r#"# Agentboard configuration
# This file is user-owned and is never read from a repository.
agent = "kiro-cli"
profile = "interactive"

# Board lanes are listed in the order they should appear. Omit a lane to hide
# it; hidden tasks remain stored and can be restored with the board's `V` key.
# [board]
# lanes = ["Backlog", "Running", "Blocked", "Review", "Done"]

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
# Add "--autopilot" to both lists to let Copilot continue until it completes the task.
# Keep it opt-in: --no-ask-user only suppresses clarifying questions.
args = ["-C", ".", "--allow-all", "--no-ask-user", "--interactive"]
resume_args = ["-C", ".", "--allow-all", "--no-ask-user", "--continue"]
"#;

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub agent: Option<String>,
    pub profile: Option<String>,
    #[serde(default)]
    pub agents: HashMap<String, AgentOptions>,
    #[serde(default)]
    pub profiles: HashMap<String, HashMap<String, AgentOptions>>,
    #[serde(default)]
    pub board: BoardConfig,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct BoardConfig {
    /// Ordered visible lanes. When omitted, the board uses its built-in default.
    pub lanes: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct AgentOptions {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub resume_args: Option<Vec<String>>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub agent: String,
    pub profile: String,
    pub program: String,
    pub args: Vec<String>,
    pub resume_args: Vec<String>,
}

#[derive(Debug, Default)]
struct CliOverrides {
    agent: Option<String>,
    model: Option<String>,
}

static CLI_OVERRIDES: OnceLock<CliOverrides> = OnceLock::new();

pub fn set_cli_overrides(agent: Option<String>, model: Option<String>) {
    let _ = CLI_OVERRIDES.set(CliOverrides { agent, model });
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("agentboard").join("config.toml"))
}

/// Return whether a configured executable can be found on the current PATH.
pub fn program_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return is_executable_file(path);
    }

    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(program))
        .any(|candidate| is_executable_file(&candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

pub fn load() -> Result<FileConfig> {
    let Some(path) = path() else {
        return builtin_config();
    };
    if !path.exists() {
        return builtin_config();
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn effective(config: &FileConfig, requested_agent: Option<&str>) -> Result<EffectiveConfig> {
    let agent = requested_agent
        .map(str::to_owned)
        .or_else(|| CLI_OVERRIDES.get().and_then(|o| o.agent.clone()))
        .or_else(|| std::env::var("AGENTBOARD_AGENT").ok())
        .or_else(|| config.agent.clone())
        .unwrap_or_else(|| "kiro-cli".to_owned());
    let profile = std::env::var("AGENTBOARD_PROFILE")
        .ok()
        .or_else(|| config.profile.clone())
        .unwrap_or_else(|| "interactive".to_owned());

    let builtin = builtin_config()?;
    let builtin_agent_opts = builtin.agents.get(&agent).cloned().unwrap_or_default();
    let builtin_profile_opts = builtin
        .profiles
        .get(&profile)
        .and_then(|agents| agents.get(&agent))
        .cloned()
        .unwrap_or_default();
    let profile_opts = config
        .profiles
        .get(&profile)
        .and_then(|agents| agents.get(&agent))
        .cloned()
        .unwrap_or_default();
    let agent_opts = config.agents.get(&agent).cloned().unwrap_or_default();

    let mut args = agent_opts
        .args
        .or(profile_opts.args)
        .or(builtin_agent_opts.args)
        .or(builtin_profile_opts.args)
        .with_context(|| format!("no arguments configured for agent/profile {agent}/{profile}"))?;
    let mut resume_args = agent_opts
        .resume_args
        .or(profile_opts.resume_args)
        .or(builtin_agent_opts.resume_args)
        .or(builtin_profile_opts.resume_args)
        .with_context(|| {
            format!("no resume arguments configured for agent/profile {agent}/{profile}")
        })?;
    let model = CLI_OVERRIDES
        .get()
        .and_then(|o| o.model.clone())
        .or_else(|| agent_opts.model.or(profile_opts.model));
    if let Some(model) = &model {
        if !args.iter().any(|arg| arg == "--model") {
            args.extend(["--model".into(), model.clone()]);
        }
        if !resume_args.iter().any(|arg| arg == "--model") {
            resume_args.extend(["--model".into(), model.clone()]);
        }
    }

    Ok(EffectiveConfig {
        agent: agent.clone(),
        profile: profile.clone(),
        program: agent_opts
            .program
            .or(profile_opts.program)
            .or(builtin_agent_opts.program)
            .or(builtin_profile_opts.program)
            .with_context(|| {
                format!("no program configured for agent/profile {agent}/{profile}")
            })?,
        args,
        resume_args,
    })
}

fn builtin_config() -> Result<FileConfig> {
    toml::from_str(TEMPLATE).context("built-in configuration is invalid")
}

/// Resolve the effective configuration for an agent/profile using only the
/// built-in (shipped) defaults, ignoring any user configuration file.
///
/// This is primarily useful for tests that need to assert the exact commands
/// produced by the shipped defaults without depending on the ambient
/// `~/.config/agentboard/config.toml`.
#[cfg(test)]
pub fn builtin_effective(agent: &str, profile: &str) -> Result<EffectiveConfig> {
    let builtin = builtin_config()?;
    let agent_opts = builtin.agents.get(agent).cloned().unwrap_or_default();
    let profile_opts = builtin
        .profiles
        .get(profile)
        .and_then(|agents| agents.get(agent))
        .cloned()
        .unwrap_or_default();

    let args = agent_opts
        .args
        .clone()
        .or_else(|| profile_opts.args.clone())
        .with_context(|| format!("no arguments configured for agent/profile {agent}/{profile}"))?;
    let resume_args = agent_opts
        .resume_args
        .clone()
        .or_else(|| profile_opts.resume_args.clone())
        .with_context(|| {
            format!("no resume arguments configured for agent/profile {agent}/{profile}")
        })?;
    let program = agent_opts
        .program
        .or(profile_opts.program)
        .with_context(|| format!("no program configured for agent/profile {agent}/{profile}"))?;

    Ok(EffectiveConfig {
        agent: agent.to_owned(),
        profile: profile.to_owned(),
        program,
        args,
        resume_args,
    })
}

pub fn init() -> Result<PathBuf> {
    let path = path().context("cannot determine the user configuration directory")?;
    if path.exists() {
        anyhow::bail!("configuration already exists at {}", path.display());
    }
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    fs::write(&path, TEMPLATE)?;
    Ok(path)
}

/// Resolve the ordered visible lanes for the board.
///
/// Only built-in statuses are accepted for now. This provides a visibility
/// and ordering seam without pretending that custom workflow stages exist.
pub fn board_lanes(config: &FileConfig) -> Result<Vec<TaskStatus>> {
    let Some(names) = &config.board.lanes else {
        return Ok(default_board_lanes());
    };

    if names.is_empty() {
        anyhow::bail!("[board].lanes must contain at least one lane");
    }

    let mut lanes = Vec::with_capacity(names.len());
    for name in names {
        let lane = name.parse::<TaskStatus>().with_context(|| {
            format!(
                "invalid board lane '{name}'; expected one of {}",
                TaskStatus::all_columns().join(", ")
            )
        })?;
        if lanes.contains(&lane) {
            anyhow::bail!("[board].lanes contains duplicate lane '{name}'");
        }
        lanes.push(lane);
    }
    Ok(lanes)
}

/// Persist the board's visible lane order while preserving the rest of the
/// user's configuration. The board editor is the primary interface; the TOML
/// representation is kept as a portable advanced setting.
pub fn save_board_lanes(lanes: &[TaskStatus]) -> Result<PathBuf> {
    if lanes.is_empty() {
        anyhow::bail!("at least one board lane must remain visible");
    }

    let path = path().context("cannot determine the user configuration directory")?;
    let text = if path.exists() {
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        TEMPLATE.to_owned()
    };
    let names = lanes
        .iter()
        .map(|lane| format!("\"{lane}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let board_block = format!("[board]\nlanes = [{names}]\n");
    let updated = replace_board_section(&text, &board_block);

    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    fs::write(&path, updated).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn replace_board_section(text: &str, replacement: &str) -> String {
    let mut output = String::new();
    let mut in_board = false;
    let mut replaced = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[board]" {
            if !replaced {
                output.push_str(replacement);
                replaced = true;
            }
            in_board = true;
            continue;
        }
        if in_board && trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_board = false;
        }
        if !in_board {
            output.push_str(line);
            output.push('\n');
        }
    }

    if !replaced {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(replacement);
    }
    output
}

pub fn default_board_lanes() -> Vec<TaskStatus> {
    vec![
        TaskStatus::Backlog,
        TaskStatus::Running,
        TaskStatus::Blocked,
        TaskStatus::Review,
        TaskStatus::Done,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_defaults_keep_approval_and_sandboxing() {
        let config = builtin_config().unwrap();
        let profile = config.profiles["interactive"]["codex"]
            .args
            .as_ref()
            .unwrap();
        assert!(profile.contains(&"on-request".into()));
        assert!(profile.contains(&"workspace-write".into()));
        assert!(!profile.contains(&"danger-full-access".into()));
    }

    #[test]
    fn unattended_profile_is_explicit() {
        let config = builtin_config().unwrap();
        let profile = config.profiles["unattended"]["kiro-cli"]
            .args
            .as_ref()
            .unwrap();
        assert!(profile.contains(&"--trust-all-tools".into()));

        let copilot_profile = config.profiles["unattended"]["copilot"]
            .args
            .as_ref()
            .unwrap();
        assert!(copilot_profile.contains(&"--allow-all".into()));
        assert!(copilot_profile.contains(&"--no-ask-user".into()));
    }

    #[test]
    fn custom_arguments_are_preserved() {
        let mut config = FileConfig {
            agent: Some("codex".into()),
            profile: Some("interactive".into()),
            ..Default::default()
        };
        config.agents.insert(
            "codex".into(),
            AgentOptions {
                args: Some(vec!["--custom".into(), "value".into()]),
                ..Default::default()
            },
        );
        let effective = effective(&config, None).unwrap();
        assert_eq!(effective.args, vec!["--custom", "value"]);
    }

    #[test]
    fn shipped_template_parses() {
        let config: FileConfig = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(config.agent.as_deref(), Some("kiro-cli"));
        assert_eq!(config.profile.as_deref(), Some("interactive"));
        assert!(config.profiles.contains_key("interactive"));
        assert!(config.profiles.contains_key("unattended"));
    }

    #[test]
    fn board_lanes_default_to_current_board() {
        let config = FileConfig::default();
        assert_eq!(board_lanes(&config).unwrap(), default_board_lanes());
    }

    #[test]
    fn board_lanes_preserve_configured_order_and_visibility() {
        let config: FileConfig = toml::from_str(
            r#"
            [board]
            lanes = ["Running", "Blocked", "Backlog"]
            "#,
        )
        .unwrap();
        assert_eq!(
            board_lanes(&config).unwrap(),
            vec![
                TaskStatus::Running,
                TaskStatus::Blocked,
                TaskStatus::Backlog
            ]
        );
    }

    #[test]
    fn board_lanes_reject_unknown_and_duplicate_lanes() {
        let unknown: FileConfig = toml::from_str(
            r#"
            [board]
            lanes = ["NotALane"]
            "#,
        )
        .unwrap();
        assert!(board_lanes(&unknown).is_err());

        let duplicate: FileConfig = toml::from_str(
            r#"
            [board]
            lanes = ["Backlog", "Backlog"]
            "#,
        )
        .unwrap();
        assert!(board_lanes(&duplicate).is_err());
    }

    #[test]
    fn replace_board_section_preserves_other_configuration() {
        let input = "agent = \"codex\"\n\n[board]\nlanes = [\"Done\"]\n\n[agents.codex]\nprogram = \"codex\"\n";
        let output = replace_board_section(input, "[board]\nlanes = [\"Backlog\"]\n");
        assert!(output.contains("agent = \"codex\""));
        assert!(output.contains("[agents.codex]"));
        assert!(output.contains("lanes = [\"Backlog\"]"));
        assert!(!output.contains("lanes = [\"Done\"]"));
    }

    #[test]
    fn missing_program_is_reported() {
        assert!(!program_available("agentboard-program-that-does-not-exist"));
    }
}
