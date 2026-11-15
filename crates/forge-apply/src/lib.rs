//! `forge apply`: co    if let Some(src) = forge_gen::microcode_image_path(root).filter(|_| package_names.iter().any(|n| n == "microcode") && under(root, "/boot").is_dir()) {verge the machine to the system declaration.
//!
//! Applying is two phases. [`plan_system`] reads the declaration and the
//! current state of the machine (under a root directory) and returns a
//! [`Plan`] without touching anything. [`apply`] then carries the plan out.
//! The CLI shows the plan as a preview and asks for confirmation in between.
//!
//! Everything is resolved under a `root` directory, so the whole thing can be
//! exercised against a scratch directory instead of `/`.

mod state;
mod users;

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use std::collections::BTreeSet;

use forge_schema::SystemDecl;
use forge_store::{FetchHandle, Index, LocalRepo, Profile, RemoteRepo, Store, StoreError, StoreId, Trust, is_url};
pub use state::State;

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("invalid {what} `{value}`")]
    InvalidName { what: &'static str, value: String },
    #[error("service `{0}` is declared but /etc/init.d/{0} does not exist")]
    ServiceMissing(String),
    #[error("reading {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("reading state file {path}: {message}")]
    BadState { path: PathBuf, message: String },
    #[error("packages are declared but no `repos` are configured in the declaration")]
    NoRepo,
    #[error("package store: {0}")]
    Store(String),
}

fn store_err(e: forge_store::StoreError) -> PlanError {
    PlanError::Store(e.to_string())
}

#[derive(Debug, thiserror::Error)]
#[error("applying `{summary}` ({path}): {source}")]
pub struct ApplyError {
    pub summary: String,
    pub path: PathBuf,
    pub source: io::Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Add,
    Modify,
    Remove,
}

impl Kind {
    fn mark(self) -> &'static str {
        match self {
            Kind::Add => "[+]",
            Kind::Modify => "[~]",
            Kind::Remove => "[-]",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    WriteFile { path: PathBuf, content: String, mode: u32 },
    RemoveFile(PathBuf),
    Symlink { link: PathBuf, target: PathBuf },
    RemoveLink(PathBuf),
    /// Verify an archive from a repository and install it into the store.
    /// `url` is set for a remote repository: the archive is downloaded first.
    Install { store_root: PathBuf, archive: PathBuf, url: Option<String>, fetch: FetchHandle, sha256: String, id: StoreId },
    /// Shown in the preview; the real work is the profile switch that follows.
    Note(PathBuf),
    /// Run a program (the shadow tools for users and groups, systemd's sysusers and tmpfiles). A best-effort one may fail.
    Run { program: PathBuf, args: Vec<String>, best_effort: bool },
    /// Copy a file (the CPU microcode image into /boot). An optional one is skipped when the source is not there.
    CopyFile { from: PathBuf, to: PathBuf, optional: bool },
}

impl Op {
    fn path(&self) -> &Path {
        match self {
            Op::WriteFile { path, .. } | Op::RemoveFile(path) | Op::RemoveLink(path) | Op::Note(path) => path,
            Op::Symlink { link, .. } => link,
            Op::Install { archive, .. } => archive,
            Op::Run { program, .. } => program,
            Op::CopyFile { to, .. } => to,
        }
    }
}

/// Make the system profile show exactly these packages (after they are installed).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileSwitch {
    root: PathBuf,
    store_root: PathBuf,
    profile_dir: PathBuf,
    ids: BTreeSet<StoreId>,
    adopt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub section: &'static str,
    pub kind: Kind,
    pub summary: String,
    op: Op,
}

impl Change {
    /// The package this change installs or removes (changes in the `packages` section), if any.
    pub fn package_id(&self) -> Option<StoreId> {
        if self.section != "packages" {
            return None;
        }
        StoreId::parse(self.summary.split_whitespace().next()?)
    }

