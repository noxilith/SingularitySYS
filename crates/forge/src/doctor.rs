//! `forge doctor`: look over the system and say, in plain words, what is wrong and what to run about it.
//!
//! Everything is read under a root directory (`--root`, `/` by default) and nothing is changed. A problem that stops
//! the machine from working (it would not boot, a base package is missing) is an error and makes the command exit
//! with a failure; anything else that is off is a warning.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use forge_schema::{Entry, SystemDecl};

use crate::ui;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

pub struct Check {
    pub group: &'static str,
    pub level: Level,
    pub text: String,
    /// What to run about it.
    pub fix: Option<String>,
}

struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn add(&mut self, group: &'static str, level: Level, text: impl Into<String>, fix: Option<&str>) {
        self.checks.push(Check { group, level, text: text.into(), fix: fix.map(String::from) });
    }
    fn ok(&mut self, group: &'static str, text: impl Into<String>) {
        self.add(group, Level::Ok, text, None);
    }
    fn warn(&mut self, group: &'static str, text: impl Into<String>, fix: &str) {
        self.add(group, Level::Warn, text, Some(fix));
    }
    fn fail(&mut self, group: &'static str, text: impl Into<String>, fix: &str) {
        self.add(group, Level::Fail, text, Some(fix));
    }
}

fn under(root: &Path, abs: &str) -> PathBuf {
    root.join(abs.trim_start_matches('/'))
}

/// Run every check against `root`.
pub fn examine(root: &Path, base: &[String]) -> Vec<Check> {
    let mut r = Report { checks: Vec::new() };
    let decl_path = under(root, "/etc/forge/system.toml");
    let decl = match fs::read_to_string(&decl_path) {
        Ok(text) => match SystemDecl::parse(&text) {
            Ok(d) => {
                r.ok("Declaration", format!("{} is valid", decl_path.display()));
                Some(d)
            }
            Err(e) => {
                r.fail("Declaration", format!("{} is not valid: {e}", decl_path.display()), "sudo forge edit");
                None
            }
        },
        Err(_) => {
            r.warn("Declaration", format!("there is no {}: this is the live system, or forge was never set up", decl_path.display()), "install with `forge strap ROOT BOOT`");
            None
        }
    };
    let system = decl.as_ref().map(|d| d.init.system.clone()).unwrap_or_else(|| "openrc".into());

    init_link(&mut r, root, &system);
    limine(&mut r, root);
    microcode(&mut r, root);
    defaults(&mut r, root, &system);
    if root == Path::new("/") {
        running_kernel(&mut r);
    }
    if let Some(d) = &decl {
        packages(&mut r, root, d, base);
        accounts(&mut r, root, d);
        hostname(&mut r, root, d);
    }
    services(&mut r, root, &system);
    if root == Path::new("/") {
        clock(&mut r, &system);
    }
    generations(&mut r, root);
    r.checks
}

fn init_link(r: &mut Report, root: &Path, system: &str) {
    const G: &str = "Boot";
    let link = under(root, "/usr/sbin/init");
    let Ok(target) = fs::read_link(&link) else {
        if link.exists() {
            r.warn(G, "/sbin/init is a file, not a link to an init system", "sudo forge apply");
        } else {
            r.fail(G, "there is no /sbin/init: the machine would not start", "sudo forge apply");
        }
        return;
    };
    let shown = target.display().to_string();
    if shown.ends_with("busybox") {
        r.fail(G, "/sbin/init is BusyBox's init: the next boot would stop at \"can't run /etc/init.d/rcS\"", "sudo forge apply");
        return;
    }
    let resolved = link.parent().map(|d| d.join(&target));
    if !resolved.as_ref().is_some_and(|p| p.exists()) {
        r.fail(G, format!("/sbin/init points at {shown}, which is not installed: the machine would not start"), "sudo forge apply");
        return;
    }
    let want = forge_schema::plumbing(system).into_iter().find(|(p, _)| *p == "/usr/sbin/init").and_then(|(_, e)| match e {
        Entry::Link(t) => Some(t),
        _ => None,
    });
    match want {
        Some(w) if w == shown => r.ok(G, format!("/sbin/init starts {}", if system == "systemd" { "systemd" } else { "OpenRC" })),
        Some(_) => r.warn(G, format!("/sbin/init starts something other than the declared init system ({system})"), "sudo forge apply (and reboot to switch)"),
        None => r.ok(G, "/sbin/init is in place"),
    }
}

