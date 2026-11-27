//! `forge appimage`: install AppImages (single-file applications) into forge's environment.
//!
//! The AppImage stays one `.AppImage` file: it is copied to /opt/appimages/NAME.AppImage and nothing is unpacked into the
//! system. forge adds a launcher /usr/local/bin/NAME and a menu entry (so it shows in the app launcher), and lists it in the
//! `appimages` key of /etc/forge/system.toml, so the declaration stays the single source of truth: `forge apply` brings back
//! the file (from the address it was declared with), the launcher and the menu entry when any is missing.
//!
//! An AppImage is a program somebody else built, not a forge package: it is not signed by the repository and runs with your
//! rights. forge never fetches one on its own: you name the file or the https address, forge shows its SHA-256 (and checks it
//! against `--sha256`), and asks before it goes on.
//!
//! The launcher starts it with APPIMAGE_EXTRACT_AND_RUN=1: the AppImage's own runtime then unpacks itself into /tmp for that
//! run, so no FUSE is needed (the store drops setuid, so `fusermount` could not work). Only the menu entry's `.desktop` file
//! and icon are read out of it at install time, by the AppImage's own `--appimage-extract` as the unprivileged user `nobody`.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use forge_schema::SystemDecl;
use sha2::{Digest, Sha256};

use crate::ui;

const APPS_DIR: &str = "/opt/appimages";
const BIN_DIR: &str = "/usr/local/bin";
const DESKTOP_DIR: &str = "/usr/local/share/applications";
const DECL: &str = "/etc/forge/system.toml";
/// The unprivileged user the AppImage's own runtime runs as while it hands over its menu entry and icon.
const NOBODY: u32 = 65534;
const MARKER: &str = "# made by forge appimage";

#[derive(Args)]
pub struct AppimageArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Install an AppImage: a file, or an https address (it is downloaded first)
    Install {
        /// APP.AppImage (a path) or an https:// address
        source: String,
        /// Name of the command and menu entry (default: from the file name, `Foo-1.2-x86_64.AppImage` becomes `foo`)
        #[arg(long)]
        name: Option<String>,
        /// The SHA-256 the file must have (what the publisher lists on its download page)
        #[arg(long)]
        sha256: Option<String>,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Remove installed AppImages (and their entries in the declaration)
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// List the declared AppImages
    List,
}

pub fn run(a: AppimageArgs) -> Result<()> {
    match a.action {
        Action::Install { source, name, sha256, yes } => install(&source, name.as_deref(), sha256.as_deref(), yes),
        Action::Remove { names } => {
            need_root()?;
            for n in names {
                remove(&n)?;
            }
            Ok(())
        }
        Action::List => list(),
    }
}

fn need_root() -> Result<()> {
    if !crate::is_root() {
        bail!("this changes the system: run it with sudo");
    }
    Ok(())
}

// ---- pure helpers ----------------------------------------------------------------------------------------------------

/// The AppImage type (1 or 2) a file's first bytes announce: an ELF file with `AI` and the type at offset 8.
fn appimage_type(head: &[u8]) -> Option<u8> {
    (head.len() >= 11 && head.starts_with(b"\x7fELF") && &head[8..10] == b"AI" && matches!(head[10], 1 | 2)).then(|| head[10])
}

