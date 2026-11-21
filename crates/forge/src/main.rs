use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use forge_kernel::Http as _;
use forge_schema::{Recipe, SystemDecl, UserDecl};
use forge_strap::Firmware;

mod deb_cmd;
mod flatpak_cmd;
mod doctor;
mod maintenance;
mod ui;

#[derive(Parser)]
#[command(name = "forge", version, about = "Singularity's declarative system and package tool")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install packages: adds them to the system declaration and applies it
    Install(PkgArgs),
    /// Remove packages from the system declaration and apply it
    Remove(PkgArgs),
    /// Check the system over (init link, boot entries, base packages, users, services, clock) and say what to run about any problem
    Doctor(DoctorArgs),
    /// Edit the system declaration (with micro, by default), check it, and apply it when you close the editor
    Edit(EditArgs),
    /// Upgrade every declared package to the newest version in the repositories
    Upgrade(UpgradeArgs),
    /// List the packages you installed, and everything in the system profile
    List(ListArgs),
    /// Search the package repositories by name
    Search(SearchArgs),
    /// Add or remove packages in a declaration (the older spelling of install and remove)
    #[command(hide = true)]
    Pkg {
        #[command(subcommand)]
        action: PkgAction,
    },
    /// Install packages from Debian (or any apt repository) that forge does not have
    Deb(deb_cmd::DebArgs),
    /// Install and update Flatpak applications (Astralis and others from Flathub)
    Flatpak(flatpak_cmd::FlatpakArgs),
    /// Work with the package store: pack, publish, install, list, gc
    Store(StoreArgs),
    /// Create signing keys and manage which public keys are trusted
    Key(KeyArgs),
    /// Sign and verify package repositories
    Repo(RepoArgs),
    /// Build the live ISO (kernel, initramfs and squashfs system, booted by Limine)
    Iso(IsoArgs),
    /// Validate a declaration or recipe file
    Check {
        #[arg(value_enum)]
        kind: CheckKind,
        file: PathBuf,
    },
    /// Install Singularity from the live system onto partitions you made (ERASES them)
    Strap(StrapArgs),
    /// Converge the machine to the system declaration
    Apply(ApplyArgs),
    /// Manage generations (read-only snapshots with their own boot entries)
    Gen(GenArgs),
    /// Secure Boot with a key of your own: forge signs the boot loader and pins what it may load
    Secureboot(SecurebootArgs),
    /// Routine maintenance: trim the SSD, scrub btrfs, prune old generations, clean the store
    Maintenance(MaintenanceArgs),
    /// Switch init system (not implemented yet)
    SwapInit,
    /// Find, build and install kernels (every driver as a module, trimmed to this machine, or by hand)
    Kernel(KernelArgs),
    /// Build a recipe from source, in a sandbox, into a package archive
    Build(BuildArgs),
    /// Internal: the sandbox helper that `forge build` re-runs itself as
    #[command(name = "__sandbox", hide = true)]
    SandboxChild { config: PathBuf },
    /// Internal: check whether the sandbox can be created on this machine
    #[command(name = "__sandbox-probe", hide = true)]
    SandboxProbe,
}

#[derive(Clone, clap::ValueEnum)]
enum CheckKind {
    System,
    User,
    Recipe,
}

#[derive(Subcommand)]
enum PkgAction {
    /// Add a package to the declaration
    Add(PkgArgs),
    /// Remove a package from the declaration
    Remove(PkgArgs),
}

#[derive(Args)]
struct PkgArgs {
    /// One or more package names
    #[arg(required = true)]
    names: Vec<String>,
    /// Edit the system declaration (the default when you are root) instead of your user declaration
    #[arg(long)]
    system: bool,
    /// Edit this file instead of the default location
    #[arg(long)]
    file: Option<PathBuf>,
    /// Only edit the declaration; do not apply it
    #[arg(long)]
    no_apply: bool,
    /// Do not ask for confirmation when applying
    #[arg(long, short = 'y')]
    yes: bool,
    /// Work under this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Directory of trusted public keys (default: <root>/etc/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
    /// Replace files in the system tree that forge did not put there
    #[arg(long)]
    adopt: bool,
    /// With `remove`: also delete the settings the package leaves behind (its files under /etc and its directories in /var)
    #[arg(long)]
    full: bool,
}

#[derive(Args)]
struct EditArgs {
    /// The declaration to edit (default: <root>/etc/forge/system.toml)
    file: Option<PathBuf>,
    /// The editor to use (default: $VISUAL, $EDITOR, then micro, nano or vi, whichever is installed)
    #[arg(long)]
    editor: Option<String>,
    /// Only check and save the declaration; do not apply it
    #[arg(long)]
    no_apply: bool,
    /// Do not ask before applying
    #[arg(long, short = 'y')]
    yes: bool,
    /// Work under this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Directory of trusted public keys (default: <root>/etc/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
    /// Replace files in the system tree that forge did not put there
    #[arg(long)]
    adopt: bool,
}

/// The editor to run: the flag, `$VISUAL`, `$EDITOR`, or the first of micro, nano and vi that exists.
fn pick_editor(flag: Option<String>) -> Result<String> {
    let from_env = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
    if let Some(e) = flag.or_else(|| from_env("VISUAL")).or_else(|| from_env("EDITOR")) {
        return Ok(e);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["micro", "nano", "vi"] {
        if std::env::split_paths(&path).any(|d| d.join(name).is_file()) {
            return Ok(name.to_string());
        }
    }
    bail!("no editor found: install one (forge install micro) or pass --editor")
}

/// Edit the system declaration the way a person wants to: open the editor on a copy, check the result when the
/// editor closes (a mistake goes back into the editor instead of breaking the system), save it, and apply it.
fn edit_cmd(a: EditArgs) -> Result<()> {
    let path = a.file.clone().unwrap_or_else(|| a.root.join("etc/forge/system.toml"));
    let system = a.file.is_none();
    if system && a.root == Path::new("/") && !is_root() {
        bail!("editing the system declaration needs root (try: sudo forge edit)");
    }
    let original = read_system_decl(&path)?;
    let editor = pick_editor(a.editor.clone())?;
    let mut parts = editor.split_whitespace();
    let program = parts.next().context("the editor command is empty")?.to_string();
    let editor_args: Vec<String> = parts.map(String::from).collect();

    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("system.toml");
    let work = dir.join(format!(".{name}.editing"));
    std::fs::write(&work, &original).with_context(|| format!("writing {}", work.display()))?;
    let cleanup = |p: &Path| {
        let _ = std::fs::remove_file(p);
    };

    let edited = loop {
        let status = std::process::Command::new(&program).args(&editor_args).arg(&work).status().with_context(|| format!("running {program}"))?;
        if !status.success() {
            cleanup(&work);
            bail!("{program} did not finish normally, so {} was not changed", path.display());
        }
        let text = std::fs::read_to_string(&work).with_context(|| format!("reading {}", work.display()))?;
        if text == original {
            cleanup(&work);
            ui::info(&format!("no changes to {}", path.display()));
            return Ok(());
        }
        match SystemDecl::parse(&text) {
            Ok(_) => break text,
            Err(e) => {
                ui::warn(&format!("the declaration is not valid: {e}"));
                if !ui::confirm("edit it again?")? {
                    cleanup(&work);
                    println!("discarded: {} was not changed", path.display());
                    return Ok(());
                }
            }
        }
    };
    cleanup(&work);
    std::fs::write(&path, &edited).with_context(|| format!("writing {}", path.display()))?;
    ui::ok(&format!("saved {}", path.display()));
    if a.no_apply {
        ui::info("not applied (--no-apply): run `forge apply` when you are ready");
        return Ok(());
    }
    apply(ApplyArgs {
        category: Category::System,
        file: Some(path),
        root: a.root,
        rebuild: false,
        dry_run: false,
        yes: a.yes,
        no_generation: false,
        keys: a.keys,
        allow_unsigned: a.allow_unsigned,
        upgrade: false,
        purge: false,
        adopt: a.adopt,
    })
}

#[derive(Args)]
struct UpgradeArgs {
    /// Show what would change and exit
    #[arg(long)]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    yes: bool,
    /// Work under this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Directory of trusted public keys (default: <root>/etc/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
    /// Replace files in the system tree that forge did not put there
    #[arg(long)]
    adopt: bool,
}

#[derive(Args)]
struct ListArgs {
    /// Work under this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
}

#[derive(Args)]
struct SearchArgs {
    /// Part of a package name (default: list everything)
    term: Option<String>,
    /// Look under this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Directory of trusted public keys (default: <root>/etc/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
}

#[derive(Args)]
struct ApplyArgs {
    /// Which declaration to apply (only `system` is implemented so far)
    #[arg(value_enum, default_value = "system")]
    category: Category,
    /// Declaration file (default: <root>/etc/forge/system.toml)
    #[arg(long)]
    file: Option<PathBuf>,
    /// Apply under this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Re-apply everything, even what already matches
    #[arg(long)]
    rebuild: bool,
    /// Show the plan and exit without changing anything
    #[arg(long)]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    yes: bool,
    /// Do not create a generation afterwards (not recommended)
    #[arg(long)]
    no_generation: bool,
    /// Directory of trusted public keys that repository indexes must be signed by
    /// (default: <root>/etc/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
    /// Replace files in the system tree that forge did not put there (the first apply over a system forge did not install)
    #[arg(long)]
    adopt: bool,
    /// Set by `forge upgrade` only: move installed packages to the newest versions (apply and install never do)
    #[arg(skip)]
    upgrade: bool,
    /// Set by `forge remove --full` only: delete the removed packages' settings too
    #[arg(skip)]
    purge: bool,
}

#[derive(Args)]
struct StoreArgs {
    #[command(subcommand)]
    action: StoreAction,
    /// The package store directory
    #[arg(long, global = true, default_value = "/forge/store")]
    store: PathBuf,
}

#[derive(Subcommand)]
enum StoreAction {
    /// Pack a directory into a package archive
    Pack(PackArgs),
    /// Add a package archive to a repository directory and update its index
    Publish {
        archive: PathBuf,
        /// The repository directory (created if needed)
        #[arg(long)]
        repo: PathBuf,
        /// Sign the index with this secret key (required to extend a signed repository)
        #[arg(long)]
        sign: Option<PathBuf>,
    },
    /// Verify a package archive and install it into the store
    Install(InstallArgs),
    /// List packages in the store
    List,
    /// Delete store entries no profile or generation uses
    Gc(GcArgs),
}

#[derive(Args)]
struct PackArgs {
    /// Directory whose contents become the package
    dir: PathBuf,
    #[arg(long)]
    name: String,
    #[arg(long)]
    version: String,
    #[arg(long)]
    libc: LibcArg,
    /// Run-time dependency, as a store id (repeatable)
    #[arg(long = "dep")]
    deps: Vec<String>,
    /// Build-input hash (64 hex); default: a hash of the package content
    #[arg(long)]
    hash: Option<String>,
    /// Archive file to write, e.g. hello-1.0.tar.zst
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args)]
struct InstallArgs {
    archive: PathBuf,
    /// Expected SHA-256 of the archive file
    #[arg(long, conflicts_with = "unverified")]
    sha256: Option<String>,
    /// Install without checking the archive's checksum (local testing only)
    #[arg(long)]
    unverified: bool,
}

#[derive(Args)]
struct GcArgs {
    /// Show what would be removed without removing it
    #[arg(long)]
    dry_run: bool,
    /// Profile directories that keep packages alive (default: the system
    /// profile and every user's profile found via /etc/passwd; needs root)
    #[arg(long = "profile")]
    profiles: Vec<PathBuf>,
    /// System generations directory (their `store_paths` keep packages alive)
    #[arg(long, default_value = "/.snapshots")]
    snapshots: PathBuf,
}

