//! `forge deb`: install packages from Debian (or any apt repository) as forge packages.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use forge_deb::{apt, install};

use crate::ui;

#[derive(Args)]
pub struct DebArgs {
    #[command(subcommand)]
    action: DebAction,
    /// Work under this directory instead of / (for testing)
    #[arg(long, default_value = "/", global = true)]
    root: PathBuf,
}

#[derive(Subcommand)]
enum DebAction {
    /// Manage the apt repositories packages come from
    Source {
        #[command(subcommand)]
        action: SourceAction,
    },
    /// Download the package lists of every source (and check their signatures)
    Update,
    /// Search the package lists by name or description
    Search { term: String },
    /// Install packages from the apt sources (dependencies come along); the result is the forge package deb-NAME
    Install(InstallArgs),
    /// Only convert (no install): make the forge package in the local repository
    Build(InstallArgs),
    /// Remove a package that `forge deb install` made
    Remove(InstallArgs),
    /// Re-read the lists, rebuild every installed deb-* package from its newest version and upgrade
    Upgrade {
        /// Do not ask for confirmation when applying
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SourceAction {
    /// Add a repository. `forge deb source add debian` adds Debian (trixie, main) with its archive keys;
    /// `forge deb source add anthropic` adds Anthropic's repository (Claude Desktop), `forge deb source add librewolf` the LibreWolf browser's
    Add {
        name: String,
        /// The repository, e.g. https://deb.debian.org/debian
        url: Option<String>,
        /// The suite (distribution), e.g. trixie or stable; `./` for a flat repository
        suite: Option<String>,
        /// Components, e.g. main (none for a flat repository)
        components: Vec<String>,
        /// The repository's public signing key: a file or an https address (repeatable). This is what you trust.
        #[arg(long = "key")]
        keys: Vec<String>,
    },
    /// List the configured repositories
    List,
    /// Forget a repository
    Remove { name: String },
}

#[derive(Args)]
struct InstallArgs {
    /// Debian package names
    #[arg(required = true)]
    names: Vec<String>,
    /// Do not ask for confirmation when applying
    #[arg(long, short = 'y')]
    yes: bool,
}

fn need_root(root: &Path) -> Result<()> {
    if root == Path::new("/") && !crate::is_root() {
        bail!("this changes the system: run it with sudo");
    }
    Ok(())
}

fn key_bytes(spec: &str, scratch: &Path) -> Result<Vec<u8>> {
    if spec.starts_with("https://") || spec.starts_with("http://") {
        let tmp = scratch.join("key.download");
        apt::download(spec, &tmp)?;
        let b = std::fs::read(&tmp)?;
        let _ = std::fs::remove_file(&tmp);
        Ok(b)
    } else {
        std::fs::read(spec).with_context(|| format!("reading {spec}"))
    }
}

/// Debian's archive signing keys for a suite (published by Debian at ftp-master.debian.org).
fn debian_keys(suite: &str) -> Vec<String> {
    let n = match suite {
        "trixie" | "stable" => 13,
        "bookworm" | "oldstable" => 12,
        _ => 13,
    };
    vec![format!("https://ftp-master.debian.org/keys/archive-key-{n}.asc"), format!("https://ftp-master.debian.org/keys/release-{n}.asc")]
}

fn forge_indexes(root: &Path) -> Vec<forge_store::Index> {
    let path = root.join("etc/forge/system.toml");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let Ok(decl) = forge_schema::SystemDecl::parse(&text) else { return Vec::new() };
    let Ok(ring) = forge_store::Keyring::load_dir(&root.join("etc/forge/keys")) else { return Vec::new() };
    let trust = forge_store::Trust::Require(ring);
    let fetch = forge_store::SystemFetch;
    let mut out = Vec::new();
    for r in &decl.repos {
        let spec = if forge_store::is_url(r) { r.clone() } else { root.join(r.trim_start_matches('/')).display().to_string() };
        match forge_store::Repository::parse(&spec).load(&fetch, &trust) {
            Ok(l) => out.push(l.index),
            Err(e) => ui::warn(&format!("could not read the repository {r}: {e}")),
        }
    }
    out
}

pub fn run(a: DebArgs) -> Result<()> {
    let cfg = install::Config::new(&a.root);
    match a.action {
        DebAction::Source { action } => source(&cfg, &a.root, action),
        DebAction::Update => {
            need_root(&a.root)?;
            update(&cfg)
        }
        DebAction::Search { term } => search(&cfg, &term),
        DebAction::Install(i) => {
            need_root(&a.root)?;
            install_cmd(&cfg, &a.root, i)
        }
        DebAction::Build(i) => {
            need_root(&a.root)?;
            for n in &i.names {
                build_one(&cfg, &a.root, n)?;
            }
            Ok(())
        }
        DebAction::Remove(i) => {
            need_root(&a.root)?;
            let names: Vec<String> = i.names.iter().map(|n| format!("{}{n}", install::PREFIX)).collect();
            crate::pkg_remove(names, i.yes, a.root)
        }
        DebAction::Upgrade { yes } => {
            need_root(&a.root)?;
            upgrade_cmd(&cfg, &a.root, yes)
        }
    }
}

fn source(cfg: &install::Config, root: &Path, action: SourceAction) -> Result<()> {
    match action {
        SourceAction::Add { name, url, suite, components, keys } => {
            need_root(root)?;
            let (url, suite, components, keys) = match url {
                Some(u) => {
                    let Some(s) = suite else { bail!("give the suite too, e.g. `forge deb source add {name} {u} stable main --key KEY`") };
                    (u, s, components, keys)
                }
                None if name == "debian" => {
                    let suite = suite.unwrap_or_else(|| "trixie".to_string());
                    let keys = if keys.is_empty() { debian_keys(&suite) } else { keys };
                    ("https://deb.debian.org/debian".to_string(), suite, vec!["main".to_string()], keys)
                }
                None if name == "anthropic" => {
                    let keys = if keys.is_empty() { vec!["https://downloads.claude.ai/keys/claude-code.asc".to_string()] } else { keys };
                    ("https://downloads.claude.ai/claude-desktop/apt/stable".to_string(), "stable".to_string(), vec!["main".to_string()], keys)
                }
                None if name == "librewolf" => {
                    let keys = if keys.is_empty() { vec!["https://repo.librewolf.net/pubkey.gpg".to_string()] } else { keys };
                    ("https://repo.librewolf.net".to_string(), "librewolf".to_string(), vec!["main".to_string()], keys)
                }
                None => bail!("give the repository address: forge deb source add {name} URL SUITE [COMPONENTS...] --key KEY"),
            };
            if keys.is_empty() {
                bail!("give the repository's public key with --key (a file or an https address): forge only uses signed repositories");
            }
            let scratch = cfg.cache().join("tmp");
            std::fs::create_dir_all(&scratch)?;
            let bytes: Vec<Vec<u8>> = keys.iter().map(|k| key_bytes(k, &scratch)).collect::<Result<_>>()?;
            let fps = install::add_source(cfg, &name, &url, &suite, components, &bytes)?;
            ui::ok(&format!("added {name}: {url} {suite}"));
            ui::info("you now trust these signing keys for it:");
            for f in fps {
                println!("    {f}");
            }
            ui::info("next: sudo forge deb update");
            Ok(())
        }
        SourceAction::List => {
            let s = cfg.sources()?;
            if s.sources.is_empty() {
                println!("no sources yet; try: sudo forge deb source add debian");
            }
            for x in s.sources {
                println!("  {:<14} {} {} {}", x.name, x.url, x.suite, x.components.join(" "));
            }
            Ok(())
        }
        SourceAction::Remove { name } => {
            need_root(root)?;
            let mut s = cfg.sources()?;
            let before = s.sources.len();
            s.sources.retain(|x| x.name != name);
            if s.sources.len() == before {
                bail!("no source called {name}");
            }
            s.save(&cfg.sources_file())?;
            ui::ok(&format!("removed {name}"));
            Ok(())
        }
    }
}

fn update(cfg: &install::Config) -> Result<()> {
    let s = cfg.sources()?;
    if s.sources.is_empty() {
        bail!("no sources yet; try: sudo forge deb source add debian");
    }
    for src in &s.sources {
        ui::info(&format!("{}: downloading and checking the package lists", src.name));
        let n = apt::update_source(&cfg.cache(), &cfg.keys_dir(), src).with_context(|| format!("updating {}", src.name))?;
        ui::ok(&format!("{}: {n} packages, signature good", src.name));
    }
    Ok(())
}

fn search(cfg: &install::Config, term: &str) -> Result<()> {
    let s = cfg.sources()?;
    let pkgs = apt::load_packages(&cfg.cache(), &s)?;
    let t = term.to_lowercase();
    let mut best: std::collections::BTreeMap<&str, &apt::Pkg> = Default::default();
    for p in pkgs.iter().filter(|p| p.name.to_lowercase().contains(&t) || p.description.to_lowercase().contains(&t)) {
        let better = best.get(p.name.as_str()).is_none_or(|b| apt::version_cmp(&p.version, &b.version).is_gt());
        if better {
            best.insert(p.name.as_str(), p);
        }
    }
    if best.is_empty() {
        println!("no Debian package matches `{term}`");
        return Ok(());
    }
    for (name, p) in best.iter().take(60) {
        println!("  {name:<32} {:<18} {}", p.version, p.description);
    }
    if best.len() > 60 {
        println!("  ... and {} more", best.len() - 60);
    }
    Ok(())
}

fn declare_repo(root: &Path, repo: &Path) -> Result<()> {
    let path = root.join("etc/forge/system.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let shown = format!("/{}", repo.strip_prefix(root).unwrap_or(repo).display().to_string().trim_start_matches('/'));
    let e = forge_decl::add_repo(&text, &shown)?;
    if e.changed {
        std::fs::write(&path, e.text).with_context(|| format!("writing {}", path.display()))?;
        ui::ok(&format!("added the local repository {shown} to {}", path.display()));
    }
    Ok(())
}

fn build_one(cfg: &install::Config, root: &Path, name: &str) -> Result<install::Built> {
    let indexes = forge_indexes(root);
    let say = |m: &str| println!("{m}");
    let b = install::build(cfg, name, &indexes, &say)?;
    ui::ok(&format!(
        "{} {} made from {} Debian package(s): {} files, {} (kept the system's own for {} files)",
        b.forge_name,
        b.version,
        b.debs.len(),
        b.files,
        forge_deb::convert::human(b.archive_size),
        b.kept_system
    ));
    if !b.unresolved.is_empty() {
        ui::warn(&format!("{} file(s) need functions that no library on this system has; they may fail when started:", b.unresolved.len()));
        for (file, syms) in b.unresolved.iter().take(12) {
            println!("    {file}: {}{}", syms.iter().take(3).cloned().collect::<Vec<_>>().join(", "), if syms.len() > 3 { ", ..." } else { "" });
        }
        if b.unresolved.len() > 12 {
            println!("    ... and {} more files", b.unresolved.len() - 12);
        }
        ui::info("usually a system library was built without a feature this program needs; tell the forge maintainers which one");
    }
    Ok(b)
}

fn declared_packages(root: &Path) -> Vec<String> {
    let path = root.join("etc/forge/system.toml");
    std::fs::read_to_string(path).ok().and_then(|t| forge_schema::SystemDecl::parse(&t).ok()).map(|d| d.packages).unwrap_or_default()
}

fn install_cmd(cfg: &install::Config, root: &Path, i: InstallArgs) -> Result<()> {
    let declared = declared_packages(root);
    let mut names: Vec<String> = Vec::new();
    let mut needed: Vec<String> = Vec::new();
    let mut rebuilt = false;
    for n in &i.names {
        let b = build_one(cfg, root, n)?;
        rebuilt |= declared.contains(&b.forge_name);
        if !names.contains(&b.forge_name) {
            names.push(b.forge_name.clone());
        }
        for need in b.needs {
            if !crate::is_base_name(&need) && !needed.contains(&need) && !declared.contains(&need) {
                needed.push(need);
            }
        }
    }
    if !needed.is_empty() {
        ui::info(&format!("these forge packages hold libraries the new package uses, so they are installed on purpose too: {}", needed.join(", ")));
    }
    declare_repo(root, &cfg.repo_dir())?;
    let new: Vec<String> = names.into_iter().filter(|n| !declared.contains(n)).chain(needed).collect();
    if !new.is_empty() {
        crate::pkg_add(new, i.yes, root.to_path_buf())?;
    }
    if rebuilt {
        ui::info("applying the newly built version (this also upgrades your other packages, like `forge upgrade`)");
        crate::upgrade_with(root.to_path_buf(), i.yes)?;
    }
    Ok(())
}

fn upgrade_cmd(cfg: &install::Config, root: &Path, yes: bool) -> Result<()> {
    update(cfg)?;
    let profile = forge_store::Profile::new(root.join("forge/profiles/system"));
    let installed = profile.current_packages().unwrap_or_default();
    let mut any = false;
    for id in installed {
        let Some(deb) = id.name().strip_prefix(install::PREFIX) else { continue };
        if deb == "compat" {
            continue;
        }
        any = true;
        build_one(cfg, root, deb)?;
    }
    if !any {
        ui::info("no Debian packages are installed through forge");
        return Ok(());
    }
    declare_repo(root, &cfg.repo_dir())?;
    crate::upgrade_with(root.to_path_buf(), yes)
}

/// Make sure a Debian package is installed (adding and reading the Debian source first when there is none): what
/// `forge flatpak setup` uses to get Flatpak itself.
pub fn ensure_installed(name: &str, yes: bool) -> Result<()> {
    let root = Path::new("/");
    need_root(root)?;
    let cfg = install::Config::new(root);
    if !cfg.sources()?.sources.iter().any(|s| s.name == "debian") {
        source(&cfg, root, SourceAction::Add { name: "debian".into(), url: None, suite: None, components: Vec::new(), keys: Vec::new() })?;
    }
    update(&cfg)?;
    install_cmd(&cfg, root, InstallArgs { names: vec![name.to_string()], yes })
}