fn limine(r: &mut Report, root: &Path) {
    const G: &str = "Boot";
    let boot = under(root, "/boot");
    let conf = boot.join("limine.conf");
    let Ok(text) = fs::read_to_string(&conf) else {
        r.warn(G, "there is no /boot/limine.conf (not an installed system, or it is not mounted)", "check that /boot is mounted");
        return;
    };
    let mut entries = 0;
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('/') && !line.starts_with("//") {
            entries += 1;
        }
        for key in ["path:", "module_path:"] {
            if let Some(rest) = line.strip_prefix(key) {
                if let Some(file) = rest.trim().strip_prefix("boot():/") {
                    if !boot.join(file).exists() {
                        missing.insert(file.to_string());
                    }
                }
            }
        }
    }
    if missing.is_empty() {
        r.ok(G, format!("Limine lists {entries} entries and every kernel file they name is there"));
    } else {
        r.fail(G, format!("Limine names files that are not in /boot: {}", missing.into_iter().collect::<Vec<_>>().join(", ")), "sudo forge kernel install, or pick another entry at boot");
    }
}

/// The files forge keeps current (services, the wifi command, shell defaults, runlevels) must match what this forge
/// carries; a mismatch after an upgrade means `forge apply` has not run since.
fn defaults(r: &mut Report, root: &Path, system: &str) {
    const G: &str = "System files";
    if !under(root, "/usr/sbin").is_dir() {
        return;
    }
    let mut stale = Vec::new();
    for m in forge_schema::managed(system).into_iter().filter(|m| m.own == forge_schema::Own::Always) {
        let path = under(root, &m.path);
        let ok = match &m.entry {
            Entry::File { content, .. } => fs::read_to_string(&path).is_ok_and(|c| &c == content),
            Entry::Link(t) => fs::read_link(&path).is_ok_and(|cur| cur == Path::new(t)),
            Entry::Absent => !path.exists(),
        };
        if !ok {
            stale.push(m.path);
        }
    }
    if stale.is_empty() {
        r.ok(G, "the services and defaults forge keeps current are up to date");
    } else {
        let shown: Vec<&str> = stale.iter().take(4).map(String::as_str).collect();
        let more = if stale.len() > 4 { format!(" and {} more", stale.len() - 4) } else { String::new() };
        r.warn(G, format!("out of date: {}{more}", shown.join(", ")), "sudo forge apply");
    }
}

/// The CPU microcode update must be where the boot loader hands it to the kernel.
fn microcode(r: &mut Report, root: &Path) {
    const G: &str = "Boot";
    let Some(src) = forge_gen::microcode_image_path(root).filter(|s| s.is_file()) else {
        return; // the package is not installed (or not an installed system, or a CPU with no update to load)
    };
    let dest = under(root, "/boot/microcode.img");
    let same = matches!((fs::read(&src), fs::read(&dest)), (Ok(a), Ok(b)) if a == b);
    if !same {
        r.warn(G, "a CPU microcode update is installed but /boot/microcode.img is missing or old", "sudo forge apply");
        return;
    }
    let conf = fs::read_to_string(under(root, "/boot/limine.conf")).unwrap_or_default();
    if conf.contains("microcode.img") {
        r.ok(G, "the CPU microcode update is loaded at boot");
    } else {
        r.warn(G, "the CPU microcode image is in /boot but no Limine entry loads it", "sudo forge gen create \"microcode\"");
    }
}

fn running_kernel(r: &mut Report) {
    const G: &str = "Boot";
    let Ok(release) = fs::read_to_string("/proc/sys/kernel/osrelease") else { return };
    let release = release.trim();
    if Path::new("/usr/lib/modules").join(release).is_dir() {
        r.ok(G, format!("the modules of the running kernel ({release}) are installed"));
    } else {
        r.warn(G, format!("the running kernel ({release}) has no modules in /usr/lib/modules: devices that need one will not work"), "sudo forge kernel install, then reboot");
    }
}