/// A command name from a file name: `Foo_Bar-1.2.3-x86_64.AppImage` -> `foo-bar`.
fn name_from_file(file: &str) -> String {
    let stem = file.strip_suffix(".AppImage").or_else(|| file.strip_suffix(".appimage")).unwrap_or(file);
    // cut at the first "-<digit>", "-v<digit>", "-x86", "-amd64", "-linux": what follows is a version or an architecture
    let mut cut = stem.len();
    for (i, c) in stem.char_indices() {
        if matches!(c, '-' | '_') {
            let rest = &stem[i + 1..];
            let lower = rest.to_ascii_lowercase();
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                || (lower.starts_with('v') && lower[1..].starts_with(|d: char| d.is_ascii_digit()))
                || lower.starts_with("x86")
                || lower.starts_with("amd64")
                || lower.starts_with("linux")
            {
                cut = i;
                break;
            }
        }
    }
    let name: String = stem[..cut].chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    name.trim_matches('-').to_string()
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 48 && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The menu entry made from the AppImage's own: `Exec` runs our launcher, `Icon` points at the kept icon file, and the keys
/// only AppImage helper tools read are dropped.
fn rewrite_desktop(text: &str, name: &str, icon: Option<&Path>) -> String {
    let mut out = String::new();
    let mut in_entry = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_entry = t == "[Desktop Entry]";
        }
        if in_entry {
            if let Some(v) = t.strip_prefix("Exec=") {
                // keep the arguments (%U, %F...), replace the program
                let args = v.trim().split_once(char::is_whitespace).map(|(_, a)| a).unwrap_or("");
                out.push_str(&format!("Exec={name}{}{args}\n", if args.is_empty() { "" } else { " " }));
                continue;
            }
            if t.starts_with("TryExec=") || t.starts_with("X-AppImage") {
                continue;
            }
            if let (Some(icon), true) = (icon, t.starts_with("Icon=")) {
                out.push_str(&format!("Icon={}\n", icon.display()));
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// A declaration entry: `name`, or `name=URL#sha256=HEX` for one that can be fetched again.
fn entry_for(name: &str, url: Option<&str>, sha: &str) -> String {
    match url {
        Some(u) => format!("{name}={u}#sha256={sha}"),
        None => name.to_string(),
    }
}

/// `(name, Some((url, sha256)))` of a declaration entry.
fn split_entry(entry: &str) -> (&str, Option<(&str, &str)>) {
    match entry.split_once('=') {
        Some((name, rest)) => (name, rest.rsplit_once("#sha256=")),
        None => (entry, None),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn app_file(name: &str) -> PathBuf {
    Path::new(APPS_DIR).join(format!("{name}.AppImage"))
}

// ---- install ---------------------------------------------------------------------------------------------------------

fn download(url: &str, dest: &Path) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("only https:// addresses are accepted (a plain http download can be changed on the way)");
    }
    ui::info(&format!("downloading {url}"));
    let st = Command::new("curl").args(["-fL", "--proto", "=https", "--tlsv1.2", "-o"]).arg(dest).arg(url).status().context("running curl")?;
    if !st.success() {
        bail!("could not download {url}");
    }
    Ok(())
}

fn install(source: &str, name: Option<&str>, want_sha: Option<&str>, yes: bool) -> Result<()> {
    need_root()?;
    fs::create_dir_all(APPS_DIR)?;
    fs::set_permissions(APPS_DIR, fs::Permissions::from_mode(0o755))?;
    let work = PathBuf::from(APPS_DIR).join(format!(".install-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let result = install_in(source, name, want_sha, yes, &work);
    let _ = fs::remove_dir_all(&work);
    result
}

fn install_in(source: &str, name: Option<&str>, want_sha: Option<&str>, yes: bool, work: &Path) -> Result<()> {
    // 1. the file
    let is_url = source.starts_with("https://") || source.starts_with("http://");
    let (file, file_name) = if is_url {
        let fname = source.split(['?', '#']).next().unwrap_or(source).rsplit('/').next().unwrap_or("app.AppImage").to_string();
        let dest = work.join("download");
        download(source, &dest)?;
        (dest, fname)
    } else {
        let p = PathBuf::from(source);
        if !p.is_file() {
            bail!("{source} is not a file");
        }
        let fname = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        (p, fname)
    };

    // 2. is it one, and is it the one you meant
    let mut head = [0u8; 16];
    fs::File::open(&file)?.read_exact(&mut head).context("the file is too short to be an AppImage")?;
    if appimage_type(&head).is_none() {
        bail!("{file_name} is not an AppImage (no AppImage marker in its header)");
    }
    let sha = sha256_file(&file)?;
    if let Some(want) = want_sha {
        if !want.trim().eq_ignore_ascii_case(&sha) {
            bail!("SHA-256 mismatch: the file is {sha}, you gave {want}");
        }
        ui::ok(&format!("SHA-256 matches ({sha})"));
    } else {
        ui::info(&format!("SHA-256 {sha} (compare it with the publisher's page)"));
    }
    let name = match name {
        Some(n) => n.to_string(),
        None => name_from_file(&file_name),
    };
    if !valid_name(&name) {
        bail!("`{name}` is not a usable command name: give one with --name (letters, digits, - _ .)");
    }
    if !yes {
        ui::warn(&format!("{file_name} is a program from outside forge's repository: it is not signed or checked, and it runs with your rights."));
        print!("install it as `{name}`? [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            bail!("not installed");
        }
    }

    // 3. the launcher must not be somebody else's file
    let launcher = Path::new(BIN_DIR).join(&name);
    if launcher.exists() && !fs::read_to_string(&launcher).is_ok_and(|t| t.contains(MARKER)) {
        bail!("{} already exists and is not an AppImage launcher of forge's: use --name to pick another", launcher.display());
    }

    // 4. the file goes in (owned by root, runnable by everyone), then the menu entry and the launcher
    let dest = app_file(&name);
    let tmp = Path::new(APPS_DIR).join(format!(".{name}.new"));
    fs::copy(&file, &tmp)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, &dest)?;
    make_launcher_and_menu(&name)?;

    // 5. declared: `forge apply` can bring it all back
    let entry = entry_for(&name, is_url.then_some(source.split('#').next().unwrap_or(source)), &sha);
    edit_decl(|t| forge_decl::add_appimage(t, &entry))?;

    ui::ok(&format!("{name} installed and declared: run `{name}` (or find it in the launcher)"));
    Ok(())
}

/// The command and the menu entry for the AppImage file already in place.
fn make_launcher_and_menu(name: &str) -> Result<()> {
    let file = app_file(name);
    fs::create_dir_all(BIN_DIR)?;
    let launcher = Path::new(BIN_DIR).join(name);
    fs::write(&launcher, format!("#!/bin/sh\n{MARKER}\n# no FUSE here: the AppImage unpacks itself for each run\nAPPIMAGE_EXTRACT_AND_RUN=1\nexport APPIMAGE_EXTRACT_AND_RUN\nexec \"{}\" \"$@\"\n", file.display()))?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))?;
    if let Err(e) = make_menu_entry(name, &file) {
        ui::warn(&format!("no menu entry for {name}: {e:#} (the command works)"));
    }
    Ok(())
}

/// Read the AppImage's `.desktop` file and icon out of it (its own `--appimage-extract`, as `nobody`) and write the menu entry.
fn make_menu_entry(name: &str, file: &Path) -> Result<()> {
    let scratch = Path::new(APPS_DIR).join(format!(".menu-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let result = (|| -> Result<()> {
        let copy = scratch.join("app.AppImage");
        fs::copy(file, &copy)?;
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o755))?;
        chown_tree(&scratch, NOBODY, NOBODY)?;
        let extract = |pattern: &str| {
            let _ = Command::new(&copy)
                .args(["--appimage-extract", pattern])
                .current_dir(&scratch)
                .uid(NOBODY)
                .gid(NOBODY)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", "/nonexistent")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        extract("*.desktop");
        let root = scratch.join("squashfs-root");
        let Some(desktop) = fs::read_dir(&root).ok().and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "desktop"))) else {
            bail!("the AppImage holds no .desktop file");
        };
        let text = fs::read_to_string(&desktop)?;
        let icon_name = text.lines().find_map(|l| l.trim().strip_prefix("Icon=")).map(str::trim).unwrap_or("");
        let mut kept: Option<PathBuf> = None;
        if !icon_name.is_empty() && !icon_name.contains('/') {
            for ext in ["png", "svg", "xpm"] {
                extract(&format!("{icon_name}.{ext}"));
            }
            extract(".DirIcon");
            for cand in ["png", "svg", "xpm"].iter().map(|e| root.join(format!("{icon_name}.{e}"))).chain(std::iter::once(root.join(".DirIcon"))) {
                if cand.is_file() {
                    let ext = cand.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
                    let dest = Path::new(APPS_DIR).join(format!("{name}.{ext}"));
                    fs::copy(&cand, &dest)?;
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))?;
                    kept = Some(dest);
                    break;
                }
            }
        }
        fs::create_dir_all(DESKTOP_DIR)?;
        let out = Path::new(DESKTOP_DIR).join(format!("appimage-{name}.desktop"));
        fs::write(&out, rewrite_desktop(&text, name, kept.as_deref()))?;
        fs::set_permissions(&out, fs::Permissions::from_mode(0o644))?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}