    /// Changes that must come after the packages are in the tree: the init links and units, and users (whose
    /// shells and tools come from packages).
    fn is_late(&self) -> bool {
        self.section == "init" || self.section == "users" || self.section == "shell" || self.section == "boot"
    }

    /// True when applying this change downloads an archive.
    pub fn downloads(&self) -> bool {
        matches!(&self.op, Op::Install { url: Some(_), .. })
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.kind.mark(), self.section, self.summary)
    }
}

/// Declared in the system declaration but not applied by this version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub section: &'static str,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub pending: Vec<Pending>,
    state_write: Option<(PathBuf, String)>,
    profile_switch: Option<ProfileSwitch>,
}

impl Plan {
    /// True when there is nothing to apply (pending items are not applied).
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.state_write.is_none() && self.profile_switch.is_none()
    }

    /// True when this plan changes which init system the machine boots (it takes effect at the next boot).
    pub fn changes_init(&self) -> bool {
        self.changes.iter().any(|c| c.section == "init" && c.summary.starts_with("/usr/sbin/init"))
    }
}

/// How `forge apply` should behave.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Re-apply everything, even what already matches the declaration.
    pub rebuild: bool,
    /// How far to trust repository indexes. The default trusts nothing, so a
    /// repository is refused unless a keyring vouches for it (or the caller
    /// explicitly chooses `Trust::AllowUnsigned`).
    pub trust: Trust,
    /// How remote repositories are downloaded from (curl or BusyBox wget; tests substitute a fake).
    pub fetch: FetchHandle,
    /// Replace files in the system tree that forge did not put there (the first apply over a system that was
    /// not installed by forge). Off by default: such files are the user's.
    pub adopt: bool,
    /// Packages every system has (the base system), installed without being declared. Empty: only what is declared.
    pub base: Vec<String>,
}

fn under(root: &Path, abs: &str) -> PathBuf {
    root.join(abs.strip_prefix('/').unwrap_or(abs))
}

fn valid(s: &str, extra: &[char]) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

fn check(what: &'static str, value: &str, extra: &[char]) -> Result<(), PlanError> {
    if valid(value, extra) {
        Ok(())
    } else {
        Err(PlanError::InvalidName { what, value: value.into() })
    }
}

fn read_opt(path: &Path) -> Result<Option<String>, PlanError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PlanError::Read { path: path.to_path_buf(), source }),
    }
}

struct Planner<'a> {
    root: &'a Path,
    opts: Options,
    changes: Vec<Change>,
}

