//! `forge flatpak`: install and update Flatpak applications (Astralis and anything else on Flathub) through forge.
//!
//! Flatpak itself comes from Debian through `forge deb` (the package `deb-flatpak`); applications are installed for the
//! whole system (`flatpak --system`, under /var/lib/flatpak) and listed in the `flatpaks` key of
//! /etc/forge/system.toml, so the declaration stays the single source of truth: `forge apply` installs the ones that are
//! missing and `forge upgrade` brings them up to date together with the packages.

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
            flatpak(&["remote-add", "--system", "--if-not-exists", DEFAULT_REMOTE, FLATHUB_REPO])?;
            ui::ok("Flatpak is ready: install applications with `sudo forge flatpak install APP` (find them with `forge flatpak search NAME`)");
            ui::info("log out and in again once, so the launcher sees the applications (/var/lib/flatpak/exports/share)");
            Ok(())
        }
        Action::Install { apps } => {
            need_root()?;
            present()?;
            for entry in &apps {
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
        Action::Remove { apps } => {
            need_root()?;
            present()?;
            for entry in &apps {
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
            upgrade_all()
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
    ui::heading("Flatpak applications");
    if let Err(e) = upgrade_all() {
        ui::warn(&format!("the Flatpak applications were not updated: {e:#}"));
    }
}

/// `forge apply`: install the declared applications that are missing (best effort; needs the network).
pub fn sync_declared() {
    if !Path::new(FLATPAK).exists() {
        return;
    }
    for entry in declared() {
        let (remote, id) = split(&entry);
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

fn upgrade_all() -> Result<()> {
    flatpak(&["update", "--system", "-y", "--noninteractive"])?;
    let _ = flatpak(&["uninstall", "--system", "--unused", "-y", "--noninteractive"]);
    Ok(())
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