/// Hand a tree to `uid:gid` (no recursion through links).
fn chown_tree(dir: &Path, uid: u32, gid: u32) -> Result<()> {
    std::os::unix::fs::lchown(dir, Some(uid), Some(gid))?;
    for e in fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if e.file_type()?.is_dir() {
            chown_tree(&p, uid, gid)?;
        } else {
            std::os::unix::fs::lchown(&p, Some(uid), Some(gid))?;
        }
    }
    Ok(())
}

// ---- the declaration ---------------------------------------------------------------------------------------------------

fn declared() -> Vec<String> {
    fs::read_to_string(DECL).ok().and_then(|t| SystemDecl::parse(&t).ok()).map(|d| d.appimages).unwrap_or_default()
}

/// Change the `appimages` list in the system declaration (comments and layout stay), checking the result still parses.
fn edit_decl(edit: impl FnOnce(&str) -> Result<forge_decl::Edited, forge_decl::EditError>) -> Result<()> {
    let text = fs::read_to_string(DECL).with_context(|| format!("reading {DECL}"))?;
    let edited = edit(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !edited.changed {
        return Ok(());
    }
    SystemDecl::parse(&edited.text).context("the edited declaration does not parse")?;
    let tmp = format!("{DECL}.new");
    fs::write(&tmp, &edited.text).with_context(|| format!("writing {tmp}"))?;
    fs::rename(&tmp, DECL).with_context(|| format!("replacing {DECL}"))?;
    Ok(())
}

/// `forge apply`: bring back the declared AppImages whose file, launcher or menu entry is missing. A missing file is fetched
/// again from the address in its entry (and must match the checksum there); one declared without an address cannot be, and
/// is reported. Best effort: a failure is a warning, never an error of the apply.
pub fn sync_declared() {
    for entry in declared() {
        let (name, source) = split_entry(&entry);
        if !valid_name(name) {
            continue;
        }
        let file = app_file(name);
        if !file.is_file() {
            let Some((url, sha)) = source else {
                ui::warn(&format!("the AppImage {name} is declared but its file {} is gone (install it again: `sudo forge appimage install FILE`)", file.display()));
                continue;
            };
            let _ = fs::create_dir_all(APPS_DIR);
            let tmp = Path::new(APPS_DIR).join(format!(".{name}.fetch"));
            let res = download(url, &tmp).and_then(|()| {
                let got = sha256_file(&tmp)?;
                if !got.eq_ignore_ascii_case(sha) {
                    bail!("its SHA-256 is {got}, the declaration says {sha}");
                }
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
                fs::rename(&tmp, &file)?;
                Ok(())
            });
            if let Err(e) = res {
                let _ = fs::remove_file(&tmp);
                ui::warn(&format!("the AppImage {name} was not fetched: {e:#} (forge apply tries again next time)"));
                continue;
            }
        }
        let launcher_ok = fs::read_to_string(Path::new(BIN_DIR).join(name)).is_ok_and(|t| t.contains(MARKER));
        let menu_ok = Path::new(DESKTOP_DIR).join(format!("appimage-{name}.desktop")).is_file();
        if !launcher_ok || !menu_ok {
            ui::info(&format!("setting up the AppImage {name}"));
            if let Err(e) = make_launcher_and_menu(name) {
                ui::warn(&format!("the AppImage {name} was not set up: {e:#}"));
            }
        }
    }
}

// ---- remove, list ------------------------------------------------------------------------------------------------------

fn remove(name: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("`{name}` is not an installed AppImage name");
    }
    let declared_here = declared().iter().any(|e| split_entry(e).0 == name);
    let file = app_file(name);
    if !declared_here && !file.is_file() {
        bail!("{name} is not an AppImage installed with forge");
    }
    let launcher = Path::new(BIN_DIR).join(name);
    if fs::read_to_string(&launcher).is_ok_and(|t| t.contains(MARKER)) {
        fs::remove_file(&launcher)?;
    }
    let _ = fs::remove_file(Path::new(DESKTOP_DIR).join(format!("appimage-{name}.desktop")));
    for ext in ["png", "svg", "xpm"] {
        let _ = fs::remove_file(Path::new(APPS_DIR).join(format!("{name}.{ext}")));
    }
    let _ = fs::remove_file(&file);
    edit_decl(|t| forge_decl::remove_appimage(t, name))?;
    ui::ok(&format!("{name} removed"));
    Ok(())
}