impl Planner<'_> {
    fn file(&mut self, section: &'static str, abs: &str, content: String) -> Result<(), PlanError> {
        self.file_mode(section, abs, content, 0o644)
    }

    fn file_mode(&mut self, section: &'static str, abs: &str, content: String, mode: u32) -> Result<(), PlanError> {
        let path = under(self.root, abs);
        let (kind, note) = match read_opt(&path)? {
            None => (Kind::Add, ""),
            Some(cur) if cur != content => (Kind::Modify, ""),
            Some(_) if self.opts.rebuild => (Kind::Modify, " (re-apply)"),
            Some(_) => return Ok(()),
        };
        // A one-line file shows its value; a longer one just its path.
        let body: Vec<&str> = content.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).collect();
        let summary = match body.as_slice() {
            [one] => format!("{abs} = {one}{note}"),
            _ => format!("{abs}{note}"),
        };
        self.changes.push(Change {
            section,
            kind,
            summary,
            op: Op::WriteFile { path, content, mode },
        });
        Ok(())
    }

    /// A file forge owns outside the packages (the init links and power commands): make it match `entry`,
    /// replacing whatever is there, including a link where a script is wanted and the other way round.
    fn plumb(&mut self, abs: &str, entry: &forge_schema::Entry) {
        use forge_schema::Entry;
        match entry {
            Entry::Link(target) => self.link("init", abs, target),
            Entry::Absent => self.unlink("init", abs),
            Entry::File { content, mode } => {
                let path = under(self.root, abs);
                let meta = path.symlink_metadata().ok();
                let same = meta.as_ref().is_some_and(|m| m.is_file()) && fs::read_to_string(&path).is_ok_and(|c| &c == content);
                if same && !self.opts.rebuild {
                    return;
                }
                self.changes.push(Change {
                    section: "init",
                    kind: if meta.is_some() { Kind::Modify } else { Kind::Add },
                    summary: format!("{abs} = {}", content.lines().find(|l| !l.starts_with('#') && !l.is_empty()).unwrap_or("")),
                    op: Op::WriteFile { path, content: content.clone(), mode: *mode },
                });
            }
        }
    }

    fn link(&mut self, section: &'static str, abs_link: &str, target: &str) {
        let link = under(self.root, abs_link);
        let want = PathBuf::from(target);
        let (kind, note) = match fs::read_link(&link) {
            Ok(cur) if cur == want => {
                if !self.opts.rebuild {
                    return;
                }
                (Kind::Modify, " (re-apply)")
            }
            Ok(_) => (Kind::Modify, ""),
            Err(_) if link.symlink_metadata().is_ok() => (Kind::Modify, ""),
            Err(_) => (Kind::Add, ""),
        };
        self.changes.push(Change {
            section,
            kind,
            summary: format!("{abs_link} -> {target}{note}"),
            op: Op::Symlink { link, target: want },
        });
    }

    fn unlink(&mut self, section: &'static str, abs_link: &str) {
        let link = under(self.root, abs_link);
        if link.symlink_metadata().is_ok() {
            self.changes.push(Change {
                section,
                kind: Kind::Remove,
                summary: abs_link.to_string(),
                op: Op::RemoveLink(link),
            });
        }
    }
}

const MODULES_FILE: &str = "/etc/modules-load.d/forge.conf";
const MANAGED_HEADER: &str = "# managed by forge; edit /etc/forge/system.toml instead\n";