#[derive(Args)]
struct GenArgs {
    #[command(subcommand)]
    action: GenAction,
    #[command(flatten)]
    paths: GenPaths,
}

/// Where generations live. The defaults are for a real Singularity system.
#[derive(Args, Clone)]
struct GenPaths {
    /// The running root subvolume to snapshot
    #[arg(long, global = true, default_value = "/")]
    live: PathBuf,
    /// Snapshots directory (the @snapshots subvolume)
    #[arg(long, global = true, default_value = "/.snapshots")]
    snapshots: PathBuf,
    /// Mount of the top-level btrfs volume (subvolid=5); mounted for you when rolling back if not given
    #[arg(long, global = true)]
    top: Option<PathBuf>,
    /// limine.conf to (re)write
    #[arg(long, global = true, default_value = "/boot/limine.conf")]
    limine_conf: PathBuf,
    /// Directory holding the kernel and initramfs files
    #[arg(long, global = true, default_value = "/boot")]
    boot_dir: PathBuf,
    /// Kernel file name (default: vmlinuz-<running release>)
    #[arg(long, global = true)]
    kernel: Option<String>,
    /// Initramfs file name (default: initramfs-<running release>.img)
    #[arg(long, global = true)]
    initramfs: Option<String>,
    /// Use this kernel command line instead of /proc/cmdline
    #[arg(long, global = true)]
    cmdline: Option<String>,
    /// Testing only: "snapshot" by copying directories instead of using btrfs
    #[arg(long, global = true, hide = true)]
    copy_backend: bool,
}

impl GenPaths {
    fn defaults() -> Self {
        GenPaths {
            live: "/".into(),
            snapshots: "/.snapshots".into(),
            top: None,
            limine_conf: "/boot/limine.conf".into(),
            boot_dir: "/boot".into(),
            kernel: None,
            initramfs: None,
            cmdline: None,
            copy_backend: false,
        }
    }
}

#[derive(Subcommand)]
enum GenAction {
    /// Snapshot the live system as a new generation
    Create {
        /// What this generation is for
        #[arg(required = true, num_args = 1..)]
        description: Vec<String>,
    },
    /// List generations (* marks the current one)
    List,
    /// Show the current generation and the subvolume you booted
    Current,
    /// Remove one generation, or `all` (the current and booted ones are kept)
    Remove { target: String },
    /// Remove the oldest generations, keeping the newest N (the current and booted ones are always kept)
    Prune {
        #[arg(long, default_value_t = 10)]
        keep: usize,
    },
    /// Roll back to a generation (finishes after a reboot)
    Rollback { id: u32 },
    /// Delete the leftover @.next once a rollback is complete
    CleanupNext,
    /// Regenerate limine.conf (also completes a staged rollback)
    RewriteLimine,
}

#[derive(Clone, clap::ValueEnum)]
enum Category {
    System,
    User,
}

#[derive(Args)]
struct StrapArgs {
    #[arg(long, conflicts_with = "bios")]
    uefi: bool,
    #[arg(long)]
    bios: bool,
    /// Target root partition, e.g. /dev/sda2 (becomes btrfs; ERASED)
    root: String,
    /// Boot partition, e.g. /dev/sda1 (becomes FAT32; ERASED)
    boot: String,
    /// Directory to copy the system from
    #[arg(long, default_value = "/")]
    source: PathBuf,
    /// Where the target is mounted during the install
    #[arg(long, default_value = "/mnt/target")]
    mount_point: PathBuf,
    /// Directory holding BOOTX64.EFI and limine-bios.sys
    #[arg(long, default_value = "/usr/share/limine")]
    limine_dir: PathBuf,
    /// Hostname for the starter /etc/forge/system.toml
    #[arg(long, default_value = "singularity")]
    hostname: String,
    /// glibc or musl (default: detected from the copied system)
    #[arg(long)]
    libc: Option<LibcArg>,
    /// Kernel file in /boot (default: the newest vmlinuz-*)
    #[arg(long)]
    kernel: Option<String>,
    /// Initramfs file in /boot (default: initramfs-<kernel release>.img)
    #[arg(long)]
    initramfs: Option<String>,
    /// Extra kernel command line, after root=UUID=...
    #[arg(long, default_value = "")]
    cmdline_extra: String,
    /// Do not ask to create a user or change the root password
    #[arg(long)]
    no_users: bool,
    /// Do not add a UEFI firmware boot entry (the removable-media path still works)
    #[arg(long)]
    no_efi_entry: bool,
    /// Print the plan and exit without changing anything
    #[arg(long)]
    dry_run: bool,
    /// Skip the typed confirmation (for scripts and test images)
    #[arg(long)]
    yes: bool,
}

#[derive(Clone, clap::ValueEnum)]
enum LibcArg {
    Glibc,
    Musl,
}

#[derive(Args)]
struct BuildArgs {
    /// Directory containing recipe.toml (and any patches)
    recipe: PathBuf,
    /// glibc or musl (needed if the recipe supports both)
    #[arg(long)]
    libc: Option<LibcArg>,
    /// Repository to resolve dependencies from (repeatable)
    #[arg(long = "repo")]
    repos: Vec<PathBuf>,
    /// Where the finished archive is written
    #[arg(long, default_value = "packages")]
    out: PathBuf,
    /// Where build tools come from: `store` (reproducible) or `host`
    /// (the host's /usr; for bootstrapping only, not reproducible)
    #[arg(long, value_enum, default_value = "store")]
    tools: ToolsArg,
    /// The package store used for `--tools store`
    #[arg(long, default_value = "/forge/store")]
    store: PathBuf,
    /// Cache of downloaded sources (default: ~/.cache/forge/sources)
    #[arg(long)]
    source_cache: Option<PathBuf>,
    /// Parent of the temporary build directory (default: ~/.cache/forge/build)
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Parallel jobs for the build ($JOBS); default: the number of CPUs
    #[arg(long)]
    jobs: Option<u32>,
    /// Keep the temporary build directory (to inspect a failed build)
    #[arg(long)]
    keep_work: bool,
    /// Also add the result to this repository directory
    #[arg(long)]
    publish: Option<PathBuf>,
    /// Sign the published index with this secret key (needed to publish into a signed repository)
    #[arg(long, requires = "publish")]
    sign: Option<PathBuf>,
    /// Directory of trusted public keys that repository indexes must be signed by
    /// (default: ~/.config/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
}

#[derive(Clone, clap::ValueEnum)]
enum ToolsArg {
    Store,
    Host,
}

fn cache_dir(sub: &str) -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".cache"),
    };
    Ok(base.join("forge").join(sub))
}

fn build_cmd(a: BuildArgs) -> Result<()> {
    let recipe_text = std::fs::read_to_string(a.recipe.join("recipe.toml"))
        .with_context(|| format!("reading {}/recipe.toml", a.recipe.display()))?;
    // Load the signing key first: a bad path or permissions should fail now, not
    // after the build.
    let signer = a.sign.as_deref().map(forge_store::load_secret_key).transpose()?;
    let recipe = Recipe::parse(&recipe_text).context("in the recipe")?;
    let libc = match (&a.libc, recipe.libc.as_slice()) {
        (Some(l), _) => to_libc(l),
        (None, [only]) => *only,
        (None, _) => bail!("{} supports several libcs; choose one with --libc glibc|musl", recipe.name),
    };
    let opts = forge_build::BuildOptions {
        recipe_dir: a.recipe,
        libc,
        out_dir: a.out,
        work_dir: match a.work_dir {
            Some(d) => d,
            None => cache_dir("build")?,
        },
        source_cache: match a.source_cache {
            Some(d) => d,
            None => cache_dir("sources")?,
        },
        repos: a.repos,
        trust: load_trust(a.keys, a.allow_unsigned, user_dir("keys")?)?,
        fetch: Default::default(),
        tools: match a.tools {
            ToolsArg::Store => forge_build::Tools::Store,
            ToolsArg::Host => forge_build::Tools::Host,
        },
        store: a.store,
        jobs: a.jobs.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get() as u32)),
        helper: std::env::current_exe().context("finding the forge executable")?,
        keep_work: a.keep_work,
    };
    if !forge_build::sandbox::supported(&opts.helper) {
        bail!(
            "cannot create the build sandbox: unprivileged user namespaces are unavailable \
             (check /proc/sys/user/max_user_namespaces)"
        );
    }
    println!("building {} {} for {}", recipe.name, recipe.version, libc.as_str());
    let built = forge_build::build(&opts)?;
    for w in &built.warnings {
        eprintln!("warning: {w}");
    }
    println!("built {}", built.id);
    println!("archive: {}", built.archive.display());
    println!("archive sha256: {}", built.archive_sha256);
    if let Some(dir) = &built.work_dir {
        println!("kept the build directory: {}", dir.display());
    }
    if let Some(repo) = a.publish {
        forge_store::LocalRepo::new(&repo).publish(&built.archive, signer.as_ref())?;
        println!("published to {}", repo.display());
    }
    Ok(())
}

fn main() -> ExitCode {
    // The login umask on an installed system is 077 (private home files). What forge creates in /usr, /etc and the boot
    // files must be readable by everyone: services that run as their own user (networkd, resolved) could not even
    // reach their programs in a directory forge made with 0700.
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o022));
    // The sandbox helper reports the build's own exit status, so it bypasses
    // the normal error handling below.
    let cli = Cli::parse();
    match &cli.command {
        Cmd::SandboxChild { config } => {
            return ExitCode::from(forge_build::sandbox::child_main(config).clamp(0, 255) as u8);
        }
        Cmd::SandboxProbe => return ExitCode::from(forge_build::sandbox::probe_main() as u8),
        _ => {}
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("forge: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Install(a) => pkg(PkgAction::Add(a)),
        Cmd::Remove(a) => pkg(PkgAction::Remove(a)),
        Cmd::Doctor(a) => doctor_cmd(a),
        Cmd::Edit(a) => edit_cmd(a),
        Cmd::Upgrade(a) => {
            let live = a.root == Path::new("/") && !a.dry_run;
            upgrade(a)?;
            if live {
                flatpak_cmd::upgrade_declared();
            }
            Ok(())
        }
        Cmd::List(a) => list_cmd(a),
        Cmd::Search(a) => search_cmd(a),
        Cmd::Pkg { action } => pkg(action),
        Cmd::Deb(a) => deb_cmd::run(a),
        Cmd::Flatpak(a) => flatpak_cmd::run(a),
        Cmd::Store(a) => store_cmd(a),
        Cmd::Key(a) => key_cmd(a),
        Cmd::Repo(a) => repo_cmd(a),
        Cmd::Iso(a) => iso_cmd(a),
        Cmd::Check { kind, file } => check(kind, &file),
        Cmd::Strap(a) => strap(a),
        Cmd::Apply(a) => {
            let live = a.root == Path::new("/") && !a.dry_run && matches!(a.category, Category::System);
            apply(a)?;
            if live {
                flatpak_cmd::sync_declared();
            }
            Ok(())
        }
        Cmd::Gen(a) => gen_cmd(a),
        Cmd::Maintenance(a) => maintenance_cmd(a),
        Cmd::Secureboot(a) => secureboot_cmd(a),
        Cmd::SwapInit => todo_cmd("swap-init"),
        Cmd::Kernel(a) => kernel_cmd(a),
        Cmd::Build(a) => build_cmd(a),
        Cmd::SandboxChild { .. } | Cmd::SandboxProbe => unreachable!("handled in main"),
    }
}