fn list() -> Result<()> {
    let entries = declared();
    if entries.is_empty() {
        ui::info("no AppImages declared (try `sudo forge appimage install APP.AppImage`)");
        return Ok(());
    }
    for e in entries {
        let (name, source) = split_entry(&e);
        let state = if app_file(name).is_file() { "" } else { "  (file missing: forge apply fetches it again)" };
        println!("  {name:<24} {}{state}", source.map(|(u, _)| u).unwrap_or("local file"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_name_comes_from_the_file_name() {
        assert_eq!(name_from_file("Foo-1.2.3-x86_64.AppImage"), "foo");
        assert_eq!(name_from_file("Foo_Bar-v2.0-linux-amd64.AppImage"), "foo-bar");
        assert_eq!(name_from_file("obsidian-x86_64.AppImage"), "obsidian");
        assert_eq!(name_from_file("Kdenlive.AppImage"), "kdenlive");
        assert_eq!(name_from_file("Some App.AppImage"), "some-app");
        assert_eq!(name_from_file("lm-studio-0.3.4.AppImage"), "lm-studio");
    }

    #[test]
    fn names_are_safe_to_use_in_paths() {
        assert!(valid_name("foo-bar_1.2"));
        for bad in ["", "-x", ".hidden", "a/b", "../x", "a b", &"x".repeat(60)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_files_with_the_appimage_marker_count() {
        let mut head = [0u8; 16];
        head[..4].copy_from_slice(b"\x7fELF");
        assert_eq!(appimage_type(&head), None);
        head[8..11].copy_from_slice(b"AI\x02");
        assert_eq!(appimage_type(&head), Some(2));
        head[10] = 3;
        assert_eq!(appimage_type(&head), None);
        assert_eq!(appimage_type(b"#!/bin/sh"), None);
    }

    #[test]
    fn the_menu_entry_runs_the_launcher_and_loses_the_helper_keys() {
        let text = "[Desktop Entry]\nName=Foo\nExec=AppRun --no-sandbox %U\nTryExec=foo\nIcon=foo\nX-AppImage-Version=1.2\nCategories=Utility;\n\n[Desktop Action x]\nExec=AppRun --other\n";
        let out = rewrite_desktop(text, "foo", Some(Path::new("/opt/appimages/foo.png")));
        assert!(out.contains("Exec=foo --no-sandbox %U\n"), "{out}");
        assert!(out.contains("Icon=/opt/appimages/foo.png\n"));
        assert!(!out.contains("TryExec") && !out.contains("X-AppImage"));
        assert!(out.contains("Exec=AppRun --other"), "other sections stay as they are");
        assert!(out.contains("Categories=Utility;"));
    }

    #[test]
    fn declaration_entries_round_trip() {
        let sha = "b".repeat(64);
        assert_eq!(entry_for("foo", None, &sha), "foo");
        let e = entry_for("foo", Some("https://example.org/Foo.AppImage"), &sha);
        assert_eq!(split_entry(&e), ("foo", Some(("https://example.org/Foo.AppImage", sha.as_str()))));
        assert_eq!(split_entry("foo"), ("foo", None));
    }
}