/// Work out what has to change to match `decl`. Reads the machine, writes nothing.
pub fn plan_system(decl: &SystemDecl, root: &Path, opts: Options) -> Result<Plan, PlanError> {
    check("hostname", &decl.hostname, &['-', '.'])?;
    check("timezone", &decl.timezone, &['/', '_', '+', '-'])?;
    if decl.timezone.contains("..") || decl.timezone.starts_with('/') {
        return Err(PlanError::InvalidName { what: "timezone", value: decl.timezone.clone() });
    }
    check("locale", &decl.locale, &['.', '_', '@', '-'])?;
    check("keymap", &decl.keymap, &['-', '_'])?;
    for m in &decl.kernel_modules {
        check("kernel module", m, &['_', '-'])?;
    }
    for s in &decl.init.services {
        check("service", s, &['_', '-', '.'])?;
    }

    let system = decl.init.system.as_str();
    let openrc = system == "openrc";
    let systemd = system == "systemd";
    let old_state = State::load(root)?;
    // The machine boots with the init system it was last set up for (the image ships OpenRC). A different one in
    // the declaration means switching: new packages, the init links, the default units. `--rebuild` redoes it.
    let switching = match old_state.init.as_deref() {
        Some(current) => current != system,
        None => system != "openrc",
    };
    let mut p = Planner { root, opts, changes: Vec::new() };

    p.file("hostname", "/etc/hostname", format!("{}\n", decl.hostname))?;
    p.link(
        "timezone",
        "/etc/localtime",
        &format!("/usr/share/zoneinfo/{}", decl.timezone),
    );
    p.file("locale", "/etc/locale.conf", format!("LANG={}\n", decl.locale))?;
    p.file("environment", "/etc/profile.d/forge.sh", profile_env())?;

    let mut pending = Vec::new();

    if openrc {
        p.file("keymap", "/etc/conf.d/keymaps", format!("keymap=\"{}\"\n", decl.keymap))?;
    } else if systemd {
        p.file("keymap", "/etc/vconsole.conf", format!("KEYMAP={}\n", decl.keymap))?;
    } else {
        pending.push(Pending {
            section: "keymap",
            summary: format!("keymap `{}`: only OpenRC is supported so far", decl.keymap),
        });
    }

    let modules_current = read_opt(&under(root, MODULES_FILE))?;
    if decl.kernel_modules.is_empty() {
        if modules_current.is_some() {
            p.changes.push(Change {
                section: "kernel modules",
                kind: Kind::Remove,
                summary: MODULES_FILE.to_string(),
                op: Op::RemoveFile(under(root, MODULES_FILE)),
            });
        }
    } else {
        let mut content = String::from(MANAGED_HEADER);
        for m in &decl.kernel_modules {
            content.push_str(m);
            content.push('\n');
        }
        p.file("kernel modules", MODULES_FILE, content)?;
    }

    let mut declared: Vec<String> = decl.init.services.clone();
    declared.sort();
    declared.dedup();
    let mut new_state = old_state.clone();

    if openrc {
        for s in &declared {
            if !under(root, &format!("/etc/init.d/{s}")).exists() {
                return Err(PlanError::ServiceMissing(s.clone()));
            }
            p.link(
                "services",
                &format!("/etc/runlevels/default/{s}"),
                &format!("/etc/init.d/{s}"),
            );
        }
        // Only services forge itself enabled earlier may be disabled again;
        // anything else on the machine is left alone.
        for s in old_state.managed_services.iter().filter(|s| !declared.contains(s)) {
            p.unlink("services", &format!("/etc/runlevels/default/{s}"));
        }
        new_state.managed_services = declared;
    } else if systemd {
        // Units are enabled the way `systemctl enable` does it: a link in the target's `.wants` directory. The
        // units themselves come with their packages, which are installed first (these changes run after them).
        let wants = |unit: &str| format!("/etc/systemd/system/multi-user.target.wants/{unit}");
        for s in &declared {
            let unit = forge_schema::systemd_unit(s);
            p.link("init", &wants(&unit), &format!("/usr/lib/systemd/system/{unit}"));
        }
        for s in old_state.managed_services.iter().filter(|s| !declared.contains(s)) {
            p.unlink("init", &wants(&forge_schema::systemd_unit(s)));
        }
        new_state.managed_services = declared;
    } else {
        for s in &declared {
            pending.push(Pending {
                section: "services",
                summary: format!("service `{s}`: only OpenRC is supported so far"),
            });
        }
    }

    // Under OpenRC the clock is kept by chrony (systemd has its own timesyncd), started in the default runlevel. The
    // service belongs to the init system's package set, so it is only enabled where that set is installed.
    if openrc && (!p.opts.base.is_empty() || old_state.init.is_some()) {
        p.link("init", "/etc/runlevels/default/chronyd", "/etc/init.d/chronyd");
    }
    users::plan_users(&mut p, decl)?;
    // The init link and the power commands are kept right on every apply (on a real system, which has /usr/sbin), not
    // only when the init system changes: whatever put something else there (an older forge, a package) is corrected.
    // The link points at the init system the declaration names; it takes effect at the next boot.
    if under(root, "/usr/sbin").is_dir() {
        for (path, entry) in forge_schema::plumbing(system) {
            p.plumb(path, &entry);
        }
    }
    if switching {
        let stub = "/run/systemd/resolve/stub-resolv.conf";
        let baseline = |unit: &str, target: &str| format!("/etc/systemd/system/{target}.wants/{unit}");
        if systemd {
            for (unit, target) in forge_schema::SYSTEMD_BASELINE {
                p.link("init", &baseline(unit, target), &format!("/usr/lib/systemd/system/{unit}"));
            }
            for (path, content) in forge_schema::SYSTEMD_NETWORK_FILES.iter().chain(forge_schema::SYSTEMD_WIFI_FILES.iter()) {
                p.file("init", path, content.to_string())?;
            }
            // resolved keeps /etc/resolv.conf pointing at its stub resolver.
            p.link("init", "/etc/resolv.conf", stub);
        } else {
            for (unit, target) in forge_schema::SYSTEMD_BASELINE {
                p.unlink("init", &baseline(unit, target));
            }
            // Leaving systemd: a resolv.conf that points at its (now gone) stub would break name lookups.
            if fs::read_link(under(root, "/etc/resolv.conf")).is_ok_and(|t| t == Path::new(stub)) {
                p.unlink("init", "/etc/resolv.conf");
            }
        }
        new_state.init = (system != "openrc" || old_state.init.is_some()).then(|| system.to_string());
    }
    let package_names = effective_packages(decl, old_state.init.is_some(), &p.opts.base);
    let profile_switch = plan_packages(&package_names, decl, root, &mut p, &old_state.repo_serials, &mut new_state.repo_serials)?;
    // CPU microcode: the package's early image for this machine's CPU is kept in /boot as microcode.img, which every
    // Limine entry loads first. Copied after the package (it may have just been installed or updated); the next
    // generation rewrites limine.conf, which is when the entries start naming it.
    let microcode_src = forge_gen::microcode_image_path(root);
    if let (Some(src), true) = (microcode_src, package_names.iter().any(|n| n == "microcode") && under(root, "/boot").is_dir()) {
        let dest = under(root, "/boot/microcode.img");
        let same = matches!((fs::read(&src), fs::read(&dest)), (Ok(a), Ok(b)) if a == b);
        if (src.is_file() && !same) || profile_switch.is_some() {
            p.changes.push(Change {
                section: "boot",
                kind: if dest.exists() { Kind::Modify } else { Kind::Add },
                summary: "/boot/microcode.img (CPU microcode, loaded before the kernel starts)".into(),
                op: Op::CopyFile { from: src, to: dest, optional: true },
            });
        }
    }
    // systemd's own users (journal, network, ...) and dbus's runtime directories are created at boot, but doing it
    // now means the first boot starts with them in place (a dbus that races its user at first boot is not a good
    // first impression). Best effort: the units order themselves after these at boot anyway.
    if systemd && root == Path::new("/") && (switching || profile_switch.is_some()) {
        for (summary, tool, args) in [
            ("systemd system users (sysusers)", "/usr/bin/systemd-sysusers", vec![]),
            ("dbus runtime directories (tmpfiles)", "/usr/bin/systemd-tmpfiles", vec!["--create".to_string(), "/usr/lib/tmpfiles.d/dbus.conf".to_string()]),
        ] {
            p.changes.push(Change { section: "init", kind: Kind::Add, summary: summary.into(), op: Op::Run { program: PathBuf::from(tool), args, best_effort: true } });
        }
    }

    // Installs and modifications first, removals last, so a swap never leaves a gap.
    let mut changes = p.changes;
    changes.sort_by_key(|c| c.kind == Kind::Remove);

    let state_write = new_state.render_if_changed(root, &old_state);
    Ok(Plan { changes, pending, state_write, profile_switch })
}

