mod agent;
mod cli;
mod config;
mod orchestrator;
mod prompts;
mod schedule;
mod setup;
mod skills;
mod store;
mod tmux;
mod tui;
mod worktree;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use store::TaskStore;

fn main() -> Result<()> {
    let cli = Cli::parse();
    config::set_cli_overrides(cli.agent.clone(), cli.model.clone());

    if cli.yolo {
        std::env::set_var("AGENTBOARD_PROFILE", "unattended");
    } else if let Some(profile) = &cli.profile {
        std::env::set_var("AGENTBOARD_PROFILE", profile);
    }

    if let Commands::Config { command } = cli.command {
        match command {
            cli::ConfigCommands::Init => {
                println!("Created {}", config::init()?.display());
            }
            cli::ConfigCommands::Show => {
                let cfg = config::load()?;
                let board_lanes = config::board_lanes(&cfg)?;
                let effective = config::effective(&cfg, None)?;
                println!(
                    "Config: {}",
                    config::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(unavailable)".into())
                );
                println!(
                    "Agent: {}\nProfile: {}\nProgram: {}\nArgs: {:?}\nResume args: {:?}",
                    effective.agent,
                    effective.profile,
                    effective.program,
                    effective.args,
                    effective.resume_args
                );
                println!("Board lanes: {:?}", board_lanes);
                if effective.profile == "unattended" {
                    println!(
                        "WARNING: unattended mode disables normal agent approvals or sandboxing."
                    );
                }
            }
            cli::ConfigCommands::Check => {
                let cfg = config::load()?;
                config::board_lanes(&cfg)?;
                let effective = config::effective(&cfg, None)?;
                println!(
                    "Configuration is valid ({} / {}).",
                    effective.agent, effective.profile
                );
            }
        }
        return Ok(());
    }

    let store = TaskStore::open()?;

    match cli.command {
        Commands::Config { .. } => unreachable!("configuration commands are handled above"),
        Commands::New {
            title,
            working_directory,
            description,
            meta,
            at,
        } => {
            let desc = description.as_deref().unwrap_or(&title);
            let workspace = match working_directory {
                Some(path) => path,
                None if meta => orchestrator::suggested_meta_workspace()?,
                None => std::env::current_dir()?.to_string_lossy().into_owned(),
            };
            let schedule = schedule::prepare(at.as_deref().unwrap_or_default())?;
            let task = store.create_scheduled_task(&title, desc, &workspace, meta, schedule)?;
            println!("Created task: {} ({})", task.title, &task.id[..8]);
            if let Some(schedule) = task.schedule {
                println!(
                    "Starts {} · {} / {}",
                    schedule::display(schedule.at),
                    schedule.config.agent,
                    schedule.config.profile
                );
            }
        }
        Commands::Schedule { task_id, at, clear } => {
            let task = store.get_task(&task_id)?;
            anyhow::ensure!(
                task.status == store::TaskStatus::Backlog,
                "Only backlog tasks can be scheduled"
            );
            if clear {
                store.set_schedule(&task.id, None)?;
                println!("Cleared scheduled start for {}", &task.id[..8]);
            } else {
                let schedule = schedule::prepare(at.as_deref().unwrap_or_default())?
                    .ok_or_else(|| anyhow::anyhow!("Specify a future start time"))?;
                store.set_schedule(&task.id, Some(&schedule))?;
                println!(
                    "Starts {} · {} / {}",
                    schedule::display(schedule.at),
                    schedule.config.agent,
                    schedule.config.profile
                );
            }
        }
        Commands::Scheduler { once } => schedule::run(&store, once)?,
        Commands::Start {
            task_id,
            skip_setup,
        } => {
            let task = store.get_task(&task_id)?;
            if task.schedule.is_none() {
                check_agent_program()?;
            }
            let updated = orchestrator::start_task_with_options(&store, &task, skip_setup)?;
            println!(
                "Started task {} in tmux session '{}'",
                &updated.id[..8],
                updated.tmux_session.as_deref().unwrap_or("")
            );
            if let Some(ref branch) = updated.branch_name {
                println!("  Branch: {}", branch);
            }
            if let Some(ref wt) = updated.worktree_path {
                println!("  Worktree: {}", wt);
            }
            println!("  Attach with: ab attach {}", &updated.id[..8]);
        }
        Commands::List => {
            let tasks = store.list_tasks()?;
            if tasks.is_empty() {
                println!("No tasks. Create one with: ab new \"task description\"");
                return Ok(());
            }
            // Print a nicely formatted table
            println!(
                "{:<10} {:<12} {:<40} WORKING DIRECTORY",
                "ID", "STATUS", "TITLE"
            );
            println!("{}", "-".repeat(80));
            for task in &tasks {
                let short_id = &task.id[..8];
                let title: String = task.title.chars().take(35).collect();
                let title = if task.title.chars().count() > 38 {
                    format!("{}...", title)
                } else {
                    task.title.clone()
                };
                println!(
                    "{:<10} {:<12} {:<40} {}",
                    short_id,
                    match &task.schedule {
                        Some(schedule) if schedule.error.is_some() => "Start failed".into(),
                        Some(_) => "Scheduled".into(),
                        None => task.status.to_string(),
                    },
                    title,
                    task.repo_path
                );
            }
        }
        Commands::Attach { task_id } => {
            let task = store.get_task(&task_id)?;
            let session = task
                .tmux_session
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Task {} has no tmux session", &task.id[..8]))?;
            if !tmux::session_exists(session) {
                anyhow::bail!(
                    "Tmux session '{}' does not exist. Task may have finished.",
                    session
                );
            }
            println!("┌─────────────────────────────────────────────┐");
            println!("│  Attaching to session '{}'", session);
            println!("│  Detach (return here): Ctrl+B then D        │");
            println!("│  Switch windows: Ctrl+B then A/S            │");
            println!("│  ⚠ Do NOT press Esc/Ctrl+C (kills agent)    │");
            println!("└─────────────────────────────────────────────┘");
            tmux::ensure_task_windows(
                session,
                task.worktree_path.as_deref().unwrap_or(&task.repo_path),
            )?;
            tmux::attach_session(session)?;
        }
        Commands::Board { repo } => {
            if store.has_pending_schedules()? {
                if let Err(error) = schedule::ensure_worker() {
                    eprintln!("Could not restart scheduler: {error:#}");
                }
            }
            check_agent_program()?;
            // Resolve repo path to absolute if provided
            let repo_filter = repo.map(|r| {
                std::path::Path::new(&r)
                    .canonicalize()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or(r)
            });
            // Need to drop store before TUI takes over (TUI opens its own)
            drop(store);
            tui::run_board(repo_filter)?;
        }
        Commands::Kill { task_id } => {
            let task = store.get_task(&task_id)?;
            let updated = orchestrator::kill_task(&store, &task)?;
            println!("Killed task {} — marked as Blocked", &updated.id[..8]);
        }
        Commands::Show { task_id } => {
            let task = store.get_task(&task_id)?;
            println!("Task: {}", task.title);
            println!("ID: {}", task.id);
            println!("Status: {}", task.status);
            if let Some(schedule) = &task.schedule {
                println!(
                    "Scheduled start: {} · {} / {}",
                    schedule::display(schedule.at),
                    schedule.config.agent,
                    schedule.config.profile
                );
                if let Some(error) = &schedule.error {
                    println!("Scheduled launch failed: {error}");
                }
            }
            println!(
                "Agent: {}",
                task.agent_cli.as_deref().unwrap_or("not started")
            );
            println!("Working directory: {}", task.repo_path);
            if task.is_meta {
                println!("Target: Agentboard board (meta task)");
            }
            if let Some(ref branch) = task.branch_name {
                println!("Branch: {}", branch);
            }
            if let Some(ref wt) = task.worktree_path {
                println!("Worktree: {}", wt);
            }
            if let Some(ref session) = task.tmux_session {
                println!(
                    "Tmux: {} (alive: {})",
                    session,
                    tmux::session_exists(session)
                );
                if tmux::session_exists(session) {
                    println!("\n--- Last 15 lines of output ---");
                    match tmux::capture_last_lines(session, 15) {
                        Ok(lines) => println!("{}", lines),
                        Err(e) => println!("(could not capture: {})", e),
                    }
                }
            }
            println!("Created: {}", task.created_at);
            println!("Updated: {}", task.updated_at);
            println!("\nDescription:\n{}", task.description);
        }
        Commands::Done { task_id } => {
            let task = store.get_task(&task_id)?;
            let updated = orchestrator::done_task(&store, &task)?;
            if let Some(ref branch) = updated.branch_name {
                println!(
                    "Task {} marked as Done. Branch '{}' preserved.",
                    &updated.id[..8],
                    branch
                );
            } else {
                println!("Task {} marked as Done.", &updated.id[..8]);
            }
        }
        Commands::Edit {
            task_id,
            title,
            description,
            working_directory,
        } => {
            let task = store.get_task(&task_id)?;
            if task.status != store::TaskStatus::Backlog {
                anyhow::bail!(
                    "Can only edit Backlog tasks (task {} is {})",
                    &task.id[..8],
                    task.status
                );
            }
            let new_title = title.as_deref().unwrap_or(&task.title);
            let new_desc = description.as_deref().unwrap_or(&task.description);
            let new_repo = working_directory.as_deref().unwrap_or(&task.repo_path);
            store.update_task_details(&task.id, new_title, new_desc, new_repo)?;
            println!("Updated task {} ({})", new_title, &task.id[..8]);
        }
    }

    Ok(())
}

fn check_agent_program() -> Result<()> {
    let cfg = config::load()?;
    let effective = config::effective(&cfg, None)?;
    if !config::program_available(&effective.program) {
        let config_path = config::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "your user configuration file".to_owned());
        anyhow::bail!(
                "Configured agent CLI '{}' (agent '{}') was not found on PATH. Install it, make sure it is executable, or update '{}' ([agents.{}.program]). Currently supported CLIs are kiro-cli, codex, and copilot.",
                effective.program,
                effective.agent,
                config_path,
                effective.agent
            );
    }
    Ok(())
}
