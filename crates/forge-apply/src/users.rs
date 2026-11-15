//! Users, groups and login shells from the declaration: `[[users]]` and `shell`.
//!
//! The declaration says who exists, which groups they are in, and their shell; forge makes the machine match using
//! the shadow tools (`useradd`, `usermod`, `groupadd`), after the packages are installed. Passwords are never in
//! the declaration: a user created here has none until `sudo passwd NAME` (the installer's user already has one).
//! Nothing is ever removed: dropping a user from the declaration does not delete the account or its files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use forge_schema::SystemDecl;

use crate::{Change, Kind, Op, Planner, under};

/// `fish` and `/usr/bin/fish` are both fine: a bare name is a program in `/usr/bin`.
pub(crate) fn shell_path(shell: &str) -> String {
    if shell.contains('/') { shell.to_string() } else { format!("/usr/bin/{shell}") }
}

/// `/bin/bash` and `/usr/bin/bash` are the same program (merged /usr).
fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.strip_prefix("/usr").unwrap_or(p).to_string();
    norm(a) == norm(b)
}

/// The packages the declared shells need: a bare name that is not part of the base system is a package of that name.
pub(crate) fn shell_packages(decl: &SystemDecl) -> Vec<String> {
    let mut v = Vec::new();
    for s in std::iter::once(decl.shell.as_str()).chain(decl.users.iter().filter_map(|u| u.shell.as_deref())) {
        if !s.contains('/') && !forge_schema::is_base_package(s) && !v.iter().any(|x| x == s) {
            v.push(s.to_string());
        }
    }
    v
}

struct Passwd {
    shell: String,
}

fn read_passwd(root: &Path) -> BTreeMap<String, Passwd> {
    let text = fs::read_to_string(under(root, "/etc/passwd")).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 7).then(|| (f[0].to_string(), Passwd { shell: f[6].to_string() }))
        })
        .collect()
}

/// group name -> its member names
fn read_groups(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let text = fs::read_to_string(under(root, "/etc/group")).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 4).then(|| (f[0].to_string(), f[3].split(',').filter(|s| !s.is_empty()).map(String::from).collect()))
        })
        .collect()
}

impl Planner<'_> {
    fn run(&mut self, section: &'static str, kind: Kind, summary: String, tool: &str, args: Vec<String>) {
        let mut full = Vec::new();
        if self.root != Path::new("/") {
            full.push("--root".to_string());
            full.push(self.root.display().to_string());
        }
        full.extend(args);
        self.changes.push(Change { section, kind, summary, op: Op::Run { program: under(self.root, &format!("/usr/sbin/{tool}")), args: full, best_effort: false } });
    }
}

/// Plan `/etc/shells` and the accounts.
pub(crate) fn plan_users(p: &mut Planner, decl: &SystemDecl) -> Result<(), crate::PlanError> {
    let root = p.root.to_path_buf();
    let system_shell = shell_path(&decl.shell);

    // /etc/shells lists the shells a user may pick; the declared ones must be there.
    let shells_text = fs::read_to_string(under(&root, "/etc/shells")).ok();
    let mut wanted: Vec<String> = vec![system_shell.clone()];
    for u in &decl.users {
        if let Some(s) = &u.shell {
            wanted.push(shell_path(s));
        }
    }
    if let Some(existing) = &shells_text {
        let have: Vec<&str> = existing.lines().collect();
        let mut text = existing.clone();
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        let mut added = false;
        for w in &wanted {
            if !have.iter().any(|h| same_path(h, w)) && !text.lines().any(|l| l == w) {
                text.push_str(w);
                text.push('\n');
                added = true;
            }
        }
        if added {
            p.file("shell", "/etc/shells", text)?;
        }
    }

    let passwd = read_passwd(&root);
    let mut groups = read_groups(&root);
    // Groups that have to exist before anyone can join them (created once, however many users want them).
    let mut made: BTreeSet<String> = BTreeSet::new();
    let mut need_group = |p: &mut Planner, g: &str, groups: &BTreeMap<String, BTreeSet<String>>| {
        if !groups.contains_key(g) && made.insert(g.to_string()) {
            p.run("users", Kind::Add, format!("group {g}: create"), "groupadd", vec![g.to_string()]);
        }
    };

    if let Some(r) = passwd.get("root").filter(|r| decl.shell != "bash" && !same_path(&r.shell, &system_shell)) {
        p.run("users", Kind::Modify, format!("user root: shell {} -> {}", r.shell, system_shell), "usermod", vec!["-s".into(), system_shell.clone(), "root".into()]);
    }
    }

    for u in &decl.users {
        let want_shell = shell_path(u.shell.as_deref().unwrap_or(&decl.shell));
        match passwd.get(&u.name) {
            None => {
                for g in &u.groups {
                    need_group(p, g, &groups);
                }
                let mut args = vec!["-m".to_string(), "-s".to_string(), want_shell.clone()];
                if !u.groups.is_empty() {
                    args.push("-G".into());
                    args.push(u.groups.join(","));
                }
                args.push(u.name.clone());
                let with = if u.groups.is_empty() { String::new() } else { format!(", groups {}", u.groups.join(", ")) };
                p.run("users", Kind::Add, format!("user {}: create (shell {want_shell}{with}); no password yet: run `sudo passwd {}`", u.name, u.name), "useradd", args);
                // The group file changes once the user exists; later users see these memberships.
                for g in &u.groups {
                    groups.entry(g.clone()).or_default().insert(u.name.clone());
                }
            }
            Some(cur) => {
                if !same_path(&cur.shell, &want_shell) {
                    p.run("users", Kind::Modify, format!("user {}: shell {} -> {want_shell}", u.name, cur.shell), "usermod", vec!["-s".into(), want_shell.clone(), u.name.clone()]);
                }
                let missing: Vec<String> = u.groups.iter().filter(|g| !groups.get(*g).is_some_and(|m| m.contains(&u.name))).cloned().collect();
                if !missing.is_empty() {
                    for g in &missing {
                        need_group(p, g, &groups);
                    }
                    p.run("users", Kind::Modify, format!("user {}: join {}", u.name, missing.join(", ")), "usermod", vec!["-a".into(), "-G".into(), missing.join(","), u.name.clone()]);
                }
            }
        }
    }
    Ok(())
}
