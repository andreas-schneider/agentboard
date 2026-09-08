//! Repository-local worktree setup configuration.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const CONFIG_NAME: &str = "setup.toml";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SetupConfig {
    #[serde(default)]
    pub steps: Vec<String>,
}

/// Resolve setup from the repo-local config and the optional per-machine
/// fallback. The repo-local config wins when both exist.
pub fn resolve(repo_path: &str) -> Result<Option<SetupConfig>> {
    let repo_config = load_config(
        &PathBuf::from(repo_path)
            .join(".agentboard")
            .join(CONFIG_NAME),
    )?;
    let global_config = global_config_path(repo_path)
        .and_then(|path| load_config(&path).transpose())
        .transpose()?;

    Ok(repo_config
        .or(global_config)
        .filter(|config| !config.steps.is_empty()))
}

/// Build the shell command that runs setup in a task's worktree and then
/// launches the agent command. The setup marker prevents completion detection
/// from mistaking a long-running install/build for a finished agent.
pub fn command(config: &SetupConfig, agent_command: &str) -> String {
    let mut command =
        String::from("mkdir -p .agentboard; touch .agentboard/setup.running; setup_failed=0;");
    for step in &config.steps {
        command.push_str(" if [ \"$setup_failed\" -eq 0 ]; then ( ");
        command.push_str(step);
        command.push_str(" ) || setup_failed=$?; fi;");
    }
    command.push_str(" rm -f .agentboard/setup.running; if [ \"$setup_failed\" -ne 0 ]; then echo AB_SETUP_FAILED:$setup_failed; exec bash; fi; ");
    command.push_str(agent_command);
    command
}

/// Return the private config location for a repository. The path is encoded
/// using the full canonical path so repositories with similar names cannot
/// collide.
fn global_config_path(repo_path: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let canonical = fs::canonicalize(repo_path).unwrap_or_else(|_| PathBuf::from(repo_path));
    let id = canonical
        .to_string_lossy()
        .trim_start_matches(std::path::MAIN_SEPARATOR)
        .replace(std::path::MAIN_SEPARATOR, "__");
    Some(
        home.join(".agentboard")
            .join("setup")
            .join(format!("{id}.toml")),
    )
}

fn load_config(path: &Path) -> Result<Option<SetupConfig>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read setup config {}", path.display()))?;
    let config = toml::from_str(&contents)
        .with_context(|| format!("failed to parse setup config {}", path.display()))?;
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_steps() {
        let config: SetupConfig = toml::from_str(
            r#"
            steps = ["pnpm install", "pnpm build"]
            "#,
        )
        .unwrap();
        assert_eq!(config.steps, vec!["pnpm install", "pnpm build"]);
    }

    #[test]
    fn setup_command_runs_steps_before_agent() {
        let config = SetupConfig {
            steps: vec!["echo one".into(), "echo two".into()],
        };
        let command = command(&config, "agent --prompt");
        let syntax = std::process::Command::new("bash")
            .args(["-n", "-c", &command])
            .status()
            .unwrap();
        assert!(
            syntax.success(),
            "generated setup command must be valid shell"
        );
        assert!(command.find("echo one").unwrap() < command.find("echo two").unwrap());
        assert!(command.find("echo two").unwrap() < command.find("agent --prompt").unwrap());
        assert!(command.contains(".agentboard/setup.running"));
    }
}