const STORE_DIR: &str = "/forge/store";
const SYSTEM_PROFILE_DIR: &str = "/forge/profiles/system";

/// The standard directories on `PATH` for login shells. Packages are copied into /usr by `forge apply`, so there is
/// no profile directory to add (and the store's links, which carry no setuid bit, must never come first).
fn profile_env() -> String {
    "# managed by forge: the system's programs are in /usr\n\
     for d in /usr/local/sbin /usr/local/bin /usr/sbin /usr/bin; do\n\
     \x20   case \":$PATH:\" in *\":$d:\"*) ;; *) PATH=\"$PATH:$d\" ;; esac\n\
     done\n\
     export PATH\n"
        .to_string()
}

/// The packages to have installed: the declared ones, without the other init systems' packages, plus the chosen
/// init system's own. (A declaration that lists no packages and keeps OpenRC asks for nothing: the image already has it.)
fn effective_packages(decl: &SystemDecl, switched_before: bool, base: &[String]) -> Vec<String> {
    let system = decl.init.system.as_str();
    // Without a base set (tests, minimal repositories) the image's own OpenRC is expected in the declaration; with
    // one, the init system's packages are always part of the system, whatever the declaration lists.
    let add_init = system != "openrc" || switched_before || !base.is_empty();
    let foreign = forge_schema::foreign_init_packages(system);
    let mut names: Vec<String> = base.to_vec();
    names.extend(decl.packages.iter().filter(|p| !foreign.contains(&p.as_str())).cloned());
    if add_init {
        names.extend(forge_schema::init_packages(system).iter().map(|s| s.to_string()));
    }
    names.extend(users::shell_packages(decl));
    let mut seen = BTreeSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// Resolve the packages against the repositories and plan the
/// installs plus the profile switch. Nothing is written here.
fn plan_packages(
    names: &[String],
    decl: &SystemDecl,
    root: &Path,
    p: &mut Planner,
    seen_serials: &std::collections::BTreeMap<String, u64>,
    new_serials: &mut std::collections::BTreeMap<String, u64>,
) -> Result<Option<ProfileSwitch>, PlanError> {
    let store = Store::new(under(root, STORE_DIR));
    let profile = Profile::new(under(root, SYSTEM_PROFILE_DIR));
    let current = profile.current_packages().map_err(store_err)?;

    // id -> (archive path, archive sha256) for packages that must be fetched.
    let mut target: BTreeSet<StoreId> = BTreeSet::new();
    // Where each missing archive comes from: a file, or a URL to download first.
    let mut sources: std::collections::BTreeMap<StoreId, (PathBuf, Option<String>, String)> = Default::default();

    if !names.is_empty() {
        if decl.repos.is_empty() {
            return Err(PlanError::NoRepo);
        }
        let mut repos: Vec<(Repo, Index)> = Vec::new();
        for r in &decl.repos {
            let (repo, label) = if is_url(r) {
                (Repo::Remote(RemoteRepo::new(r.as_str())), PathBuf::from(r))
            } else {
                (Repo::Local(LocalRepo::new(under(root, r))), under(root, r))
            };
            let loaded = repo.load(&p.opts.fetch, &p.opts.trust).map_err(store_err)?;
            // A signed index must not be older than one seen before: that would be an
            // attacker replaying a genuine but outdated repository (with the old,
            // vulnerable packages). Unsigned indexes carry no trustworthy serial.
            if loaded.signed_by.is_some() {
                let seen = seen_serials.get(r).copied().unwrap_or(0);
                if loaded.index.serial < seen {
                    return Err(store_err(StoreError::Rollback {
                        repo: label.clone(),
                        serial: loaded.index.serial,
                        seen,
                    }));
                }
                new_serials.insert(r.clone(), seen.max(loaded.index.serial));
            }
            repos.push((repo, loaded.index));
        }
        // Earlier repositories win when several offer the same build.
        let mut merged = Index::default();
        for (_, index) in &repos {
            for e in &index.packages {
                if merged.by_id(&e.id().map_err(store_err)?).is_none() {
                    merged.insert(e.clone());
                }
            }
        }
        for e in forge_store::resolve(&merged, names, decl.libc).map_err(store_err)? {
            let id = e.id().map_err(store_err)?;
            if !store.has(&id) {
                let (repo, entry) = repos
                    .iter()
                    .find_map(|(r, i)| i.by_id(&id).map(|e| (r, e)))
                    .expect("resolved from these indexes");
                let (path, url) = repo.archive_ref(entry).map_err(store_err)?;
                sources.insert(id.clone(), (path, url, entry.archive_sha256.clone()));
            }
            target.insert(id);
        }
    }

    for id in &target {
        match sources.remove(id) {
            Some((archive, url, sha256)) => p.changes.push(Change {
                section: "packages",
                kind: if current.contains(id) { Kind::Modify } else { Kind::Add },
                summary: format!("{id} (install from {})", archive.display()),
                op: Op::Install { store_root: store.root().to_path_buf(), archive, url, fetch: p.opts.fetch.clone(), sha256, id: id.clone() },
            }),
            None if !current.contains(id) => p.changes.push(Change {
                section: "packages",
                kind: Kind::Add,
                summary: format!("{id} (already in the store; link into the profile)"),
                op: Op::Note(store.root().to_path_buf()),
            }),
            None => {}
        }
    }
    for id in current.difference(&target) {
        p.changes.push(Change {
            section: "packages",
            kind: Kind::Remove,
            summary: format!("{id} (unlink from the profile; `forge store gc` frees the space)"),
            op: Op::Note(store.root().to_path_buf()),
        });
    }

    Ok((target != current).then(|| ProfileSwitch {
        root: root.to_path_buf(),
        store_root: store.root().to_path_buf(),
        profile_dir: profile.dir().to_path_buf(),
        ids: target,
        adopt: p.opts.adopt,
    }))
}

fn write_atomic(path: &Path, content: &str, mode: u32) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".forge-tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, content)?;
    fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

fn ignore_missing(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn run(op: &Op) -> io::Result<()> {
    match op {
        Op::WriteFile { path, content, mode } => write_atomic(path, content, *mode),
        Op::RemoveFile(p) | Op::RemoveLink(p) => ignore_missing(fs::remove_file(p)),
        Op::Symlink { link, target } => {
            if let Some(dir) = link.parent() {
                fs::create_dir_all(dir)?;
            }
            ignore_missing(fs::remove_file(link))?;
            symlink(target, link)
        }
        Op::Install { store_root, archive, url, fetch, sha256, id } => {
            // A remote archive is downloaded next to the store first; either way the store verifies
            // it against the hash from the (signed) index before anything is kept.
            let downloaded = match url {
                Some(u) => Some(
                    forge_store::download_to_temp(fetch.0.as_ref(), u, &store_root.join(".downloads"))
                        .map_err(|e| io::Error::other(e.to_string()))?,
                ),
                None => None,
            };
            let result = (|| {
                let file = fs::File::open(downloaded.as_deref().unwrap_or(archive))?;
                Store::new(store_root).install(file, Some(sha256)).map_err(|e| io::Error::other(e.to_string()))
            })();
            if let Some(tmp) = &downloaded {
                let _ = fs::remove_file(tmp);
            }
            let got = result?;
            if &got != id {
                return Err(io::Error::other(format!("the archive contained {got}, but the index promised {id}")));
            }
            Ok(())
        }
        Op::Note(_) => Ok(()),
        Op::CopyFile { from, to, optional } => {
            if !from.is_file() && *optional {
                return Ok(());
            }
            let mut tmp = to.as_os_str().to_owned();
            tmp.push(".forge-new");
            let tmp = PathBuf::from(tmp);
            fs::copy(from, &tmp)?;
            fs::rename(&tmp, to).inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            })
        }
        Op::Run { program, args, best_effort } => {
            let out = std::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).output();
            match out {
                Ok(o) if o.status.success() => Ok(()),
                Ok(_) | Err(_) if *best_effort => Ok(()),
                Ok(o) => Err(io::Error::other(format!("{} failed: {}", program.display(), String::from_utf8_lossy(&o.stderr).trim()))),
                Err(e) => Err(e),
            }
        }
    }
}