/// Install packages into the system declaration and apply it (used by `forge deb`).
fn pkg_add(names: Vec<String>, yes: bool, root: PathBuf) -> Result<()> {
    pkg(PkgAction::Add(PkgArgs { names, system: true, file: None, no_apply: false, yes, root, keys: None, allow_unsigned: false, adopt: false, full: false }))
}

fn pkg_remove(names: Vec<String>, yes: bool, root: PathBuf) -> Result<()> {
    pkg(PkgAction::Remove(PkgArgs { names, system: true, file: None, no_apply: false, yes, root, keys: None, allow_unsigned: false, adopt: false, full: false }))
}

fn upgrade_with(root: PathBuf, yes: bool) -> Result<()> {
    upgrade(UpgradeArgs { dry_run: false, yes, root, keys: None, allow_unsigned: false, adopt: false })
}

fn todo_cmd(name: &str) -> Result<()> {
    bail!("`forge {name}` is not implemented yet")
}

fn decl_path(system: bool, file: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(f) = file {
        return Ok(f);
    }
    if system {
        return Ok(PathBuf::from("/etc/forge/system.toml"));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("forge/user.toml"))
}

fn pkg(action: PkgAction) -> Result<()> {
    let (args, adding) = match action {
        PkgAction::Add(a) => (a, true),
        PkgAction::Remove(a) => (a, false),
    };
    if args.full && adding {
        bail!("--full only goes with `forge remove` (it deletes the settings a removed package leaves behind)");
    }
    // Packages are installed for the whole system by default when you are root, and into your own
    // declaration otherwise (per-user installs are not implemented yet, so that only records the wish).
    let system = args.system || (args.file.is_none() && (is_root() || args.root != Path::new("/")));
    let path = match (&args.file, system) {
        (Some(f), _) => f.clone(),
        (None, true) => args.root.join("etc/forge/system.toml"),
        (None, false) => decl_path(false, None)?,
    };
    if system && args.root == Path::new("/") && !is_root() && args.file.is_none() {
        bail!("changing the system declaration needs root (try: sudo forge {} {})", if adding { "install" } else { "remove" }, args.names.join(" "));
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !system => String::new(),
        Err(_) => read_system_decl(&path)?,
    };

    for name in &args.names {
        if is_base_name(name) {
            if adding {
                ui::info(&format!("{name} is part of the base system: it is always installed"));
            } else {
                bail!("{name} is part of the base system, which forge keeps installed: it cannot be removed");
            }
        }
    }
    let args_names: Vec<String> = args.names.iter().filter(|n| !is_base_name(n)).cloned().collect();
    if args_names.is_empty() {
        return Ok(());
    }
    // All the names are edited into the declaration first, then it is applied once (one generation).
    let verb = if adding { "already in" } else { "not in" };
    let mut edited = forge_decl::Edited { text: text.clone(), changed: false };
    for name in &args_names {
        let e = if adding { forge_decl::add_package(&edited.text, name)? } else { forge_decl::remove_package(&edited.text, name)? };
        if e.changed {
            edited = e;
        } else {
            ui::info(&format!("{name} is {verb} {}", path.display()));
        }
    }
    if !edited.changed {
        return Ok(());
    }
    let label = args_names.join(" ");

    // For a system package, check that the edited declaration can actually be applied BEFORE writing it:
    // a typo or a package missing from the repositories must not leave a declaration that cannot apply.
    let opts = if system && !args.no_apply {
        let o = forge_apply::Options {
            rebuild: false,
            trust: load_trust(args.keys.clone(), args.allow_unsigned, args.root.join("etc/forge/keys"))?,
            fetch: Default::default(),
            adopt: args.adopt,
            upgrade: false,
            purge: args.full,
            base: base_packages(),
        };
        let decl = SystemDecl::parse(&edited.text).with_context(|| format!("in the edited {}", path.display()))?;
        forge_apply::plan_system(&decl, &args.root, o.clone())
            .with_context(|| format!("{} cannot be applied, so {} was not changed", label, path.display()))?;
        Some(o)
    } else {
        None
    };

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, edited.text).with_context(|| format!("writing {}", path.display()))?;
    ui::ok(&format!("{} {} {} {}", if adding { "added" } else { "removed" }, label, if adding { "to" } else { "from" }, path.display()));

    if !system {
        println!("note: per-user installs are not implemented yet, so nothing was installed;");
        println!("      for a system-wide package run: sudo forge {} {}", if adding { "install" } else { "remove" }, label);
        return Ok(());
    }
    if opts.is_none() {
        ui::info("not applied (--no-apply): run `forge apply` when you are ready");
        return Ok(());
    }
    apply(ApplyArgs {
        category: Category::System,
        file: Some(path),
        root: args.root,
        rebuild: false,
        dry_run: false,
        yes: args.yes,
        no_generation: false,
        keys: args.keys,
        allow_unsigned: args.allow_unsigned,
        upgrade: false,
        purge: args.full,
        adopt: args.adopt,
    })
}

fn check(kind: CheckKind, file: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    match kind {
        CheckKind::System => drop(SystemDecl::parse(&text)?),
        CheckKind::User => drop(UserDecl::parse(&text)?),
        CheckKind::Recipe => drop(Recipe::parse(&text)?),
    }
    println!("{}: ok", file.display());
    Ok(())
}

fn strap(a: StrapArgs) -> Result<()> {
    let firmware = if a.uefi {
        Firmware::Uefi
    } else if a.bios {
        Firmware::Bios
    } else if Path::new("/sys/firmware/efi").is_dir() {
        Firmware::Uefi
    } else {
        Firmware::Bios
    };
    let plan = forge_strap::plan(firmware, &a.root, Some(&a.boot))?;
    println!("Install plan ({:?}):", plan.firmware);
    for (i, step) in plan.steps.iter().enumerate() {
        println!("  {}. {step}", i + 1);
    }
    if a.dry_run {
        println!("dry run: nothing was changed");
        return Ok(());
    }
    if !is_root() {
        bail!("installing needs root");
    }

    use std::os::unix::fs::FileTypeExt;
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    forge_strap::preflight(&plan, &mounts, &|d| {
        std::fs::metadata(d).is_ok_and(|m| m.file_type().is_block_device())
    })?;

    let accounts = if a.no_users { forge_strap::Accounts::default() } else { collect_accounts()? };

    if !a.yes {
        println!();
        println!("This will ERASE {} and {}.", plan.root_dev, plan.boot_dev);
        let typed = prompt_line(&format!("Type {} to continue: ", plan.root_dev))?;
        if typed.trim() != plan.root_dev {
            println!("aborted: nothing was changed");
            return Ok(());
        }
    }

    let req = forge_strap::Request {
        top_mount: PathBuf::from(format!("{}-top", a.mount_point.display())),
        mount_point: a.mount_point,
        source: a.source,
        limine_dir: a.limine_dir,
        hostname: a.hostname,
        libc: a.libc.map(|l| match l {
            LibcArg::Glibc => forge_schema::Libc::Glibc,
            LibcArg::Musl => forge_schema::Libc::Musl,
        }),
        kernel: a.kernel,
        initramfs: a.initramfs,
        extra_cmdline: a.cmdline_extra,
        efi_entry: !a.no_efi_entry,
        accounts,
        plan,
    };
    forge_strap::execute(&req, &mut forge_strap::SystemRunner, &mut TermUi)?;
    println!();
    println!("Installed. Remove the live media and reboot.");
    println!("After the first boot, run `forge apply` to converge the system to /etc/forge/system.toml.");
    Ok(())
}

struct TermUi;

impl forge_strap::Ui for TermUi {
    fn step(&mut self, n: usize, total: usize, text: &str) {
        println!("[{n}/{total}] {text}");
    }
    fn warn(&mut self, text: &str) {
        eprintln!("warning: {text}");
    }
}

fn prompt_line(prompt: &str) -> Result<String> {
    use std::io::{BufRead, Write};
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        bail!("no input (end of file)");
    }
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

fn yes_no(question: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    Ok(match prompt_line(&format!("{question} {hint} "))?.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

fn ask_password(who: &str) -> Result<String> {
    loop {
        let a = rpassword::prompt_password(format!("Password for {who}: "))?;
        let b = rpassword::prompt_password("Repeat password: ")?;
        if a != b {
            println!("passwords do not match, try again");
        } else if a.is_empty() {
            println!("the password must not be empty");
        } else {
            return Ok(a);
        }
    }
}

/// Ask for the accounts up front, so typos are caught before any disk is touched.
fn collect_accounts() -> Result<forge_strap::Accounts> {
    let mut acc = forge_strap::Accounts::default();
    println!();
    if yes_no("Create a user account?", true)? {
        loop {
            let name = prompt_line("Username: ")?;
            let candidate = forge_strap::Accounts {
                user: Some(forge_strap::NewUser { name: name.trim().to_string(), password: "x".into() }),
                root_password: None,
            };
            match candidate.validate() {
                Ok(()) => {
                    let name = name.trim().to_string();
                    let password = ask_password(&name)?;
                    acc.user = Some(forge_strap::NewUser { name, password });
                    break;
                }
                Err(_) => println!("use lowercase letters, digits, `-` and `_`, starting with a letter or `_`"),
            }
        }
    }
    if yes_no("Change the root password from the live default?", true)? {
        acc.root_password = Some(ask_password("root")?);
    }
    Ok(acc)
}

/// Read the system declaration; a missing one means this is not an installed system (the live ISO has none).
fn read_system_decl(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "there is no system declaration at {}: this is probably the live system, which has none. Install first with `forge strap ROOT BOOT`; `forge install` and `forge upgrade` manage an installed system",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:").map(|r| r.split_whitespace().next() == Some("0")))
        })
        .unwrap_or(false)
}

/// Bring every declared package to the newest version the repositories offer (what `apply` resolves anyway).
fn upgrade(a: UpgradeArgs) -> Result<()> {
    let path = a.root.join("etc/forge/system.toml");
    let text = read_system_decl(&path)?;
    let decl = SystemDecl::parse(&text).with_context(|| format!("in {}", path.display()))?;
    if decl.packages.is_empty() && base_packages().is_empty() {
        ui::info("no packages are installed through forge yet (see: forge install NAME); nothing to upgrade");
        return Ok(());
    }
    apply(ApplyArgs {
        category: Category::System,
        file: None,
        root: a.root,
        rebuild: false,
        dry_run: a.dry_run,
        yes: a.yes,
        no_generation: false,
        keys: a.keys,
        allow_unsigned: a.allow_unsigned,
        upgrade: true,
        purge: false,
        adopt: a.adopt,
    })
}

fn list_cmd(a: ListArgs) -> Result<()> {
    let path = a.root.join("etc/forge/system.toml");
    let text = read_system_decl(&path)?;
    let decl = SystemDecl::parse(&text).with_context(|| format!("in {}", path.display()))?;
    let profile = forge_store::Profile::new(a.root.join("forge/profiles/system"));
    let installed = profile.current_packages().unwrap_or_default();
    // What you chose; the base system is not listed one by one (it comes with forge).
    let chosen: Vec<&String> = decl.packages.iter().filter(|n| !is_base_name(n)).collect();
    let base: Vec<_> = installed.iter().filter(|id| is_base_name(id.name())).collect();
    let others: Vec<_> = installed.iter().filter(|id| !is_base_name(id.name()) && !chosen.iter().any(|n| n.as_str() == id.name())).collect();
    let width = installed.iter().map(|id| id.name().len()).chain(chosen.iter().map(|n| n.len())).max().unwrap_or(0);
    let u = ui::ui();
    ui::heading("Installed with forge");
    if chosen.is_empty() {
        println!("  (none; try: forge install NAME)");
    }
    for name in chosen {
        match installed.iter().find(|id| id.name() == name.as_str()) {
            Some(id) => println!("  {} {name:<width$}  {}", u.on(), id.version()),
            None => println!("  {} {name:<width$}  {}", u.off(), u.dim("declared, not applied yet: run forge upgrade")),
        }
    }
    if !others.is_empty() {
        ui::heading(&format!("Pulled in as dependencies ({})", others.len()));
        for id in others {
            println!("  {} {:<width$}  {}", u.dot(), id.name(), u.dim(id.version()));
        }
    }
    println!();
    ui::dim(&format!("plus the base system: {} packages, kept installed and upgraded by forge", base.len()));
    Ok(())
}