fn packages(r: &mut Report, root: &Path, d: &SystemDecl, base: &[String]) {
    const G: &str = "Packages";
    let installed = forge_store::Profile::new(under(root, "/forge/profiles/system")).current_packages().unwrap_or_default();
    let names: BTreeSet<&str> = installed.iter().map(|id| id.name()).collect();
    if names.is_empty() {
        r.warn(G, "no package set is recorded for this system", "sudo forge apply");
        return;
    }
    let mut must: Vec<&str> = base.iter().map(String::as_str).collect();
    must.extend(forge_schema::init_packages(&d.init.system));
    let missing: Vec<&str> = must.into_iter().filter(|n| !names.contains(n)).collect();
    if missing.is_empty() {
        r.ok(G, format!("the base system is complete ({} packages installed)", installed.len()));
    } else {
        r.fail(G, format!("base packages are missing: {}", missing.join(", ")), "sudo forge apply");
    }
    let pending: Vec<&String> = d.packages.iter().filter(|n| !names.contains(n.as_str()) && !forge_schema::is_base_package(n)).collect();
    if pending.is_empty() {
        r.ok(G, "everything you declared is installed");
    } else {
        r.warn(G, format!("declared but not installed: {}", pending.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")), "sudo forge apply");
    }
}

fn accounts(r: &mut Report, root: &Path, d: &SystemDecl) {
    const G: &str = "Users";
    if d.users.is_empty() {
        return;
    }
    let passwd = fs::read_to_string(under(root, "/etc/passwd")).unwrap_or_default();
    let shells = fs::read_to_string(under(root, "/etc/shells")).unwrap_or_default();
    for u in &d.users {
        let line = passwd.lines().find(|l| l.split(':').next() == Some(u.name.as_str()));
        let Some(line) = line else {
            r.warn(G, format!("user {} is declared but does not exist", u.name), "sudo forge apply");
            continue;
        };
        let shell = line.split(':').nth(6).unwrap_or("");
        if !shell.is_empty() && !under(root, shell).exists() {
            r.warn(G, format!("{}'s shell {shell} is not installed: they could not log in", u.name), "sudo forge apply");
        } else if !shell.is_empty() && !shells.lines().any(|l| l == shell) && !shell.starts_with("/usr/bin/nologin") {
            r.warn(G, format!("{}'s shell {shell} is not listed in /etc/shells", u.name), "sudo forge apply");
        } else {
            r.ok(G, format!("{} exists with shell {}", u.name, shell));
        }
    }
}

fn hostname(r: &mut Report, root: &Path, d: &SystemDecl) {
    let have = fs::read_to_string(under(root, "/etc/hostname")).unwrap_or_default();
    if have.trim() != d.hostname {
        r.warn("Declaration", format!("the hostname is {:?} but the declaration says {:?}", have.trim(), d.hostname), "sudo forge apply");
    }
}

fn services(r: &mut Report, root: &Path, system: &str) {
    const G: &str = "Services";
    let mut broken = Vec::new();
    let mut count = 0;
    let dirs: Vec<PathBuf> = if system == "systemd" {
        fs::read_dir(under(root, "/etc/systemd/system")).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".wants")).collect()).unwrap_or_default()
    } else {
        fs::read_dir(under(root, "/etc/runlevels")).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
    };
    for dir in dirs {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                count += 1;
                // Links name absolute paths inside the system being examined.
                let ok = fs::read_link(&p).map(|t| if t.is_absolute() { under(root, &t.display().to_string()).exists() } else { p.parent().is_some_and(|d| d.join(&t).exists()) }).unwrap_or(false);
                if !ok {
                    broken.push(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                }
            }
        }
    }
    if broken.is_empty() {
        r.ok(G, format!("{count} enabled services, all of them installed"));
    } else {
        broken.sort();
        broken.dedup();
        r.warn(G, format!("enabled but not installed: {}", broken.join(", ")), "remove them from `services` with sudo forge edit");
    }
}

/// Is a program with this name running? (Looks at /proc, so only for the live system.)
fn running(name: &str) -> bool {
    fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
        e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) && fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name)
    })
}

fn clock(r: &mut Report, system: &str) {
    const G: &str = "Time";
    let (daemon, fix) = if system == "systemd" { ("systemd-timesyn", "sudo systemctl enable --now systemd-timesyncd") } else { ("chronyd", "sudo rc-service chronyd start") };
    if running(daemon) {
        r.ok(G, "the clock is kept in sync over the network");
    } else {
        r.warn(G, "nothing is keeping the clock in sync (it will drift, and certificates can start to fail)", fix);
    }
}

fn generations(r: &mut Report, root: &Path) {
    const G: &str = "Generations";
    let dir = under(root, "/.snapshots");
    let n = fs::read_dir(&dir).map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("gen-")).count()).unwrap_or(0);
    if n == 0 {
        if dir.is_dir() {
            r.warn(G, "there are no generations to roll back to", "sudo forge gen create \"before changes\"");
        }
        return;
    }
    if n > 30 {
        r.warn(G, format!("{n} generations are kept: they use disk space"), "sudo forge gen remove ID (see forge gen list)");
    } else {
        r.ok(G, format!("{n} generation{} to roll back to", if n == 1 { "" } else { "s" }));
    }
}

/// Print the report; the exit status says whether anything is broken.
pub fn run(root: &Path, base: &[String]) -> Result<bool> {
    let checks = examine(root, base);
    let u = ui::ui();
    let mut group = "";
    for c in &checks {
        if c.group != group {
            ui::heading(c.group);
            group = c.group;
        }
        let mark = match c.level {
            Level::Ok => u.on(),
            Level::Warn => u.warn_mark(),
            Level::Fail => u.fail_mark(),
        };
        println!("  {mark} {}", c.text);
        if let Some(fix) = &c.fix {
            println!("      {} {}", u.arrow(), u.dim(&format!("run: {fix}")));
        }
    }
    let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warns = checks.iter().filter(|c| c.level == Level::Warn).count();
    println!();
    match (fails, warns) {
        (0, 0) => ui::ok("everything looks right"),
        (0, w) => ui::info(&format!("{w} warning{}, nothing that stops the system", if w == 1 { "" } else { "s" })),
        (f, w) => println!("{} {f} problem{} that can stop the system, {w} warning{}", u.fail_mark(), if f == 1 { "" } else { "s" }, if w == 1 { "" } else { "s" }),
    }
    Ok(fails == 0)
}