/// Carry out a plan, stopping at the first failure. Changes are applied in plan
/// order, and the bookkeeping state is written last, only if everything worked.
pub fn apply(plan: &Plan) -> Result<(), ApplyError> {
    apply_with(plan, &mut |_| {})
}

/// What [`apply_with`] reports while it works, so a front end can show progress.
#[derive(Debug)]
pub enum Progress<'a> {
    /// A change is starting (`index` of `total`, counting only changes that do work).
    Start { index: usize, total: usize, change: &'a Change },
    Done { index: usize, total: usize, change: &'a Change },
    /// The packages' files are being copied into the system tree.
    Copying,
    /// The new package set is being recorded.
    Recording,
}

/// Carry out a plan like [`apply`], telling `on` what is happening.
pub fn apply_with(plan: &Plan, on: &mut dyn FnMut(Progress<'_>)) -> Result<(), ApplyError> {
    let total = plan.changes.iter().filter(|c| !matches!(c.op, Op::Note(_))).count();
    let mut index = 0;
    // The init links and units come last: they point into packages, and until those are in the tree a half-done switch
    // must leave the machine booting the way it did.
    let mut step = |c: &Change, on: &mut dyn FnMut(Progress<'_>)| -> Result<(), ApplyError> {
        if matches!(c.op, Op::Note(_)) {
            return Ok(());
        }
        index += 1;
        on(Progress::Start { index, total, change: c });
        run(&c.op).map_err(|source| ApplyError { summary: c.summary.clone(), path: c.op.path().to_path_buf(), source })?;
        on(Progress::Done { index, total, change: c });
        Ok(())
    };
    for c in plan.changes.iter().filter(|c| !c.is_late()) {
        step(c, on)?;
    }
    if let Some(sw) = &plan.profile_switch {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let store = Store::new(&sw.store_root);
        // The system itself: the packages' files copied into the real tree, replacing what the last apply put
        // there. The profile above is the record of which generation of packages this is.
        let fail = |what: &str, e: StoreError| ApplyError {
            summary: format!("{what} the system tree"),
            path: sw.root.clone(),
            source: io::Error::other(e.to_string()),
        };
        on(Progress::Copying);
        let old = forge_store::Materialized::load(&sw.root).map_err(|e| fail("read the state of", e))?;
        let state = forge_store::materialize(&store, &sw.ids, &sw.root, &old, &forge_store::MaterializeOptions { adopt: sw.adopt })
            .map_err(|e| fail("update", e))?;
        state.save(&sw.root).map_err(|e| fail("record the state of", e))?;
        on(Progress::Recording);
        // Only now, with the tree updated, is the new set recorded as a generation (so a failed update, a conflict
        // for instance, leaves the record saying what is really installed).
        Profile::new(&sw.profile_dir).record(&sw.ids, now).map_err(|e| ApplyError {
            summary: "record the system generation".into(),
            path: sw.profile_dir.clone(),
            source: io::Error::other(e.to_string()),
        })?;
    }
    for c in plan.changes.iter().filter(|c| c.is_late()) {
        step(c, on)?;
    }
    if let Some((path, content)) = &plan.state_write {
        write_atomic(path, content, 0o644).map_err(|source| ApplyError {
            summary: "forge state".into(),
            path: path.clone(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// A repository named in the declaration: a directory, or an HTTP(S) base URL.
enum Repo {
    Local(LocalRepo),
    Remote(RemoteRepo),
}

impl Repo {
    fn load(&self, fetch: &FetchHandle, trust: &Trust) -> Result<forge_store::Loaded, StoreError> {
        match self {
            Repo::Local(r) => r.load(trust),
            Repo::Remote(r) => r.load(fetch.0.as_ref(), trust),
        }
    }

    /// Where an archive comes from: its path (for a remote one, the URL as a path, only for display) and the
    /// URL to download it from, if remote.
    fn archive_ref(&self, e: &forge_store::IndexEntry) -> Result<(PathBuf, Option<String>), StoreError> {
        match self {
            Repo::Local(r) => Ok((r.archive_path(e), None)),
            Repo::Remote(r) => {
                let url = r.archive_url(e)?;
                Ok((PathBuf::from(&url), Some(url)))
            }
        }
    }
}
