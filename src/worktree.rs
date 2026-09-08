use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

/// Validate that `repo_path` is inside a git repository.
///
/// Runs `git rev-parse --git-dir` in the given directory. Returns `Ok(())`
/// if the path is a valid git repo (or inside one), or an actionable error
/// message if not.
pub fn validate_git_repo(repo_path: &str) -> Result<()> {
    let path = std::path::Path::new(repo_path);
    if !path.exists() {
        anyhow::bail!(
            "Repository path '{}' does not exist. Check the --repo flag or current directory.",
            repo_path
        );
    }
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(repo_path)
        .output()
        .context("failed to run git — is git installed?")?;

    if !output.status.success() {
        anyhow::bail!(
            "'{}' is not a git repository. Use --repo to specify a valid git repo path.",
            repo_path
        );
    }
    Ok(())
}

/// Create a git worktree for the given task inside `repo_path/.agentboard-worktrees/<task_id>/`.
///
/// Runs `git worktree add <path> -b <branch_name>`. If the branch already exists,
/// falls back to `git worktree add <path> <branch_name>` (without `-b`).
///
/// Returns the absolute worktree path as a string.
pub fn create_worktree(repo_path: &str, task_id: &str, branch_name: &str) -> Result<String> {
    let base_dir = PathBuf::from(repo_path).join(".agentboard-worktrees");
    fs::create_dir_all(&base_dir)
        .with_context(|| format!("Failed to create worktree base dir: {}", base_dir.display()))?;

    let worktree_path = base_dir.join(task_id);
    let worktree_str = worktree_path
        .to_str()
        .context("Worktree path contains invalid UTF-8")?
        .to_string();

    // Try creating a new branch first.
    let output = Command::new("git")
        .args(["worktree", "add", &worktree_str, "-b", branch_name])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git worktree add")?;

    if output.status.success() {
        return Ok(worktree_str);
    }

    // Branch likely already exists — retry without -b.
    let output = Command::new("git")
        .args(["worktree", "add", &worktree_str, branch_name])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git worktree add (existing branch)")?;

    if output.status.success() {
        return Ok(worktree_str);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("git worktree add failed: {stderr}");
}

/// Remove a git worktree. Errors are intentionally ignored because the worktree
/// may have already been cleaned up.
pub fn cleanup_worktree(repo_path: &str, worktree_path: &str) -> Result<()> {
    let _ = Command::new("git")
        .args(["worktree", "remove", worktree_path, "--force"])
        .current_dir(repo_path)
        .output();

    Ok(())
}

/// Check if a branch has unmerged work relative to the main branch.
/// Returns true if the branch contains commits not merged into HEAD of the main branch.
pub fn has_unmerged_work(repo_path: &str, branch_name: &str) -> bool {
    // Use `git log <main>..<branch> --oneline` to check for unmerged commits.
    // We need to figure out the main branch name first (could be main or master).
    // Try 'main' first, fall back to 'master', fall back to 'HEAD'.
    let main_ref = get_main_branch(repo_path).unwrap_or_else(|| "HEAD".to_string());
    let range = format!("{}..{}", main_ref, branch_name);
    let output = Command::new("git")
        .args(["log", &range, "--oneline"])
        .current_dir(repo_path)
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            !stdout.trim().is_empty()
        }
        Err(_) => false, // If we can't check, assume no unmerged work
    }
}

/// Delete a local git branch.
pub fn delete_branch(repo_path: &str, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", "-D", branch_name])
        .current_dir(repo_path)
        .output()
        .context("failed to execute git branch -D")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git branch -D failed: {stderr}");
    }
    Ok(())
}

/// Try to determine the main branch name for a repo.
fn get_main_branch(repo_path: &str) -> Option<String> {
    // Check if 'main' exists
    for name in &["main", "master"] {
        let output = Command::new("git")
            .args(["rev-parse", "--verify", name])
            .current_dir(repo_path)
            .output()
            .ok()?;
        if output.status.success() {
            return Some(name.to_string());
        }
    }
    None
}

