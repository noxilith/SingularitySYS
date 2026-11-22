//! `forge flatpak`: install and update Flatpak applications (Astralis and anything else on Flathub) through forge.
//!
//! Flatpak itself comes from Debian through `forge deb` (the package `deb-flatpak`); applications are installed for the
//! whole system (`flatpak --system`, under /var/lib/flatpak) and listed in the `flatpaks` key of
//! /etc/forge/system.toml, so the declaration stays the single source of truth: `forge apply` installs the ones that are
//! missing and `forge upgrade` brings them up to date together with the packages.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use forge_schema::SystemDecl;

use crate::ui;

const FLATPAK: &str = "/usr/bin/flatpak";
const DECL: &str = "/etc/forge/system.toml";
const DEFAULT_REMOTE: &str = "flathub";
const FLATHUB_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// Applications forge knows by a short name: (name, remote, the remote's .flatpakrepo file, application id).
const KNOWN: &[(&str, &str, &str, &str)] = &[
    ("astralis", "astralis", "https://noxilith.github.io/Astralis/astralis.flatpakrepo", "app.astralis.desktop"),
    ("discord", DEFAULT_REMOTE, FLATHUB_REPO, "com.discordapp.Discord"),
    ("vesktop", DEFAULT_REMOTE, FLATHUB_REPO, "dev.vencord.Vesktop"),
];

#[derive(Args)]
pub struct FlatpakArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Install Flatpak (from Debian, through `forge deb`) and add the Flathub remote
    Setup {
        /// Do not ask for confirmation when applying
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Install applications and keep them in the system declaration: an id from Flathub, REMOTE:ID from another
    /// remote, or a downloaded bundle (APP.flatpak) or reference (APP.flatpakref, a file or an address)
    Install {
        /// Application ids, e.g. org.mozilla.firefox
        #[arg(required = true)]
        apps: Vec<String>,
    },
    /// Remove applications (and the runtimes nothing uses any more)
    Remove {
        #[arg(required = true)]
        apps: Vec<String>,
    },
    /// List the installed applications
    List,
    /// Search the remotes for an application
    Search { term: String },
    /// Update every installed application and runtime
    Upgrade,
    /// Add a remote, e.g. `forge flatpak remote add NAME https://example.org/repo.flatpakrepo`
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Add a remote (a .flatpakrepo file carries its signing key)
    Add { name: String, url: String },
    /// List the remotes
    List,
}

pub fn run(a: FlatpakArgs) -> Result<()> {
    match a.action {
        Action::Setup { yes } => {
            need_root()?;
            if !Path::new(FLATPAK).exists() {
                crate::deb_cmd::ensure_installed("flatpak", yes)?;
            }
            ensure_remote(DEFAULT_REMOTE, FLATHUB_REPO)?;
            ui::ok("Flatpak is ready: install applications with `sudo forge flatpak install APP` (find them with `forge flatpak search NAME`)");
            ui::info("log out and in again once, so the launcher sees the applications (/var/lib/flatpak/exports/share)");
            Ok(())
        }
        Action::Install { apps } => install_apps(&apps),
        Action::Remove { apps } => remove_apps(&apps),
        Action::List => {
            present()?;
            flatpak(&["list", "--system", "--app", "--columns=application,version,origin"])
        }
        Action::Search { term } => {
            present()?;
            flatpak(&["search", &term])
        }
        Action::Upgrade => {
            need_root()?;
            present()?;
            upgrade_listed(true)
        }
        Action::Remote { action } => match action {
            RemoteAction::Add { name, url } => {
                need_root()?;
                present()?;
                flatpak(&["remote-add", "--system", "--if-not-exists", &name, &url])
            }
            RemoteAction::List => {
                present()?;
                flatpak(&["remotes", "--system"])
            }
        },
    }
}

