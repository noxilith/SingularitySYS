//! Routine maintenance: trim the SSD, scrub btrfs, prune old generations, clean the package store.
//! `forge maintenance run --due` is what the maintenance service calls every hour; it runs only what is overdue.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

const DAY: u64 = 86_400;
pub const STATE: &str = "/var/lib/forge/maintenance";

pub struct Task {
    pub name: &'static str,
    pub what: &'static str,
    pub every: u64,
}

pub const TASKS: &[Task] = &[
    Task { name: "trim", what: "tell the SSD which blocks are free (fstrim)", every: 7 * DAY },
    Task { name: "prune", what: "remove old generations beyond the number to keep", every: 7 * DAY },
    Task { name: "gc", what: "delete packages nothing uses any more", every: 7 * DAY },
    Task { name: "scrub", what: "read every block and check it against its checksum (btrfs scrub)", every: 30 * DAY },
];

/// `name unixtime` per line.
pub fn parse_state(text: &str) -> BTreeMap<String, u64> {
    text.lines().filter_map(|l| { let (n, t) = l.split_once(' ')?; Some((n.to_string(), t.trim().parse().ok()?)) }).collect()
}

pub fn render_state(s: &BTreeMap<String, u64>) -> String {
    s.iter().map(|(n, t)| format!("{n} {t}\n")).collect()
}

/// Seconds until the task is due (0 when it is overdue or never ran).
pub fn due_in(state: &BTreeMap<String, u64>, task: &Task, now: u64) -> u64 {
    match state.get(task.name) {
        Some(last) => (last + task.every).saturating_sub(now),
        None => 0,
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(path).map(|t| parse_state(&t)).unwrap_or_default()
}

fn save(path: &Path, s: &BTreeMap<String, u64>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, render_state(s));
}

fn sh(program: &str, args: &[&str]) -> bool {
    Command::new(program).args(args).status().is_ok_and(|s| s.success())
}

fn run_task(name: &str, keep: u32) -> bool {
    let me = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "forge".into());
    match name {
        "trim" => sh("fstrim", &["--all", "--verbose"]),
        "prune" => sh(&me, &["gen", "prune", "--keep", &keep.to_string()]),
        "gc" => sh(&me, &["store", "gc"]),
        "scrub" => sh("btrfs", &["scrub", "start", "-B", "/"]),
        _ => false,
    }
}

fn describe(secs: u64) -> String {
    match secs {
        0 => "due now".into(),
        s if s < DAY => "due within a day".into(),
        s => format!("due in {} days", s.div_ceil(DAY)),
    }
}

/// Run the named tasks (all of them, or only the overdue ones), recording the time of each success.
pub fn run(only_due: bool, names: &[String], keep: u32, state_file: &Path) -> Result<()> {
    for n in names {
        if !TASKS.iter().any(|t| t.name == n) {
            bail!("unknown task `{n}` (tasks: {})", TASKS.iter().map(|t| t.name).collect::<Vec<_>>().join(", "));
        }
    }
    let mut state = load(state_file);
    let t = now();
    let mut failed = Vec::new();
    for task in TASKS {
        let wanted = names.is_empty() || names.iter().any(|n| n == task.name);
        if !wanted || (only_due && due_in(&state, task, t) > 0) {
            continue;
        }
        println!("maintenance: {} — {}", task.name, task.what);
        if run_task(task.name, keep) {
            state.insert(task.name.to_string(), now());
            save(state_file, &state);
        } else {
            failed.push(task.name);
        }
    }
    if !failed.is_empty() {
        bail!("maintenance task(s) failed: {}", failed.join(", "));
    }
    Ok(())
}

pub fn status(state_file: &Path) {
    let state = load(state_file);
    let t = now();
    for task in TASKS {
        let last = match state.get(task.name) {
            Some(l) => format!("last ran {} days ago", t.saturating_sub(*l) / DAY),
            None => "never ran".into(),
        };
        println!("{:<6} {last}, {} — {}", task.name, describe(due_in(&state, task, t)), task.what);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_ignores_garbage() {
        let s = parse_state("trim 100\nbogus\nscrub x\nprune 7\n");
        assert_eq!(s.len(), 2);
        assert_eq!(parse_state(&render_state(&s)), s);
    }

    #[test]
    fn a_task_is_due_when_it_never_ran_or_its_interval_has_passed() {
        let task = &TASKS[0];
        let mut s = BTreeMap::new();
        assert_eq!(due_in(&s, task, 1_000_000), 0);
        s.insert(task.name.to_string(), 1_000_000);
        assert_eq!(due_in(&s, task, 1_000_000 + DAY), 6 * DAY);
        assert_eq!(due_in(&s, task, 1_000_000 + 8 * DAY), 0);
    }

    #[test]
    fn an_unknown_task_is_refused() {
        let d = tempfile::tempdir().unwrap();
        assert!(run(false, &["nope".into()], 5, &d.path().join("m")).is_err());
    }
}
