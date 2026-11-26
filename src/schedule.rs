use std::fs::{File, OpenOptions};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, NaiveDateTime, TimeDelta, TimeZone};

use crate::{config, orchestrator, store::TaskStore};

#[derive(Clone, Debug)]
pub struct Schedule {
    pub at: i64,
    pub config: config::EffectiveConfig,
    pub error: Option<String>,
}

pub const PLACEHOLDER: &str = "YYYY-MM-DD HH:MM, in 30m, in 2h, tomorrow 09:00";
pub const QUICK_PICKS: [&str; 3] = ["in 30m", "in 2h", "tomorrow 09:00"];

fn parse_at(input: &str, now: DateTime<Local>) -> Result<DateTime<Local>> {
    let input = input.trim();
    let at = if let Some(relative) = input.strip_prefix("in ") {
        let (number, multiplier) = relative
            .strip_suffix('m')
            .map(|number| (number, 60))
            .or_else(|| relative.strip_suffix('h').map(|number| (number, 3600)))
            .context("Use minutes or hours, e.g. in 30m or in 2h")?;
        let seconds = number
            .trim()
            .parse::<i64>()?
            .checked_mul(multiplier)
            .context("Delay is out of range")?;
        now.checked_add_signed(TimeDelta::try_seconds(seconds).context("Delay is out of range")?)
            .context("Start time is out of range")?
    } else if let Ok(at) = DateTime::parse_from_rfc3339(input) {
        at.with_timezone(&Local)
    } else {
        let local = if let Some(time) = input.strip_prefix("tomorrow ") {
            now.date_naive()
                .succ_opt()
                .context("Date is out of range")?
                .and_time(
                    chrono::NaiveTime::parse_from_str(time, "%H:%M")
                        .context("Use tomorrow HH:MM")?,
                )
        } else {
            NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M")
                .context("Use YYYY-MM-DD HH:MM, a quick pick, or an ISO timestamp with offset")?
        };
        Local.from_local_datetime(&local).single().context(
            "This local time is ambiguous or skipped. Use an ISO timestamp with UTC offset.",
        )?
    };
    if at.timestamp() <= now.timestamp() {
        bail!("Start time must be in the future");
    }
    Ok(at)
}

pub fn display(at: i64) -> String {
    match Local.timestamp_opt(at, 0).single() {
        Some(at) => at.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        None => "Invalid start time".into(),
    }
}

pub fn preview(input: &str) -> Result<String> {
    let at = parse_at(input, Local::now())?;
    let cfg = config::effective(&config::load()?, None)?;
    Ok(format!(
        "{} · {} / {}",
        display(at.timestamp()),
        cfg.agent,
        cfg.profile
    ))
}

pub fn prepare(input: &str) -> Result<Option<Schedule>> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    let at = parse_at(input, Local::now())?.timestamp();
    let config = config::effective(&config::load()?, None)?;
    ensure_worker()?;
    Ok(Some(Schedule {
        at,
        config,
        error: None,
    }))
}

fn worker_dir() -> Result<std::path::PathBuf> {
    let dir = dirs::home_dir()
        .context("Could not determine home directory")?
        .join(".agentboard");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn worker_lock() -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(worker_dir()?.join("scheduler.lock"))?)
}

pub fn ensure_worker() -> Result<()> {
    let lock = worker_lock()?;
    match lock.try_lock() {
        Ok(()) => drop(lock),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(worker_dir()?.join("scheduler.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("scheduler")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .context("Could not start Agentboard scheduler")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn run(store: &TaskStore, once: bool) -> Result<()> {
    let lock = worker_lock()?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    loop {
        let result = run_due(store);
        if once {
            return result;
        }
        if let Err(error) = result {
            eprintln!("Scheduler: {error:#}");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn run_due(store: &TaskStore) -> Result<()> {
    let mut errors = Vec::new();
    for id in store.due_schedules(chrono::Utc::now().timestamp())? {
        if let Err(error) = orchestrator::start_scheduled_task(store, &id) {
            errors.push(format!("{id}: {error:#}"));
        }
    }
    if !errors.is_empty() {
        bail!("Could not process scheduled tasks:\n{}", errors.join("\n"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TaskStatus;

    #[test]
    fn parses_delay_and_offset_and_rejects_past_time() {
        let now = Local::now();
        assert_eq!(
            parse_at("in 30m", now).unwrap().timestamp(),
            now.timestamp() + 1800
        );
        let future = now + TimeDelta::hours(1);
        assert_eq!(parse_at(&future.to_rfc3339(), now).unwrap(), future);
        assert!(parse_at("in -1m", now).is_err());
        assert!(parse_at("in 9223372036854775807h", now).is_err());
    }

    #[test]
    fn failed_launch_remains_visible_and_can_be_rescheduled() {
        let store = TaskStore::open_in_memory().unwrap();
        let now = chrono::Utc::now().timestamp();
        let schedule = Schedule {
            at: now - 1,
            config: config::EffectiveConfig {
                agent: "codex".to_owned(),
                profile: "interactive".to_owned(),
                program: "/__agentboard_no_such_agent__".to_owned(),
                args: Vec::new(),
                resume_args: Vec::new(),
            },
            error: None,
        };
        let task = store
            .create_scheduled_task("Schedule me", "", "/tmp", false, Some(schedule))
            .unwrap();
        run_due(&store).unwrap();

        let task = store.get_task(&task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Backlog);
        let mut schedule = task.schedule.unwrap();
        assert!(schedule
            .error
            .as_deref()
            .unwrap()
            .contains("was not found on PATH"));
        assert!(store.due_schedules(now).unwrap().is_empty());

        schedule.at = now + 60;
        store.set_schedule(&task.id, Some(&schedule)).unwrap();
        let saved = store.get_task(&task.id).unwrap().schedule.unwrap();
        assert_eq!(saved.at, now + 60);
        assert!(saved.error.is_none());

        store.update_status(&task.id, TaskStatus::Keep).unwrap();
        assert!(store.get_task(&task.id).unwrap().schedule.is_none());
        assert!(!store.has_pending_schedules().unwrap());
    }
}
