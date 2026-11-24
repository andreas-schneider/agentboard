//! Install Agentboard's board skill where supported agent CLIs discover it.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

const SKILL_NAME: &str = "agentboard-board";
const SKILL: &str = include_str!("../skills/agentboard-board/SKILL.md");
const MANAGED_MARKER: &str = ".agentboard-managed";

/// Prepare every supported CLI when the human opens the board, including
/// agents the user may launch manually in an existing tmux session.
pub fn install_board_skills() -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory for agent skills")?;
    install_board_skills_at(&home)
}

pub fn install_board_skill(agent_name: &str) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory for agent skills")?;
    install_board_skill_at(&home, agent_name)
}

fn install_board_skills_at(home: &Path) -> Result<()> {
    install_in(&home.join(".agents/skills"))?;
    install_in(&home.join(".kiro/skills"))
}

fn install_board_skill_at(home: &Path, agent_name: &str) -> Result<()> {
    let parent = match agent_name {
        "codex" | "copilot" => home.join(".agents/skills"),
        "kiro-cli" => home.join(".kiro/skills"),
        other => bail!("unsupported agent CLI for board skill: {other}"),
    };
    install_in(&parent)
}

fn install_in(parent: &Path) -> Result<()> {
    let directory = parent.join(SKILL_NAME);
    let skill_path = directory.join("SKILL.md");
    let marker_path = directory.join(MANAGED_MARKER);

    if directory.exists() && !marker_path.exists() {
        let same_content = fs::read_to_string(&skill_path)
            .map(|contents| contents == SKILL)
            .unwrap_or(false);
        if same_content {
            return Ok(());
        }
        bail!(
            "skill directory {} already exists and is not managed by Agentboard",
            directory.display()
        );
    }

    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create skill directory {}", directory.display()))?;
    if fs::read_to_string(&skill_path).ok().as_deref() != Some(SKILL) {
        fs::write(&skill_path, SKILL)
            .with_context(|| format!("failed to write skill {}", skill_path.display()))?;
    }
    if !marker_path.exists() {
        fs::write(&marker_path, "Managed by Agentboard.\n")
            .with_context(|| format!("failed to mark skill {}", directory.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_skill_for_all_supported_agents_and_refreshes_managed_copies() {
        let home = tempfile::tempdir().unwrap();
        install_board_skills_at(home.path()).unwrap();
        for parent in [".agents/skills", ".kiro/skills"] {
            let directory = home.path().join(parent).join(SKILL_NAME);
            assert_eq!(
                fs::read_to_string(directory.join("SKILL.md")).unwrap(),
                SKILL
            );
            fs::write(directory.join("SKILL.md"), "stale").unwrap();
        }
        install_board_skills_at(home.path()).unwrap();
        for parent in [".agents/skills", ".kiro/skills"] {
            assert_eq!(
                fs::read_to_string(home.path().join(parent).join(SKILL_NAME).join("SKILL.md"))
                    .unwrap(),
                SKILL
            );
        }
    }

    #[test]
    fn leaves_unmanaged_skill_untouched() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".agents/skills").join(SKILL_NAME);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("SKILL.md"), "user skill").unwrap();
        assert!(install_board_skill_at(home.path(), "codex").is_err());
        assert_eq!(
            fs::read_to_string(directory.join("SKILL.md")).unwrap(),
            "user skill"
        );
    }

    #[test]
    fn accepts_matching_unmanaged_skill_without_claiming_it() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".agents/skills").join(SKILL_NAME);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("SKILL.md"), SKILL).unwrap();
        install_board_skill_at(home.path(), "codex").unwrap();
        assert!(!directory.join(MANAGED_MARKER).exists());
    }

    #[test]
    fn installing_one_agent_does_not_touch_another_agents_directory() {
        let home = tempfile::tempdir().unwrap();
        install_board_skill_at(home.path(), "kiro-cli").unwrap();
        assert!(!home.path().join(".agents/skills").exists());
    }
}