/// Generate a branch name in the format `ab/<short_id>/<slugified_title>`.
///
/// - `short_id` is the first 8 characters of `task_id`.
/// - The title is lowercased, non-alphanumeric characters are replaced with hyphens,
///   consecutive hyphens are collapsed, leading/trailing hyphens are trimmed, and the
///   result is truncated to 50 characters.
pub fn generate_branch_name(task_id: &str, title: &str) -> String {
    let short_id: String = task_id.chars().take(8).collect();

    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push('-');
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }

    // Trim leading/trailing hyphens, then truncate to 50 chars.
    let trimmed = collapsed.trim_matches('-');
    let truncated: String = trimmed.chars().take(50).collect();
    let truncated = truncated.trim_end_matches('-');

    // Fall back to "task" when the slug is empty (e.g., all-punctuation or
    // non-ASCII titles that produce no ASCII-alphanumeric characters).
    let slug_final = if truncated.is_empty() {
        "task"
    } else {
        truncated
    };

    format!("ab/{short_id}/{slug_final}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_branch_name_basic() {
        let result = generate_branch_name("abcd1234efgh", "Fix the login bug");
        assert_eq!(result, "ab/abcd1234/fix-the-login-bug");
    }

    #[test]
    fn test_generate_branch_name_special_chars() {
        let result = generate_branch_name("task5678xxxx", "Hello, World! (test)");
        assert_eq!(result, "ab/task5678/hello-world-test");
    }

    #[test]
    fn test_generate_branch_name_collapses_hyphens() {
        let result = generate_branch_name("aaaabbbb", "foo---bar___baz");
        assert_eq!(result, "ab/aaaabbbb/foo-bar-baz");
    }

    #[test]
    fn test_generate_branch_name_trims_hyphens() {
        let result = generate_branch_name("12345678", "  --hello--  ");
        assert_eq!(result, "ab/12345678/hello");
    }

    #[test]
    fn test_generate_branch_name_truncates_to_50() {
        let long_title = "a".repeat(100);
        let result = generate_branch_name("longid12", &long_title);
        // "ab/longid12/" prefix + 50 chars of slug
        let slug_part = result.strip_prefix("ab/longid12/").unwrap();
        assert_eq!(slug_part.len(), 50);
    }

    #[test]
    fn test_generate_branch_name_short_task_id() {
        let result = generate_branch_name("abc", "my task");
        assert_eq!(result, "ab/abc/my-task");
    }

    #[test]
    fn test_generate_branch_name_truncation_trims_trailing_hyphen() {
        // Build a title that, after slugification and truncation to 50 chars,
        // would end with a hyphen.
        let title = format!("{}-x", "a".repeat(49));
        let result = generate_branch_name("12345678", &title);
        let slug_part = result.strip_prefix("ab/12345678/").unwrap();
        assert!(
            !slug_part.ends_with('-'),
            "slug should not end with hyphen: {slug_part}"
        );
    }

    #[test]
    fn test_generate_branch_name_empty_title_falls_back_to_task() {
        let result = generate_branch_name("12345678", "");
        assert_eq!(result, "ab/12345678/task");
    }

    #[test]
    fn test_generate_branch_name_all_special_chars_falls_back_to_task() {
        let result = generate_branch_name("12345678", "!!!");
        assert_eq!(result, "ab/12345678/task");
    }

    #[test]
    fn test_generate_branch_name_non_ascii_falls_back_to_task() {
        let result = generate_branch_name("12345678", "日本語タスク");
        assert_eq!(result, "ab/12345678/task");
    }

    /// Helper: create a temporary git repo and return its path.
    fn make_temp_repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let repo_path = dir.path().to_str().unwrap().to_string();
        // git init
        let out = Command::new("git")
            .args(["init"])
            .current_dir(&repo_path)
            .output()
            .expect("git init failed");
        assert!(out.status.success(), "git init failed");
        // Configure user for commits
        let _ = Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo_path)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo_path)
            .output();
        // Create an initial commit on main
        let _ = Command::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(&repo_path)
            .output();
        std::fs::write(dir.path().join("README.md"), "# test").unwrap();
        let _ = Command::new("git")
            .args(["add", "."])
            .current_dir(&repo_path)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo_path)
            .output();
        (dir, repo_path)
    }

    #[test]
    fn test_has_unmerged_work_no_extra_commits() {
        let (_dir, repo_path) = make_temp_repo();
        // Create a branch at the same commit as main — no unmerged work
        let _ = Command::new("git")
            .args(["branch", "feature-a"])
            .current_dir(&repo_path)
            .output();
        assert!(!has_unmerged_work(&repo_path, "feature-a"));
    }

    #[test]
    fn test_has_unmerged_work_with_extra_commits() {
        let (_dir, repo_path) = make_temp_repo();
        // Create a branch and add a commit
        let _ = Command::new("git")
            .args(["checkout", "-b", "feature-b"])
            .current_dir(&repo_path)
            .output();
        std::fs::write(std::path::Path::new(&repo_path).join("new.txt"), "hello").unwrap();
        let _ = Command::new("git")
            .args(["add", "."])
            .current_dir(&repo_path)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-m", "feature commit"])
            .current_dir(&repo_path)
            .output();
        // Switch back to main
        let _ = Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&repo_path)
            .output();
        assert!(has_unmerged_work(&repo_path, "feature-b"));
    }

    #[test]
    fn test_has_unmerged_work_nonexistent_branch() {
        let (_dir, repo_path) = make_temp_repo();
        // Non-existent branch should return false (git log will fail)
        assert!(!has_unmerged_work(&repo_path, "no-such-branch"));
    }

    #[test]
    fn test_delete_branch_success() {
        let (_dir, repo_path) = make_temp_repo();
        // Create a branch, stay on main, then delete it
        let _ = Command::new("git")
            .args(["branch", "to-delete"])
            .current_dir(&repo_path)
            .output();
        assert!(delete_branch(&repo_path, "to-delete").is_ok());
        // Verify branch is gone
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "to-delete"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        assert!(!out.status.success());
    }

    #[test]
    fn test_delete_branch_nonexistent() {
        let (_dir, repo_path) = make_temp_repo();
        // Deleting a non-existent branch should fail
        assert!(delete_branch(&repo_path, "no-such-branch").is_err());
    }

    #[test]
    fn test_validate_git_repo_valid() {
        let (_dir, repo_path) = make_temp_repo();
        assert!(validate_git_repo(&repo_path).is_ok());
    }

    #[test]
    fn test_validate_git_repo_not_a_repo() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let path = dir.path().to_str().unwrap().to_string();
        let err = validate_git_repo(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not a git repository"),
            "expected git repo error, got: {msg}"
        );
    }

    #[test]
    fn test_validate_git_repo_nonexistent_path() {
        let err = validate_git_repo("/tmp/nonexistent-agentboard-test-path-12345").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does not exist"),
            "expected path error, got: {msg}"
        );
    }
}