/// The ids of the applications installed in the system installation.
fn installed_apps() -> Vec<String> {
    match Command::new(FLATPAK).args(["list", "--system", "--app", "--columns=application"]).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// `forge flatpak install FILE.flatpak` (a bundle) or `FILE.flatpakref` (a file or address that says where the
/// application comes from): install it, then record what was installed and the remote it came from, so `forge apply`
/// can get it again.
fn install_file(file: &str) -> Result<()> {
    let before = installed_apps();
    let how = if file.ends_with(".flatpakref") { "--from" } else { "--bundle" };
    flatpak(&["install", "--system", "-y", "--noninteractive", how, file])?;
    let new: Vec<String> = installed_apps().into_iter().filter(|a| !before.contains(a)).collect();
    if new.is_empty() {
        ui::info("that application was already installed (updated in place); the declaration is as it was");
        return Ok(());
    }
    for id in new {
        let origin = Command::new(FLATPAK)
            .args(["info", "--system", "--show-origin", &id])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let entry = if origin.is_empty() || origin == DEFAULT_REMOTE { id.clone() } else { format!("{origin}:{id}") };
        edit_decl(|t| forge_decl::add_flatpak(t, &entry))?;
        ui::ok(&format!("installed {id} (recorded as {entry})"));
        if file.ends_with(".flatpak") {
            ui::info("a bundle file has no repository to update from unless it names one: install a newer .flatpak the same way to update it");
        }
    }
    Ok(())
}

/// `forge upgrade`: update the applications too (only when Flatpak is installed and something is installed with it).
pub fn upgrade_declared() {
    if !Path::new(FLATPAK).exists() || declared().is_empty() {
        return;
    }
    // Nothing is printed unless something has an update: flatpak's own output is full of notes about end-of-life runtimes
    // and refs that are no longer on the remote, which are not news at every upgrade.
    if let Err(e) = upgrade_listed(false) {
        ui::warn(&format!("the Flatpak applications were not updated: {e:#}"));
    }
}

/// Run flatpak with its output collected; the error carries the last lines it printed.
fn flatpak_quiet(args: &[&str]) -> Result<String> {
    let out = Command::new(FLATPAK).args(args).output().with_context(|| format!("running {FLATPAK}"))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).rev().take(4).collect();
        bail!("`flatpak {}` failed ({}): {}", args.join(" "), out.status, tail.into_iter().rev().collect::<Vec<_>>().join(" / "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The installed applications and runtimes with an update on their remote: (id, installed version, new version).
fn pending_updates() -> Vec<(String, String, String)> {
    let Ok(list) = flatpak_quiet(&["remote-ls", "--system", "--updates", "--columns=application,version"]) else { return Vec::new() };
    let installed: BTreeMap<String, String> = flatpak_quiet(&["list", "--system", "--columns=application,version"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut c = l.split('\t');
            Some((c.next()?.trim().to_string(), c.next().unwrap_or("").trim().to_string()))
        })
        .collect();
    let mut rows: Vec<(String, String, String)> = list
        .lines()
        .filter_map(|l| {
            let mut c = l.split('\t');
            let id = c.next()?.trim().to_string();
            if id.is_empty() {
                return None;
            }
            let new = c.next().unwrap_or("").trim().to_string();
            Some((id.clone(), installed.get(&id).cloned().unwrap_or_default(), new))
        })
        .collect();
    rows.sort();
    rows.dedup();
    rows
}

/// Update what has an update and say so in the package-list style; with `say_none` also say when nothing needs it.
fn upgrade_listed(say_none: bool) -> Result<()> {
    let rows = pending_updates();
    if rows.is_empty() {
        if say_none {
            ui::ok("the Flatpak applications are up to date");
        }
        return Ok(());
    }
    ui::print_flatpak_updates(&rows);
    println!();
    let spin = ui::Spinner::start("updating the Flatpak applications");
    match flatpak_quiet(&["update", "--system", "-y", "--noninteractive"]) {
        Ok(_) => {
            let _ = flatpak_quiet(&["uninstall", "--system", "--unused", "-y", "--noninteractive"]);
            spin.finish();
            Ok(())
        }
        Err(e) => {
            spin.fail();
            Err(e)
        }
    }
}

/// `forge apply`: install the declared applications that are missing (best effort; needs the network).
pub fn sync_declared() {
    if !Path::new(FLATPAK).exists() {
        return;
    }
    for entry in declared() {
        let (remote, id) = split(&entry);
        if let Some((_, r, repo, _)) = KNOWN.iter().find(|k| k.1 == remote) {
            let _ = ensure_remote(r, repo);
        }
        let have = Command::new(FLATPAK).args(["info", "--system", id]).output().map(|o| o.status.success()).unwrap_or(false);
        if have {
            continue;
        }
        ui::info(&format!("installing the Flatpak application {id}"));
        if let Err(e) = flatpak(&["install", "--system", "-y", "--noninteractive", remote, id]) {
            ui::warn(&format!("{id} was not installed: {e:#} (forge apply tries again next time)"));
        }
    }
}

/// Install Flatpak applications and record them in the declaration (`forge flatpak install`, and `forge install` for the
/// short names forge knows).
pub fn install_apps(apps: &[String]) -> Result<()> {
    need_root()?;
    ensure_flatpak()?;
    for entry in apps {
        let known = known(entry);
        let entry = &match known {
            Some((_, remote, repo, id)) => {
                ensure_remote(remote, repo)?;
                format!("{remote}:{id}")
            }
            None => entry.clone(),
        };
        if entry.ends_with(".flatpak") || entry.ends_with(".flatpakref") {
            install_file(entry)?;
            continue;
        }
        let (remote, id) = split(entry);
        flatpak(&["install", "--system", "-y", "--noninteractive", remote, id])?;
        edit_decl(|t| forge_decl::add_flatpak(t, entry))?;
        ui::ok(&format!("installed {id}"));
    }
    Ok(())
}

/// Remove Flatpak applications and their entries in the declaration.
pub fn remove_apps(apps: &[String]) -> Result<()> {
    need_root()?;
    present()?;
    for entry in apps {
        let entry = &known(entry).map(|(_, remote, _, id)| format!("{remote}:{id}")).unwrap_or_else(|| entry.clone());
        let (_, id) = split(entry);
        flatpak(&["uninstall", "--system", "-y", "--noninteractive", id])?;
        // the declaration may list it with or without a remote
        for e in declared().into_iter().filter(|e| split(e).1 == id) {
            edit_decl(|t| forge_decl::remove_flatpak(t, &e))?;
        }
        ui::ok(&format!("removed {id}"));
    }
    let _ = flatpak(&["uninstall", "--system", "--unused", "-y", "--noninteractive"]);
    Ok(())
}

/// Is this one of the application names forge knows (`discord`, `vesktop`, `astralis`)?
pub fn is_known(name: &str) -> bool {
    known(name).is_some()
}

/// A short name (`astralis`) for an application forge knows.
fn known(name: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    KNOWN.iter().find(|k| k.0 == name.to_lowercase()).copied()
}

/// Add a remote for the whole system unless it is there (a .flatpakrepo file carries the signing key).
fn ensure_remote(name: &str, repo: &str) -> Result<()> {
    flatpak(&["remote-add", "--system", "--if-not-exists", name, repo])
}

/// Install Flatpak itself (from Debian) and the Flathub remote when that has not been done: a first
/// `forge flatpak install` does it without a separate setup step.
fn ensure_flatpak() -> Result<()> {
    if !Path::new(FLATPAK).exists() {
        ui::info("Flatpak is not installed yet: setting it up first");
        crate::deb_cmd::ensure_installed("flatpak", false)?;
    }
    ensure_remote(DEFAULT_REMOTE, FLATHUB_REPO)
}

fn need_root() -> Result<()> {
    if !crate::is_root() {
        bail!("this changes the system: run it with sudo");
    }
    Ok(())
}

fn present() -> Result<()> {
    if !Path::new(FLATPAK).exists() {
        bail!("Flatpak is not installed: run `sudo forge flatpak setup` first");
    }
    Ok(())
}

/// `remote:id` or just `id` (the Flathub remote).
fn split(entry: &str) -> (&str, &str) {
    match entry.split_once(':') {
        Some((remote, id)) => (remote, id),
        None => (DEFAULT_REMOTE, entry),
    }
}

fn declared() -> Vec<String> {
    std::fs::read_to_string(DECL).ok().and_then(|t| SystemDecl::parse(&t).ok()).map(|d| d.flatpaks).unwrap_or_default()
}

fn flatpak(args: &[&str]) -> Result<()> {
    let status = Command::new(FLATPAK).args(args).status().with_context(|| format!("running {FLATPAK}"))?;
    if !status.success() {
        bail!("`flatpak {}` failed ({status})", args.join(" "));
    }
    Ok(())
}

/// Change the `flatpaks` list in the system declaration (comments and layout stay), checking the result still parses.
fn edit_decl(edit: impl FnOnce(&str) -> Result<forge_decl::Edited, forge_decl::EditError>) -> Result<()> {
    let text = std::fs::read_to_string(DECL).with_context(|| format!("reading {DECL}"))?;
    let edited = edit(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !edited.changed {
        return Ok(());
    }
    SystemDecl::parse(&edited.text).context("the edited declaration does not parse")?;
    let tmp = format!("{DECL}.new");
    std::fs::write(&tmp, &edited.text).with_context(|| format!("writing {tmp}"))?;
    std::fs::rename(&tmp, DECL).with_context(|| format!("replacing {DECL}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_map_to_their_flathub_ids() {
        assert_eq!(known("discord").unwrap().3, "com.discordapp.Discord");
        assert_eq!(known("Vesktop").unwrap().3, "dev.vencord.Vesktop");
        assert_eq!(known("vesktop").unwrap().1, DEFAULT_REMOTE);
        assert_eq!(known("astralis").unwrap().3, "app.astralis.desktop");
        assert!(known("nothing-like-this").is_none());
    }
}