fn search_cmd(a: SearchArgs) -> Result<()> {
    let path = a.root.join("etc/forge/system.toml");
    let text = std::fs::read_to_string(&path).ok();
    let decl = text.as_deref().and_then(|t| SystemDecl::parse(t).ok());
    let repos = match &decl {
        Some(d) if !d.repos.is_empty() => d.repos.clone(),
        _ => vec![forge_schema::DEFAULT_REPO_URL.to_string()],
    };
    let libc = decl.as_ref().map(|d| d.libc).unwrap_or(forge_schema::Libc::Glibc);
    let trust = load_trust(a.keys, a.allow_unsigned, a.root.join("etc/forge/keys"))?;
    let fetch = forge_store::SystemFetch;
    let term = a.term.as_deref().unwrap_or("").to_lowercase();
    let mut newest: std::collections::BTreeMap<String, String> = Default::default();
    for r in &repos {
        // A local repository path is inside the root being looked at (like in apply).
        let spec = if forge_store::is_url(r) { r.clone() } else { a.root.join(r.trim_start_matches('/')).display().to_string() };
        let loaded = forge_store::Repository::parse(&spec).load(&fetch, &trust)?;
        for e in loaded.index.packages.iter().filter(|e| e.libc == libc && e.name.to_lowercase().contains(&term)) {
            let better = newest.get(&e.name).is_none_or(|v| forge_store::version_cmp(&e.version, v).is_gt());
            if better {
                newest.insert(e.name.clone(), e.version.clone());
            }
        }
    }
    if newest.is_empty() {
        println!("no package matches `{}`", a.term.unwrap_or_default());
        return Ok(());
    }
    let installed = forge_store::Profile::new(a.root.join("forge/profiles/system")).current_packages().unwrap_or_default();
    let u = ui::ui();
    let w = newest.keys().map(|n| n.len()).max().unwrap_or(0).max(4);
    println!("{}", u.dim(&format!("  {:<w$}  VERSION", "NAME")));
    let total = newest.len();
    for (name, version) in newest {
        let mark = match installed.iter().find(|id| id.name() == name) {
            Some(id) if id.version() == version => format!("{} {}", u.on(), u.dim("installed")),
            Some(id) => format!("{} {}", u.off(), u.dim(&format!("installed {}", id.version()))),
            None => String::new(),
        };
        println!("  {}", format!("{name:<w$}  {version:<16} {mark}").trim_end());
    }
    println!("{}", u.dim(&format!("{total} package{}", if total == 1 { "" } else { "s" })));
    Ok(())
}

fn apply(a: ApplyArgs) -> Result<()> {
    if matches!(a.category, Category::User) {
        bail!("`forge apply user` is not implemented yet");
    }
    if a.root == Path::new("/") && !a.dry_run && !is_root() {
        bail!("applying to / needs root (use --dry-run to preview, or --root DIR to test)");
    }
    let path = a.file.unwrap_or_else(|| a.root.join("etc/forge/system.toml"));
    let text = read_system_decl(&path)?;
    let decl = SystemDecl::parse(&text).with_context(|| format!("in {}", path.display()))?;

    let opts = forge_apply::Options {
        rebuild: a.rebuild,
        trust: load_trust(a.keys, a.allow_unsigned, a.root.join("etc/forge/keys"))?,
        fetch: Default::default(),
        adopt: a.adopt,
        upgrade: a.upgrade,
        purge: a.purge,
        base: base_packages(),
    };
    let plan = forge_apply::plan_system(&decl, &a.root, opts)?;

    if !plan.pending.is_empty() {
        ui::warn("not applied by this version of forge:");
        for p in &plan.pending {
            println!("  [ ] {}: {}", p.section, p.summary);
        }
    }
    if plan.is_empty() {
        ui::ok(&format!("nothing to do: the system already matches {}", path.display()));
        return Ok(());
    }
    let mut requested = decl.packages.clone();
    requested.extend(forge_schema::init_packages(&decl.init.system).iter().map(|s| s.to_string()));
    ui::print_plan(&plan, &requested, &base_packages());
    if a.dry_run {
        println!();
        ui::info("dry run: nothing was changed");
        return Ok(());
    }
    println!();
    if !a.yes && !ui::confirm("apply these changes?")? {
        println!("aborted: nothing was changed");
        return Ok(());
    }
    let started = std::time::Instant::now();
    ui::apply_plan(&plan)?;
    println!();
    ui::ok(&format!("applied {} change(s) in {}", plan.changes.len(), ui::duration(started.elapsed())));
    // The machine must still start its init system: a link that resolves to nothing, or to BusyBox's own init, boots
    // into "can't run /etc/init.d/rcS". (Run `forge apply` again with the new forge if this fires after an upgrade.)
    let init_link = a.root.join("usr/sbin/init");
    if let Ok(target) = std::fs::read_link(&init_link) {
        let resolved = init_link.parent().map(|d| d.join(&target));
        if target.to_string_lossy().ends_with("busybox") || resolved.is_some_and(|p| !p.exists()) {
            ui::warn("/usr/sbin/init does not point at an init system: run `sudo forge apply` again before rebooting");
        }
    }
    if a.no_generation {
        ui::info("no generation was created (--no-generation)");
    } else if a.root != Path::new("/") {
        ui::info("no generation was created (--root is not the real system)");
    } else {
        let g = with_gen_ctx(&GenPaths::defaults(), true, |ctx| {
            Ok(forge_gen::create(ctx, "forge apply system")?)
        })
        .context("the changes were applied, but creating the generation failed")?;
        ui::ok(&format!("created generation {} (boot it from the Limine menu to roll back to this state)", g.id));
    }
    if plan.changes_init() {
        println!();
        ui::next(&format!("the init system is now `{}`: reboot to start it", decl.init.system));
        ui::dim("the old init system stays installed until then, so reboot and shutdown keep working; after the reboot run `sudo forge apply` once more and it is removed");
        ui::dim("(the previous generation still boots the old one from the Limine menu)");
    }
    Ok(())
}

