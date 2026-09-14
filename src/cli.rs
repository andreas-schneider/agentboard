use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ab", about = "Agentboard - AI agent task orchestrator")]
pub struct Cli {
    /// Select the agent CLI for this invocation.
    #[arg(long, global = true, value_parser = ["kiro-cli", "codex", "copilot"])]
    pub agent: Option<String>,

    /// Select the model for this invocation, overriding configuration.
    #[arg(long, global = true)]
    pub model: Option<String>,

    /// Select the unattended agent profile for this invocation.
    #[arg(long, global = true, conflicts_with = "profile")]
    pub yolo: bool,

    /// Override the configured agent profile for this invocation.
    #[arg(long, global = true, value_parser = ["interactive", "unattended"])]
    pub profile: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage the user configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Create a new task
    New {
        /// Task title/description
        #[arg()]
        title: String,

        /// Working directory (Git directories use task worktrees; defaults to current directory)
        #[arg(short = 'r', long = "dir", visible_alias = "repo")]
        working_directory: Option<String>,

        /// Detailed description/prompt for the agent (if different from title)
        #[arg(short, long)]
        description: Option<String>,

        /// Give the task Agentboard board/task-management context
        #[arg(long)]
        meta: bool,
    },

    /// Start a task - creates a Git worktree when applicable, then spawns an agent
    Start {
        /// Task ID (or unique prefix)
        #[arg()]
        task_id: String,

        /// Create the worktree without running repository setup
        #[arg(long)]
        skip_setup: bool,
    },

    /// List all tasks
    List,

    /// Attach to a running task's tmux session
    Attach {
        /// Task ID (or unique prefix)
        #[arg()]
        task_id: String,
    },

    /// Open the TUI kanban board
    Board {
        /// Filter to a specific repository path
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Kill a running task's agent session
    Kill {
        /// Task ID (or unique prefix)
        #[arg()]
        task_id: String,
    },

    /// Show details of a task
    Show {
        /// Task ID (or unique prefix)
        #[arg()]
        task_id: String,
    },

    /// Mark a task as done (session and worktree preserved)
    Done {
        /// Task ID (or unique prefix)
        #[arg()]
        task_id: String,
    },

    /// Edit a backlog task's title, description, or working directory
    Edit {
        /// Task ID (or unique prefix)
        #[arg()]
        task_id: String,

        /// New title
        #[arg(short, long)]
        title: Option<String>,

        /// New working directory
        #[arg(short = 'r', long = "dir", visible_alias = "repo")]
        working_directory: Option<String>,

        /// New description/prompt for the agent
        #[arg(short, long)]
        description: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Create a commented configuration template
    Init,
    /// Display the effective agent and safety profile
    Show,
    /// Parse and validate the configuration
    Check,
}