fn with_gen_ctx<R>(
    p: &GenPaths,
    need_boot: bool,
    f: impl FnOnce(&forge_gen::Ctx) -> Result<R>,
) -> Result<R> {
    let raw = match &p.cmdline {
        Some(c) => c.clone(),
        None => std::fs::read_to_string("/proc/cmdline").unwrap_or_default(),
    };
    let boot = if need_boot {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let kernel = p.kernel.clone().unwrap_or_else(|| format!("vmlinuz-{release}"));
        // The initramfs is optional (a kernel with its root drivers built in needs none).
        let initramfs = match &p.initramfs {
            Some(i) => i.clone(),
            None => {
                let default = format!("initramfs-{release}.img");
                if p.boot_dir.join(&default).exists() { default } else { String::new() }
            }
        };
        for f in [&kernel, &initramfs] {
            if !f.is_empty() && !p.boot_dir.join(f).exists() {
                bail!(
                    "{} not found in {} (use --kernel / --initramfs / --boot-dir)",
                    f,
                    p.boot_dir.display()
                );
            }
        }
        forge_gen::BootInfo { kernel, initramfs, cmdline: forge_gen::base_cmdline(&raw) }
    } else {
        forge_gen::BootInfo { kernel: "-".into(), initramfs: "-".into(), cmdline: String::new() }
    };
    let backend: Box<dyn forge_gen::Backend> = if p.copy_backend {
        Box::new(forge_gen::CopyBackend)
    } else {
        Box::new(forge_gen::BtrfsCli)
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ctx = forge_gen::Ctx {
        backend: backend.as_ref(),
        live: p.live.clone(),
        snaps: p.snapshots.clone(),
        top: p.top.clone(),
        limine_conf: p.limine_conf.clone(),
        booted_subvol: forge_gen::booted_subvol(&raw),
        boot,
        now,
    };
    f(&ctx)
}


/// The device the root file system is mounted from, out of `/proc/self/mountinfo`.
fn root_device(mountinfo: &str) -> Option<String> {
    mountinfo
        .lines()
        .filter_map(|l| {
            let (before, after) = l.split_once(" - ")?;
            (before.split_whitespace().nth(4) == Some("/")).then(|| after.split_whitespace().nth(1).map(String::from))?
        })
        .last()
}

#[cfg(test)]
mod top_mount_tests {
    use super::root_device;

    #[test]
    fn the_root_device_is_found_in_mountinfo() {
        let info = "22 1 0:21 /@ / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,ssd,subvol=/@\n\
                    30 22 0:5 / /dev rw - devtmpfs devtmpfs rw\n\
                    31 22 0:26 /@home /home rw - btrfs /dev/nvme0n1p2 rw,subvol=/@home\n";
        assert_eq!(root_device(info).as_deref(), Some("/dev/nvme0n1p2"));
        assert_eq!(root_device("30 22 0:5 / /dev rw - devtmpfs devtmpfs rw\n"), None);
    }
}


/// The btrfs top-level volume (subvolid=5) mounted on a private directory for the length of a command: rolling back
/// replaces the root subvolume, which can only be done from the top. Unmounted again when dropped.
struct TopMount {
    dir: PathBuf,
}

impl TopMount {
    fn mount() -> Result<Self> {
        // The device the running root is on: the source field of the mountinfo line whose mount point is `/`.
        let info = std::fs::read_to_string("/proc/self/mountinfo").context("reading /proc/self/mountinfo")?;
        let device = root_device(&info).context("could not find the device the system is mounted from")?;
        let dir = PathBuf::from("/run/forge-top");
        std::fs::create_dir_all(&dir).context("creating /run/forge-top")?;
        let out = std::process::Command::new("mount").args(["-o", "subvolid=5", &device]).arg(&dir).output().context("running mount")?;
        if !out.status.success() {
            bail!("mounting the top-level volume of {device} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(Self { dir })
    }
}

impl Drop for TopMount {
    fn drop(&mut self) {
        let _ = std::process::Command::new("umount").arg(&self.dir).status();
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn gen_cmd(a: GenArgs) -> Result<()> {
    let read_only = matches!(a.action, GenAction::List | GenAction::Current);
    if !read_only && !a.paths.copy_backend && !is_root() {
        bail!("changing generations needs root");
    }
    // Rolling back works on the top-level volume: when none was given, mount it for this command and unmount it after.
    let needs_top = matches!(a.action, GenAction::Rollback { .. } | GenAction::CleanupNext | GenAction::RewriteLimine);
    let mut paths = a.paths.clone();
    let _top_mount = if needs_top && paths.top.is_none() && !paths.copy_backend {
        let m = TopMount::mount()?;
        paths.top = Some(m.dir.clone());
        Some(m)
    } else {
        None
    };
    with_gen_ctx(&paths, !read_only, |ctx| match a.action {
        GenAction::Create { description } => {
            let g = forge_gen::create(ctx, &description.join(" "))?;
            println!("created generation {}: {}", g.id, g.description);
            Ok(())
        }
        GenAction::List => {
            let cur = forge_gen::current(ctx)?;
            let gens = forge_gen::list(ctx)?;
            if gens.is_empty() {
                println!("no generations");
            }
            let u = ui::ui();
            if !gens.is_empty() {
                println!("{}", u.dim("    ID  CREATED              DESCRIPTION  [KERNEL]"));
            }
            for g in gens {
                let (mark, id) = if cur == Some(g.id) { (u.on(), u.bold(&format!("{:>3}", g.id))) } else { (" ".to_string(), format!("{:>3}", g.id)) };
                println!("{mark} {id}  {}  {}  {}", forge_gen::format_utc(g.created), g.description, u.dim(&format!("[{}]", g.kernel)));
            }
            if cur.is_some() {
                println!("{}", u.dim(&format!("{} = the generation you booted", u.on())));
            }
            Ok(())
        }
        GenAction::Current => {
            match forge_gen::current(ctx)? {
                Some(n) => println!("current generation: {n}"),
                None => println!("current generation: none"),
            }
            match &ctx.booted_subvol {
                Some(s) => println!("booted subvolume:   {s}"),
                None => println!("booted subvolume:   unknown"),
            }
            Ok(())
        }
        GenAction::Remove { target } => {
            let r = if target == "all" {
                forge_gen::remove_all(ctx)?
            } else {
                let id: u32 = target.parse().context("expected a generation number or `all`")?;
                forge_gen::remove(ctx, id)?
            };
            for id in &r.removed {
                println!("removed generation {id}");
            }
            for (id, why) in &r.skipped {
                println!("kept generation {id}: it is {why}");
            }
            Ok(())
        }
        GenAction::Prune { keep } => {
            let r = forge_gen::prune(ctx, keep)?;
            for id in &r.removed {
                println!("removed generation {id}");
            }
            for (id, why) in &r.skipped {
                println!("kept generation {id}: it is {why}");
            }
            if r.removed.is_empty() {
                println!("nothing to prune (keeping the newest {keep})");
            }
            Ok(())
        }
        GenAction::Rollback { id } => {
            match forge_gen::rollback(ctx, id)? {
                forge_gen::RollbackOutcome::Staged => println!(
                    "rollback to generation {id} is staged; reboot to finish it, then run \
                     `forge gen rewrite-limine` and, after one more reboot, `forge gen cleanup-next`"
                ),
                forge_gen::RollbackOutcome::Replaced => {
                    println!("@ was replaced with generation {id}; reboot into the normal entry")
                }
            }
            Ok(())
        }
        GenAction::CleanupNext => {
            forge_gen::cleanup_next(ctx)?;
            println!("removed @.next");
            Ok(())
        }
        GenAction::RewriteLimine => {
            let p = forge_gen::promote(ctx)?;
            forge_gen::rewrite_limine(ctx)?;
            println!("wrote {}", ctx.limine_conf.display());
            if p == forge_gen::Promotion::WaitingForReboot {
                println!("a rollback is staged: reboot into it to finish");
            }
            Ok(())
        }
    })
}

#[derive(Args)]
struct MaintenanceArgs {
    #[command(subcommand)]
    action: MaintenanceAction,
}

#[derive(Subcommand)]
enum MaintenanceAction {
    /// Run the routine tasks now (all of them, or the ones named: trim, prune, gc, scrub)
    Run {
        /// Only what is overdue (this is what the maintenance service runs every hour)
        #[arg(long)]
        due: bool,
        tasks: Vec<String>,
    },
    /// Show when each task last ran and when it is next due
    Status,
}

fn maintenance_cmd(a: MaintenanceArgs) -> Result<()> {
    let state = Path::new(maintenance::STATE);
    match a.action {
        MaintenanceAction::Status => {
            maintenance::status(state);
            Ok(())
        }
        MaintenanceAction::Run { due, tasks } => {
            if !is_root() {
                bail!("maintenance needs root: sudo forge maintenance run");
            }
            let keep = std::fs::read_to_string("/etc/forge/system.toml")
                .ok()
                .and_then(|t| SystemDecl::parse(&t).ok())
                .map_or(10, |d| d.maintenance.keep_generations);
            maintenance::run(due, &tasks, keep, state)
        }
    }
}

#[derive(Args)]
struct SecurebootArgs {
    #[command(subcommand)]
    action: SecurebootAction,
}

#[derive(Subcommand)]
enum SecurebootAction {
    /// What is set up and what the firmware says
    Status,
    /// Make a key, sign the boot loader, pin the kernel and menu by hash, and write the files for the firmware
    Setup,
    /// Tell the firmware to trust the key (only possible while the firmware is in setup mode)
    Enroll,
    /// Sign the boot loader again and pin the boot files again (done by forge itself after kernel and package changes)
    Refresh,
    /// Go back to an unsigned boot loader and a menu without hashes (turn Secure Boot off in the firmware first)
    Disable,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Rewrite limine.conf (which, with Secure Boot set up, pins the files and signs the loader).
fn rewrite_menu() -> Result<()> {
    with_gen_ctx(&GenPaths::defaults(), true, |ctx| {
        forge_gen::rewrite_limine(ctx)?;
        Ok(())
    })
}

fn secureboot_cmd(a: SecurebootArgs) -> Result<()> {
    let setup = forge_secureboot::BootSetup::default();
    if let SecurebootAction::Status = a.action {
        let s = setup.status();
        let yn = |b: bool| if b { "yes" } else { "no" };
        let fw = |v: Option<bool>, on: &str, off: &str| match v {
            Some(true) => on.to_string(),
            Some(false) => off.to_string(),
            None => "unknown".to_string(),
        };
        ui::heading("Secure Boot");
        if !is_root() {
            ui::info("not running as root: /boot and the key are hidden from you, so lines below may say no; run it with sudo");
        }
        println!("  started through UEFI:        {}", yn(s.uefi));
        println!("  signing key made by forge:   {}", yn(s.key));
        println!("  set up (menu pinned):        {}", yn(s.enabled));
        println!("  boot loader signed by it:    {}", yn(s.loader_signed));
        println!("  firmware in setup mode:      {}", fw(s.setup_mode, "yes (it accepts a new key: forge secureboot enroll)", "no"));
        println!("  Secure Boot active now:      {}", fw(s.secure_boot, "yes", "no"));
        if !s.uefi {
            ui::info("this machine started in BIOS mode: Secure Boot needs UEFI");
        } else if !s.key {
            ui::next("sudo forge secureboot setup");
        } else if s.secure_boot != Some(true) {
            ui::next("sudo forge secureboot enroll  (or enroll the files in /boot/EFI/forge-secureboot from the firmware menu), then switch Secure Boot on");
        }
        return Ok(());
    }
    if !is_root() {
        bail!("this needs root: sudo forge secureboot ...");
    }
    if !setup.efivars.is_dir() && !matches!(a.action, SecurebootAction::Refresh | SecurebootAction::Disable) {
        bail!("{}", forge_secureboot::Error::NoEfi);
    }
    match a.action {
        SecurebootAction::Status => unreachable!(),
        SecurebootAction::Setup => {
            let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
            let (_, made) = setup.ensure_identity(host.trim())?;
            if made {
                ui::ok(&format!("made the signing key in {} (sb.key is secret: back it up, and never share it)", setup.dir.display()));
            }
            std::fs::write(setup.marker(), "Secure Boot is set up: forge pins the boot files and signs the loader.\n")?;
            rewrite_menu().context("could not write the pinned menu and the signed boot loader")?;
            ui::ok("the boot menu now names every file with its hash, and the boot loader is signed");
            let dir = setup.export(unix_now())?;
            ui::ok(&format!("wrote the files for the firmware menu to {}", dir.display()));
            println!();
            ui::info("Reboot once and check that the machine starts (Secure Boot can still be off: the signed loader works either way).");
            ui::next("then `sudo forge secureboot enroll` while the firmware is in setup mode, and switch Secure Boot on");
        }
        SecurebootAction::Enroll => {
            if !setup.is_enabled() {
                bail!("run `sudo forge secureboot setup` first");
            }
            setup.enroll(unix_now())?;
            ui::ok("the firmware now trusts the key (database, key-exchange key and platform key)");
            ui::next("reboot, open the firmware settings and make sure Secure Boot is on; then `forge secureboot status` should say it is active");
        }
        SecurebootAction::Refresh => {
            if !setup.is_enabled() {
                bail!("Secure Boot is not set up (sudo forge secureboot setup)");
            }
            rewrite_menu()?;
            ui::ok("boot files pinned again and the boot loader signed again");
        }
        SecurebootAction::Disable => {
            let _ = std::fs::remove_file(setup.marker());
            rewrite_menu()?;
            std::fs::copy(&setup.loader_source, setup.loader()).context("restoring the unsigned boot loader")?;
            ui::ok("back to an unsigned boot loader and a menu without hashes (the key stays in the firmware until you clear it there)");
        }
    }
    Ok(())
}

fn to_libc(l: &LibcArg) -> forge_schema::Libc {
    match l {
        LibcArg::Glibc => forge_schema::Libc::Glibc,
        LibcArg::Musl => forge_schema::Libc::Musl,
    }
}

/// Every profile directory that must keep its packages alive: the system
/// profile and each account's profile, found through /etc/passwd so that no
/// home directory is missed.
fn default_profile_dirs() -> Result<Vec<PathBuf>> {
    let mut dirs = vec![PathBuf::from("/forge/profiles/system")];
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    for line in passwd.lines() {
        if let Some(home) = line.split(':').nth(5).filter(|h| h.starts_with('/') && *h != "/") {
            let dir = PathBuf::from(home).join(".local/state/forge/profile");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    Ok(dirs)
}

fn store_cmd(a: StoreArgs) -> Result<()> {
    let store = forge_store::Store::new(&a.store);
    match a.action {
        StoreAction::Pack(p) => {
            let packed = forge_store::pack_dir(
                &p.dir,
                &forge_store::PackOptions {
                    name: p.name,
                    version: p.version,
                    libc: to_libc(&p.libc),
                    hash: p.hash,
                    deps: p.deps,
                },
                &p.out,
            )?;
            println!("packed {} -> {}", packed.id, p.out.display());
            println!("archive sha256: {}", packed.archive_sha256);
        }
        StoreAction::Publish { archive, repo, sign } => {
            let signer = sign.as_deref().map(forge_store::load_secret_key).transpose()?;
            let e = forge_store::LocalRepo::new(&repo).publish(&archive, signer.as_ref())?;
            println!("published {} to {}", e.id()?, repo.display());
        }
        StoreAction::Install(i) => {
            if i.sha256.is_none() && !i.unverified {
                bail!("give --sha256 <hash> to verify the archive, or --unverified to skip the check");
            }
            let file = std::fs::File::open(&i.archive)
                .with_context(|| format!("opening {}", i.archive.display()))?;
            let id = store.install(file, i.sha256.as_deref())?;
            println!("installed {id}");
        }
        StoreAction::List => {
            let ids = store.list()?;
            if ids.is_empty() {
                println!("the store is empty");
            }
            for id in ids {
                println!("{id}");
            }
        }
        StoreAction::Gc(g) => {
            if g.profiles.is_empty() && !is_root() {
                bail!("gc must read every user's profile, which needs root (or name the roots with --profile)");
            }
            let dirs = if g.profiles.is_empty() { default_profile_dirs()? } else { g.profiles };
            let mut roots = std::collections::BTreeSet::new();
            for d in &dirs {
                // A profile that cannot be read is an error, never "empty":
                // otherwise its packages would look unused and be deleted.
                roots.extend(
                    forge_store::Profile::new(d)
                        .all_packages()
                        .with_context(|| format!("reading profile {}", d.display()))?,
                );
            }
            roots.extend(forge_store::roots_from_snapshots(&g.snapshots)?);
            let report = store.gc(&roots, g.dry_run)?;
            let verb = if g.dry_run { "would remove" } else { "removed" };
            for id in &report.removed {
                println!("{verb} {id}");
            }
            for p in &report.stale_tmp {
                println!("{verb} leftover {}", p.display());
            }
            println!("kept {} package(s), {verb} {}", report.kept.len(), report.removed.len());
        }
    }
    Ok(())
}

// ---- signing: keys, trust and repositories -----------------------------------

/// `~/.config/forge/<sub>` (or under `$XDG_CONFIG_HOME`).
fn user_dir(sub: &str) -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("forge").join(sub))
}

/// The policy for repository indexes: signed by a key in `keys` (or `default_dir`),
/// unless `allow_unsigned` explicitly turns the check off.
fn load_trust(keys: Option<PathBuf>, allow_unsigned: bool, default_dir: PathBuf) -> Result<forge_store::Trust> {
    if allow_unsigned {
        eprintln!("warning: --allow-unsigned: repository indexes are NOT being verified");
        return Ok(forge_store::Trust::AllowUnsigned);
    }
    let dir = keys.unwrap_or(default_dir);
    let ring = forge_store::Keyring::load_dir(&dir)
        .with_context(|| format!("reading the trusted keys in {}", dir.display()))?;
    if ring.is_empty() {
        eprintln!(
            "note: no trusted keys in {}; trust one with `forge key trust <file.pub> --dir {}`",
            dir.display(),
            dir.display()
        );
    }
    Ok(forge_store::Trust::Require(ring))
}

#[derive(Args)]
struct KeyArgs {
    #[command(subcommand)]
    action: KeyAction,
}

#[derive(Subcommand)]
enum KeyAction {
    /// Create a signing key pair (NAME.key is secret, NAME.pub is for sharing)
    Generate {
        name: String,
        /// Where to write the pair (default: ~/.config/forge/signing)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// List the trusted public keys in a directory
    List {
        /// The keyring (default: ~/.config/forge/keys)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Sign a file (an ISO) with a secret key: writes FILE.sig next to it
    Sign {
        file: PathBuf,
        /// The secret key (a NAME.key from `forge key generate`)
        #[arg(long)]
        key: PathBuf,
        /// Where to write the signature (default: FILE.sig)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Check a file against its signature
    Verify {
        file: PathBuf,
        /// The signature (default: FILE.sig)
        #[arg(long)]
        sig: Option<PathBuf>,
        /// Trust exactly this public key (a NAME.pub), instead of a keyring
        #[arg(long = "pub", conflicts_with = "dir")]
        public: Option<PathBuf>,
        /// The keyring (default: /etc/forge/keys, which on a SingularitySYS system holds the release key)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Stop trusting a key for good: delete it from the keyring and remember it, so it cannot come back
    Revoke {
        /// The key's 64-digit id (see `forge key list`), or its `.pub` file
        key: String,
        /// The keyring (default: ~/.config/forge/keys; the system's is /etc/forge/keys)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Trust a public key by adding it to a keyring
    Trust {
        /// The NAME.pub file to trust
        file: PathBuf,
        /// The keyring (default: ~/.config/forge/keys; the system's is /etc/forge/keys)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 and size of a file, read in blocks (an ISO is hundreds of megabytes).
fn file_digest(path: &Path) -> Result<([u8; 32], u64)> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((h.finalize().into(), total))
}

/// A key named on the command line: its 64-digit id, or the `.pub` file to read the id from.
fn key_id_arg(arg: &str) -> Result<String> {
    if forge_store::valid_key_id(arg) {
        return Ok(arg.to_string());
    }
    Ok(forge_store::load_public_key(Path::new(arg)).with_context(|| format!("`{arg}` is neither a key id (64 hex digits) nor a readable .pub file"))?.id())
}

fn key_cmd(a: KeyArgs) -> Result<()> {
    match a.action {
        KeyAction::Generate { name, dir } => {
            let dir = match dir {
                Some(d) => d,
                None => user_dir("signing")?,
            };
            let key = forge_store::SecretKey::generate()?;
            let (secret, public) = forge_store::write_key_pair(&dir, &name, &key)?;
            println!("key id:     {}", key.public().id());
            println!("secret key: {}  (keep this private; it signs repositories)", secret.display());
            println!("public key: {}  (share this; others trust it with `forge key trust`)", public.display());
        }
        KeyAction::List { dir } => {
            let dir = match dir {
                Some(d) => d,
                None => user_dir("keys")?,
            };
            let ring = forge_store::Keyring::load_dir(&dir)?;
            if ring.is_empty() {
                println!("no trusted keys in {}", dir.display());
            }
            for id in ring.ids() {
                println!("{id}");
            }
        }
        KeyAction::Sign { file, key, out } => {
            let secret = forge_store::load_secret_key(&key)?;
            let (digest, size) = file_digest(&file)?;
            let sig = secret.sign_file_digest(&digest);
            let out = out.unwrap_or_else(|| PathBuf::from(format!("{}.sig", file.display())));
            std::fs::write(&out, sig.render()).with_context(|| format!("writing {}", out.display()))?;
            println!("sha256:    {}  {} ({size} bytes)", hex_of(&digest), file.display());
            println!("signed by: {}", sig.key_id);
            println!("signature: {}", out.display());
        }
        KeyAction::Verify { file, sig, public, dir } => {
            let sig_path = sig.unwrap_or_else(|| PathBuf::from(format!("{}.sig", file.display())));
            let sig_text = std::fs::read_to_string(&sig_path).with_context(|| format!("reading {}", sig_path.display()))?;
            let sig = forge_store::SigFile::parse(&sig_text).map_err(|e| anyhow::anyhow!("{}: {e}", sig_path.display()))?;
            let ring = match public {
                Some(p) => {
                    let mut r = forge_store::Keyring::empty();
                    r.insert(forge_store::load_public_key(&p)?);
                    r
                }
                None => forge_store::Keyring::load_dir(&dir.unwrap_or_else(|| PathBuf::from("/etc/forge/keys")))?,
            };
            let (digest, _) = file_digest(&file)?;
            match ring.verify_file_digest(&digest, &sig) {
                Ok(id) => println!("good signature on {} by key {id}\nsha256: {}", file.display(), hex_of(&digest)),
                Err(e) => bail!("{} does NOT match its signature: {e:?}", file.display()),
            }
        }
        KeyAction::Revoke { key, dir } => {
            let dir = match dir {
                Some(d) => d,
                None => user_dir("keys")?,
            };
            let id = key_id_arg(&key)?;
            if forge_store::revoke_in_dir(&dir, &id)? {
                println!("revoked {id}");
            } else {
                println!("already revoked");
            }
            println!("keyring: {}", dir.display());
        }
        KeyAction::Trust { file, dir } => {
            let dir = match dir {
                Some(d) => d,
                None => user_dir("keys")?,
            };
            let key = forge_store::load_public_key(&file)?;
            if forge_store::revoked_ids(&dir).contains(&key.id()) {
                bail!("key {} was revoked in {}: it is not trusted again (a leaked key stays revoked)", key.id(), dir.display());
            }
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let dest = dir.join(format!("{}.pub", &key.id()[..16]));
            let text = std::fs::read_to_string(&file)?;
            match std::fs::read_to_string(&dest) {
                Ok(existing) if existing == text => println!("already trusted"),
                Ok(_) => bail!("{} already exists with different content", dest.display()),
                Err(_) => {
                    std::fs::write(&dest, text).with_context(|| format!("writing {}", dest.display()))?;
                    println!("trusted");
                }
            }
            println!("key id: {}", key.id());
            println!("keyring: {}", dir.display());
        }
    }
    Ok(())
}

#[derive(Args)]
struct RepoArgs {
    #[command(subcommand)]
    action: RepoAction,
}

#[derive(Subcommand)]
enum RepoAction {
    /// Sign a repository's index (this vouches for whatever it lists: check it first)
    Sign {
        repo: PathBuf,
        /// The secret key file to sign with
        #[arg(long)]
        key: PathBuf,
    },
    /// Build one clean repository from several: the newest version of every package, signed by KEY
    Assemble {
        /// The new repository (must not exist yet)
        dest: PathBuf,
        /// A repository to take packages from (repeatable; on a tie the earlier one wins)
        #[arg(long = "from", required = true)]
        from: Vec<PathBuf>,
        /// The secret key to sign the new repository with
        #[arg(long)]
        key: PathBuf,
        /// The new serial is at least this (the published repository's serial + 1: it must never go down)
        #[arg(long, default_value_t = 0)]
        min_serial: u64,
        /// Leave out packages whose name contains this (default: -stage, the bootstrap steps)
        #[arg(long, default_value = "-stage")]
        skip: Vec<String>,
        /// A public key (`.pub` file) to announce: systems that trust KEY will trust it too, so the repository can move to
        /// a new signing key (publish with the old key announcing the new one; sign with the new key afterwards)
        #[arg(long = "trust-key")]
        trust_key: Vec<PathBuf>,
        /// A signing key to stop trusting (its 64-digit id, or its `.pub` file): systems that trust KEY delete it from
        /// their keyring and never trust it again. Use it when a signing key has leaked: publish with a key that is still
        /// good, announcing the new one (--trust-key) and revoking the old one.
        #[arg(long = "revoke-key")]
        revoke_key: Vec<String>,
    },
    /// Check a repository's signature against the trusted keys
    Verify {
        repo: PathBuf,
        /// The keyring (default: ~/.config/forge/keys)
        #[arg(long)]
        keys: Option<PathBuf>,
    },
}

fn repo_cmd(a: RepoArgs) -> Result<()> {
    match a.action {
        RepoAction::Sign { repo, key } => {
            let signer = forge_store::load_secret_key(&key)?;
            let repo_handle = forge_store::LocalRepo::new(&repo);
            repo_handle.sign(&signer)?;
            println!("signed {} with key {}", repo.display(), signer.public().id());
        }
        RepoAction::Assemble { dest, from, key, skip, min_serial, trust_key, revoke_key } => {
            if dest.exists() {
                bail!("{} already exists; remove it or choose another name", dest.display());
            }
            let signer = forge_store::load_secret_key(&key)?;
            // The newest version of each (name, libc) across the sources; earlier sources win ties.
            let mut best: std::collections::BTreeMap<(String, String), (forge_store::IndexEntry, PathBuf)> = Default::default();
            for src in &from {
                let repo = forge_store::LocalRepo::new(src);
                let index = repo.index_unverified().with_context(|| format!("reading the index of {}", src.display()))?;
                for e in index.packages {
                    if skip.iter().any(|s| e.name.contains(s.as_str())) {
                        continue;
                    }
                    let k = (e.name.clone(), format!("{:?}", e.libc));
                    let newer = match best.get(&k) {
                        None => true,
                        Some((cur, _)) => forge_store::version_cmp(&e.version, &cur.version) == std::cmp::Ordering::Greater,
                    };
                    if newer {
                        let path = repo.archive_path(&e);
                        best.insert(k, (e, path));
                    }
                }
            }
            if best.is_empty() {
                bail!("no packages found in the given repositories");
            }
            let target = forge_store::LocalRepo::new(&dest);
            for (e, path) in best.values() {
                target.publish(path, Some(&signer)).with_context(|| format!("adding {} {}", e.name, e.version))?;
                println!("  {} {}", e.name, e.version);
            }
            let revoke_key: Vec<String> = revoke_key.iter().map(|a| key_id_arg(a)).collect::<Result<_>>()?;
            let announced: Vec<forge_store::PublicKey> = trust_key.iter().map(|f| forge_store::load_public_key(f)).collect::<Result<_, _>>()?;
            target.sign_announcing(&signer, min_serial, &announced, &revoke_key)?;
            // Run-time dependencies are exact builds: every one must have come along.
            let index = target.load(&forge_store::Trust::AllowUnsigned)?.index;
            let mut missing = Vec::new();
            for e in &index.packages {
                for d in &e.deps {
                    if forge_store::StoreId::parse(d).and_then(|id| index.by_id(&id).map(|_| ())).is_none() {
                        missing.push(format!("{} {} needs {d}", e.name, e.version));
                    }
                }
            }
            if !missing.is_empty() {
                bail!("the assembled repository is incomplete:\n  {}", missing.join("\n  "));
            }
            println!("assembled {} package(s) into {} (serial {}), signed with key {}",
                index.packages.len(), dest.display(), index.serial, signer.public().id());
        }
        RepoAction::Verify { repo, keys } => {
            let trust = load_trust(keys, false, user_dir("keys")?)?;
            let loaded = forge_store::LocalRepo::new(&repo).load(&trust)?;
            println!(
                "{}: signed by {}, serial {}, {} package(s)",
                repo.display(),
                loaded.signed_by.as_deref().unwrap_or("?"),
                loaded.index.serial,
                loaded.index.packages.len()
            );
        }
    }
    Ok(())
}

// ---- the live ISO ---------------------------------------------------------------

#[derive(Args)]
struct IsoArgs {
    /// Package repository to take the system from (repeatable)
    #[arg(long, required = true)]
    repo: Vec<PathBuf>,
    /// A picture (JPEG or PNG) for the boot menu: behind the live image's menu, and on installed systems too
    #[arg(long)]
    wallpaper: Option<PathBuf>,
    /// Packages in the live system; their dependencies come along
    #[arg(long = "package", default_values_t = default_iso_packages())]
    packages: Vec<String>,
    #[arg(long, default_value = "glibc")]
    libc: String,
    /// The ISO file to write
    #[arg(long, default_value = "singularity-live.iso")]
    out: PathBuf,
    /// Scratch directory (must not exist; kept afterwards for inspection)
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Boot this kernel instead of the newest /boot/vmlinuz-* in the system
    #[arg(long)]
    kernel: Option<PathBuf>,
    /// Where Limine's files are
    #[arg(long, default_value = "/usr/share/limine")]
    limine_dir: PathBuf,
    /// Volume label (capital letters, digits and underscores)
    #[arg(long, default_value = "SINGULARITY_LIVE")]
    label: String,
    #[arg(long, default_value = "singularity-live")]
    hostname: String,
    /// Kernel command line
    #[arg(long, default_value = "console=ttyS0,115200 console=tty0")]
    cmdline: String,
    /// The live root password
    #[arg(long, default_value = "root")]
    root_password: String,
    /// Directory of `*.pub` keys the installed system will trust for package repositories
    /// (copied to /etc/forge/keys; the project's release key is in distro/keys)
    #[arg(long, value_name = "DIR")]
    trusted_keys: Option<PathBuf>,
    /// Directory of trusted public keys (default: ~/.config/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Do not verify repository signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
}

fn iso_cmd(a: IsoArgs) -> Result<()> {
    let libc = match a.libc.as_str() {
        "musl" => forge_schema::Libc::Musl,
        "glibc" => forge_schema::Libc::Glibc,
        other => bail!("--libc must be musl or glibc, not {other}"),
    };
    let work = a.work_dir.clone().unwrap_or_else(|| PathBuf::from(format!("{}.work", a.out.display())));
    if work.exists() {
        bail!("{} already exists; remove it or pass another --work-dir", work.display());
    }
    if a.out.exists() {
        bail!("{} already exists; remove it first", a.out.display());
    }
    let trust = load_trust(a.keys, a.allow_unsigned, user_dir("keys")?)?;

    eprintln!("fetching {} (and dependencies) from {} repositor{}", a.packages.join(", "), a.repo.len(), if a.repo.len() == 1 { "y" } else { "ies" });
    let trees = forge_iso::fetch(&a.repo, &trust, &a.packages, libc, &work.join("store"))?;
    let rootfs = work.join("rootfs");
    forge_iso::materialize_into(&work.join("store"), &trees, &rootfs)?;
    // The initramfs is made of a static BusyBox: a build tool of the image, fetched on its own. The live system has none.
    let busybox_trees = forge_iso::fetch(&a.repo, &trust, &["busybox".to_string()], libc, &work.join("store"))?;
    let initramfs_busybox = busybox_trees.iter().map(|t| t.join("usr/bin/busybox")).find(|p| p.is_file()).context("the repository has no busybox package (the initramfs needs it)")?;

    let kernel = match a.kernel {
        Some(k) => k,
        None => forge_iso::kernel_in(&rootfs)
            .context("the system has no kernel in /boot (add the `linux` package, or pass --kernel)")?,
    };
    eprintln!("kernel: {}", kernel.display());
    let mut trusted_keys = Vec::new();
    if let Some(dir) = &a.trusted_keys {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "pub") {
                forge_store::load_public_key(&p).with_context(|| format!("{} is not a usable public key", p.display()))?;
                trusted_keys.push(p);
            }
        }
        if trusted_keys.is_empty() {
            bail!("no *.pub files in {}", dir.display());
        }
        trusted_keys.sort();
    }
    if let Some(w) = &a.wallpaper {
        if !w.is_file() {
            bail!("the wallpaper {} does not exist", w.display());
        }
    }
    let spec = forge_iso::IsoSpec {
        rootfs,
        initramfs_busybox,
        wallpaper: a.wallpaper.clone(),
        kernel,
        limine_dir: a.limine_dir,
        work: work.join("build"),
        out: a.out.clone(),
        label: a.label,
        hostname: a.hostname,
        cmdline: a.cmdline,
        root_password: a.root_password,
        trusted_keys,
    };
    forge_iso::build(&spec, &mut forge_iso::SystemRunner)?;
    let size = std::fs::metadata(&a.out)?.len();
    println!("wrote {} ({} MiB)", a.out.display(), size / (1024 * 1024));
    println!("scratch files are in {} (safe to delete)", work.display());
    Ok(())
}

// ---- kernels ---------------------------------------------------------------------------------------

#[derive(Args)]
struct KernelArgs {
    #[command(subcommand)]
    action: KernelAction,
}

#[derive(Subcommand)]
enum KernelAction {
    /// List the kernels kernel.org offers now
    List,
    /// Download, verify, configure and build a kernel into a staging directory
    Build(KernelBuildArgs),
    /// Install a staged kernel: modules, boot files, a generation to roll back to, and Limine
    Install(KernelInstallArgs),
    /// Install the newest kernel the package repository offers (what the live ISO boots; every driver as a module)
    Update,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Series {
    /// The newest stable release (new hardware support soonest, supported for a few months)
    Stable,
    /// The newest long-term release (fewer changes, supported for years)
    Longterm,
}

#[derive(Args)]
struct KernelBuildArgs {
    /// An exact version, e.g. 7.3.3
    #[arg(long, conflicts_with = "series")]
    version: Option<String>,
    /// Which kernel.org series to take the newest of
    #[arg(long, value_enum, default_value = "stable")]
    series: Series,
    /// Every driver the kernel has, as a module: the live ISO's kernel (about 300 MB of modules)
    #[arg(long, group = "mode")]
    full: bool,
    /// Keep only the modules loaded right now (the default). Plug in the devices you use first, or name them with --also-keep
    #[arg(long, group = "mode")]
    auto: bool,
    /// Choose in `make menuconfig`, starting from --base-config
    #[arg(long, group = "mode")]
    menuconfig: bool,
    /// Use this configuration as it is (it is only checked)
    #[arg(long, group = "mode", value_name = "FILE")]
    config: Option<PathBuf>,
    /// Starting point for --auto and --menuconfig (default: the running kernel's configuration)
    #[arg(long)]
    base_config: Option<PathBuf>,
    /// With --auto: read the loaded modules from this file (a copy of /proc/modules saved on the machine the
    /// kernel is FOR) instead of from this machine
    #[arg(long, value_name = "FILE")]
    loaded_modules: Option<PathBuf>,
    /// With --auto: keep this module even if it is not loaded now (repeatable)
    #[arg(long = "also-keep", value_name = "MODULE")]
    also_keep: Vec<String>,
    #[arg(long)]
    jobs: Option<usize>,
    /// Scratch directory (default ~/.cache/forge/kernel)
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Where to stage the result (default <work-dir>/stage-<version>)
    #[arg(long)]
    out: Option<PathBuf>,
    /// Downloaded sources are kept here (default ~/.cache/forge/sources)
    #[arg(long)]
    source_cache: Option<PathBuf>,
    /// The C compiler command
    #[arg(long, default_value = "gcc -std=gnu11")]
    cc: String,
    /// Install it when the build is done (needs root)
    #[arg(long)]
    install: bool,
    /// Build with the tools installed on this machine instead of the packages (needed for --menuconfig)
    #[arg(long)]
    host_tools: bool,
    /// A package repository to fetch the build tools from (repeatable; default: the ones in /etc/forge/system.toml)
    #[arg(long, value_name = "DIR-OR-URL")]
    repo: Vec<String>,
    /// Directory of trusted public keys (default: /etc/forge/keys)
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Use repository indexes WITHOUT checking their signatures (insecure)
    #[arg(long)]
    allow_unsigned: bool,
    /// The package store the build tools are installed into (default: /forge/store as root, else ~/.cache/forge/store)
    #[arg(long)]
    store: Option<PathBuf>,
    /// Keep the build tools this build downloaded (a cache for the next build); by default they are removed again
    #[arg(long)]
    keep_tools: bool,
    /// Keep the build directory after a failed build, for inspection
    #[arg(long)]
    keep_work: bool,
    /// Do not check for the build tools first
    #[arg(long)]
    skip_checks: bool,
}

#[derive(Args)]
struct KernelInstallArgs {
    /// The staging directory `forge kernel build` printed (default: the kernel you built last)
    staged: Option<PathBuf>,
    #[arg(long, default_value = "/boot")]
    boot_dir: PathBuf,
    /// The system root the modules go under
    #[arg(long, default_value = "/")]
    root: PathBuf,
    #[arg(long, default_value = "/boot/limine.conf")]
    limine_conf: PathBuf,
    #[arg(long, default_value = "/.snapshots")]
    snapshots: PathBuf,
    /// Do not snapshot a generation first (you could not roll back to the old kernel's system)
    #[arg(long)]
    no_generation: bool,
    /// Copy the files but leave limine.conf alone: the new kernel is not offered at boot
    #[arg(long)]
    no_limine: bool,
    /// Replace this release if it is already installed
    #[arg(long)]
    force: bool,
}

fn cache_home(sub: &str) -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".cache"),
    };
    Ok(base.join("forge").join(sub))
}

fn kernel_cmd(a: KernelArgs) -> Result<()> {
    match a.action {
        KernelAction::List => {
            let text = forge_kernel::Curl.get(forge_kernel::RELEASES_URL)?;
            for r in forge_kernel::parse_releases(&text)? {
                if r.moniker != "linux-next" {
                    println!("{:<10} {}", r.moniker, r.version);
                }
            }
            Ok(())
        }
        KernelAction::Build(b) => kernel_build(b),
        KernelAction::Install(i) => kernel_install(i),
        KernelAction::Update => kernel_update(),
    }
}

/// The running kernel's configuration: `/boot/config-<release>`, or `/proc/config.gz`.
fn running_config(work: &Path) -> Result<PathBuf> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let in_boot = PathBuf::from(format!("/boot/config-{}", release.trim()));
    if in_boot.is_file() {
        return Ok(in_boot);
    }
    if Path::new("/proc/config.gz").is_file() {
        let dst = work.join("running-config");
        std::fs::create_dir_all(work)?;
        let out = std::process::Command::new("gzip").args(["-dc", "/proc/config.gz"]).output().context("running gzip")?;
        if out.status.success() {
            std::fs::write(&dst, out.stdout)?;
            return Ok(dst);
        }
    }
    bail!("cannot find the running kernel's configuration (no /boot/config-{} and no /proc/config.gz); pass --base-config", release.trim())
}

fn kernel_build(b: KernelBuildArgs) -> Result<()> {
    if b.host_tools && !b.skip_checks {
        forge_kernel::preflight()?;
    }
    let work = match b.work_dir {
        Some(w) => w,
        None => cache_home("kernel")?,
    };
    let cache_root = match b.source_cache {
        Some(c) => c,
        None => cache_home("sources")?,
    };
    let cache = forge_build::SourceCache::new(cache_root.clone());
    let selector = match (&b.version, b.series) {
        (Some(v), _) => forge_kernel::Selector::Version(v.clone()),
        (None, Series::Stable) => forge_kernel::Selector::Stable,
        (None, Series::Longterm) => forge_kernel::Selector::Longterm,
    };
    ui::heading("Kernel build");
    let finding = ui::Spinner::start("finding the kernel on kernel.org");
    let (version, tarball, tarball_sha256) = match forge_kernel::fetch(&forge_kernel::Curl, &cache, &selector) {
        Ok(v) => v,
        Err(e) => {
            finding.fail();
            return Err(e.into());
        }
    };
    finding.finish();
    ui::ok(&format!("kernel {version} downloaded and checked against kernel.org's checksum list"));
    ui::dim(&format!("  {}", tarball.display()));

    let mode = if b.full {
        forge_kernel::Mode::Full
    } else if let Some(file) = b.config {
        forge_kernel::Mode::File(file)
    } else if b.menuconfig {
        forge_kernel::Mode::Menu { base_config: match b.base_config { Some(c) => c, None => running_config(&work)? } }
    } else {
        let loaded = match &b.loaded_modules {
            Some(f) => std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?,
            None => std::fs::read_to_string("/proc/modules").context("reading /proc/modules")?,
        };
        if loaded.trim().is_empty() {
            bail!("no modules are loaded, so there is nothing to base a trimmed kernel on; use --full or --menuconfig");
        }
        forge_kernel::Mode::Auto {
            base_config: match b.base_config {
                Some(c) => Some(c),
                None => match running_config(&work) {
                    Ok(c) => Some(c),
                    Err(_) => {
                        ui::info("the running kernel does not expose its configuration: starting from Singularity's standard kernel configuration");
                        None
                    }
                },
            },
            loaded_modules: loaded,
            keep: b.also_keep,
        }
    };
    let jobs = b.jobs.unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
    let latest = work.join("latest");
    let staged = if b.host_tools {
        // The tools installed on this machine do the work (needed for --menuconfig).
        let out = b.out.clone().unwrap_or_else(|| work.join(format!("stage-{version}")));
        if out.exists() && std::fs::read_dir(&out).map(|mut d| d.next().is_some()).unwrap_or(false) {
            bail!("{} is not empty; remove it or pass another --out", out.display());
        }
        let spec = forge_kernel::BuildSpec { version, tarball, work, out, mode, jobs, cc: b.cc };
        forge_kernel::build(&spec, &mut forge_kernel::SystemRunner)?
    } else {
        // The default: the build tools come from the packages (fetched and verified like any package) and the
        // build runs in the package sandbox, so nothing has to be installed first.
        let repos = if b.repo.is_empty() { system_repos() } else { b.repo.clone() };
        let spec = forge_kernel::StoreBuildSpec {
            version,
            tarball_sha256,
            source_cache: cache_root.clone(),
            work,
            mode,
            jobs,
            libc: forge_schema::Libc::Glibc,
            repos,
            trust: load_trust(b.keys.clone(), b.allow_unsigned, PathBuf::from("/etc/forge/keys"))?,
            fetch: Default::default(),
            store: match &b.store { Some(s) => s.clone(), None => if is_root() { PathBuf::from("/forge/store") } else { cache_home("store")? } },
            helper: std::env::current_exe().context("finding the forge executable")?,
            keep_work: b.keep_work,
            keep_tools: b.keep_tools,
        };
        ui::info(&format!("build tools: {} (fetched into {}, removed again afterwards)", forge_kernel::BUILD_PACKAGES.join(" "), spec.store.display()));
        ui::info("building the kernel: this takes a while (a trimmed kernel is much faster than --full)");
        forge_kernel::build_in_store(&spec)?
    };
    println!();
    ui::ok(&format!("built kernel {} in {}", staged.release, staged.dir.display()));
    // A short way back to it: `forge kernel install` with no path takes the kernel built last.
    let _ = std::fs::remove_file(&latest);
    let _ = std::os::unix::fs::symlink(&staged.dir, &latest);
    if b.install {
        return kernel_install(KernelInstallArgs {
            staged: Some(staged.dir),
            boot_dir: "/boot".into(),
            root: "/".into(),
            limine_conf: "/boot/limine.conf".into(),
            snapshots: "/.snapshots".into(),
            no_generation: false,
            no_limine: false,
            force: false,
        });
    }
    println!();
    ui::next("install it with:  sudo forge kernel install");
    Ok(())
}

/// The kernel `forge kernel build` finished last (its `latest` link), also when run through sudo.
fn latest_staged() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(c) = cache_home("kernel") {
        candidates.push(c.join("latest"));
    }
    if let Some(user) = std::env::var_os("SUDO_USER") {
        candidates.push(PathBuf::from("/home").join(user).join(".cache/forge/kernel/latest"));
    }
    candidates
        .into_iter()
        .find(|c| c.exists())
        .context("no kernel has been built yet: run `forge kernel build --auto` first, or give the staging directory")
}

/// `forge kernel update`: the `linux` package of the declared repositories (verified like every package) installed with
/// the same code as a kernel you built yourself: modules, boot files, a generation to roll back to, and Limine.
fn kernel_update() -> Result<()> {
    if !is_root() {
        bail!("installing a kernel needs root");
    }
    let trust = load_trust(None, false, PathBuf::from("/etc/forge/keys"))?;
    let fetch = forge_store::SystemFetch;
    let store = forge_store::Store::new("/forge/store");
    let scratch = PathBuf::from("/forge/tmp");
    std::fs::create_dir_all(&scratch).context("creating /forge/tmp")?;
    let mut best: Option<(forge_store::Repository, forge_store::IndexEntry)> = None;
    for spec in system_repos() {
        let repo = forge_store::Repository::parse(&spec);
        let loaded = repo.load(&fetch, &trust)?;
        for e in loaded.index.packages.iter().filter(|e| e.name == "linux" && e.libc == forge_schema::Libc::Glibc) {
            if best.as_ref().is_none_or(|(_, b)| forge_store::version_cmp(&e.version, &b.version).is_gt()) {
                best = Some((forge_store::Repository::parse(&spec), e.clone()));
            }
        }
    }
    let (repo, entry) = best.context("the repositories have no `linux` package")?;
    ui::heading(&format!("Kernel update: linux {}", entry.version));
    let file = repo.open_archive(&entry, &fetch, &scratch)?;
    let id = store.install(file, Some(&entry.archive_sha256))?;
    ui::ok(&format!("downloaded and verified linux {}", entry.version));
    let _ = std::fs::remove_dir_all(&scratch);
    kernel_install(KernelInstallArgs {
        staged: Some(store.path(&id)),
        boot_dir: PathBuf::from("/boot"),
        root: PathBuf::from("/"),
        limine_conf: PathBuf::from("/boot/limine.conf"),
        snapshots: PathBuf::from("/.snapshots"),
        no_generation: false,
        no_limine: false,
        // The same release number can be a newer build of the same kernel: replace it.
        force: true,
    })
}

fn kernel_install(i: KernelInstallArgs) -> Result<()> {
    if !is_root() {
        bail!("installing a kernel needs root");
    }
    let staged = match &i.staged {
        Some(s) => s.clone(),
        None => latest_staged()?,
    };
    let release = forge_kernel::staged_release(&staged)?;
    ui::heading(&format!("Kernel install {release}"));
    let paths = |kernel: Option<String>, initramfs: Option<String>| GenPaths {
        snapshots: i.snapshots.clone(),
        limine_conf: i.limine_conf.clone(),
        boot_dir: i.boot_dir.clone(),
        kernel,
        initramfs,
        ..GenPaths::defaults()
    };

    // A generation of the system as it is now, with the kernel it boots now: the way back.
    if !i.no_generation && !i.no_limine {
        with_gen_ctx(&paths(None, None), true, |ctx| {
            let g = forge_gen::create(ctx, &format!("before kernel {release}"))?;
            ui::ok(&format!("created generation {} to roll back to (pick it in the Limine menu if this kernel misbehaves)", g.id));
            Ok(())
        })
        .context("could not snapshot a generation first (not a Singularity btrfs system? use --no-generation to go on without one)")?;
    }

    let installed = forge_kernel::install_files(&staged,&i.boot_dir, &i.root, i.force)?;
    ui::ok(&format!("installed {} and its modules", installed.kernel_file));

    if i.no_limine {
        ui::info("limine.conf was left alone: the new kernel is not offered at boot yet");
        return Ok(());
    }
    // No initramfs: these kernels carry the drivers needed to reach the root.
    with_gen_ctx(&paths(Some(installed.kernel_file.clone()), Some(String::new())), true, |ctx| {
        forge_gen::rewrite_limine(ctx)?;
        Ok(())
    })?;
    ui::ok(&format!("limine.conf now boots {}; older kernels stay available under Generations", installed.kernel_file));
    println!();
    ui::next(&format!("reboot to use kernel {}", installed.release));
    Ok(())
}

/// The repositories the system declares (`repos` in /etc/forge/system.toml), or the project's own.
fn system_repos() -> Vec<String> {
    std::fs::read_to_string("/etc/forge/system.toml")
        .ok()
        .and_then(|t| SystemDecl::parse(&t).ok())
        .map(|d| d.repos)
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| vec![forge_schema::DEFAULT_REPO_URL.to_string()])
}

/// The base system's packages, as `forge apply` wants them (see `forge_schema::BASE_PACKAGES`).
/// `FORGE_BASE_PACKAGES` (a comma-separated list, possibly empty) replaces them: for tests and tiny repositories.
fn base_packages() -> Vec<String> {
    match std::env::var("FORGE_BASE_PACKAGES") {
        Ok(list) => list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        Err(_) => forge_schema::BASE_PACKAGES.iter().map(|s| s.to_string()).collect(),
    }
}

/// A package of the base system or of an init system: always installed, never removable.
fn is_base_name(name: &str) -> bool {
    base_packages().iter().any(|b| b == name) || forge_schema::INIT_SYSTEMS.iter().any(|s| forge_schema::init_packages(s).contains(&name))
}

/// What `forge iso` puts in the image by default: the kernel, the base system, and OpenRC.
fn default_iso_packages() -> Vec<String> {
    let mut v = vec!["linux".to_string()];
    v.extend(forge_schema::BASE_PACKAGES.iter().map(|s| s.to_string()));
    v.extend(forge_schema::init_packages("openrc").iter().map(|s| s.to_string()));
    v
}

#[derive(Args)]
struct DoctorArgs {
    /// Examine this directory instead of / (for testing)
    #[arg(long, default_value = "/")]
    root: PathBuf,
}

/// Exit with a failure when something that can stop the system is wrong.
fn doctor_cmd(a: DoctorArgs) -> Result<()> {
    if doctor::run(&a.root, &base_packages())? { Ok(()) } else { bail!("forge doctor found problems that can stop the system") }
}
