//! `forge appimage`: install AppImages (single-file applications) for the whole system.
//!
//! An AppImage is a program somebody else built, not a forge package: it is not signed by the repository and runs with
//! your rights. forge therefore never fetches one on its own; you name the file or the https address, forge shows its
//! SHA-256 (and checks it against `--sha256` when you give one), and asks before it goes on.
//!
//! No FUSE is needed (the store drops setuid, so `fusermount` would not work anyway). The AppImage is unpacked once with its
//! own `--appimage-extract`, run as the unprivileged user `nobody`, into /opt/appimages/NAME/app (owned by root). A small
//! launcher /usr/local/bin/NAME starts its `AppRun`, and its menu entry goes to /usr/local/share/applications. Neither
//! place belongs to forge's packages, so `forge upgrade` and `forge apply` leave them alone.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use sha2::{Digest, Sha256};

use crate::ui;

const APPS_DIR: &str = "/opt/appimages";
const BIN_DIR: &str = "/usr/local/bin";
const DESKTOP_DIR: &str = "/usr/local/share/applications";
/// The unprivileged user the AppImage's own runtime runs as while it unpacks itself.
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
    /// Remove installed AppImages
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// List the installed AppImages
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
    // cut at the first "-<digit>", "-x86", "-linux", "-amd64": what follows is a version or an architecture
    let mut cut = stem.len();
    let b = stem.as_bytes();
    for i in 0..b.len() {
        if matches!(b[i], b'-' | b'_') {
            let rest = &stem[i + 1..];
            let lower = rest.to_ascii_lowercase();
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit() || c == 'v' && rest[1..].starts_with(|d: char| d.is_ascii_digit()))
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

/// The menu entry made from the AppImage's own: `Exec` runs our launcher, `Icon` points into the installed copy, and the
/// keys only AppImage helper tools read are dropped.
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
            if t.starts_with("TryExec=") || t.starts_with("X-AppImage-") || t.starts_with("X-AppImage") {
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

// ---- install ---------------------------------------------------------------------------------------------------------

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
    let (file, file_name) = if source.starts_with("https://") {
        let fname = source.split('?').next().unwrap_or(source).rsplit('/').next().unwrap_or("app.AppImage").to_string();
        let dest = work.join("download");
        ui::info(&format!("downloading {source}"));
        let st = Command::new("curl").args(["-fL", "--proto", "=https", "--tlsv1.2", "-o"]).arg(&dest).arg(source).status().context("running curl")?;
        if !st.success() {
            bail!("could not download {source}");
        }
        (dest, fname)
    } else if source.starts_with("http://") {
        bail!("only https:// addresses are accepted (a plain http download can be changed on the way)");
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
    let Some(kind) = appimage_type(&head) else { bail!("{file_name} is not an AppImage (no AppImage marker in its header)") };
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

    // 3. unpack it, as nobody
    let unpack = work.join("unpack");
    fs::create_dir_all(&unpack)?;
    let copy = unpack.join("app.AppImage");
    fs::copy(&file, &copy)?;
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o755))?;
    chown_tree(&unpack, NOBODY, NOBODY)?;
    ui::info(&format!("unpacking (AppImage type {kind})"));
    let st = Command::new(&copy)
        .arg("--appimage-extract")
        .current_dir(&unpack)
        .uid(NOBODY)
        .gid(NOBODY)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .stdout(std::process::Stdio::null())
        .status()
        .context("running the AppImage's unpacker")?;
    let root = unpack.join("squashfs-root");
    if !st.success() || !root.is_dir() {
        bail!("the AppImage could not be unpacked (its own --appimage-extract failed)");
    }
    if !root.join("AppRun").exists() {
        bail!("the unpacked AppImage has no AppRun");
    }
    // root's files from here on: readable and runnable by everyone, no setuid bits
    chown_tree(&root, 0, 0)?;
    normalize_modes(&root)?;

    // 4. put it in place (an older copy of the same name is replaced)
    let dest = Path::new(APPS_DIR).join(&name);
    let old = Path::new(APPS_DIR).join(format!(".old-{name}-{}", std::process::id()));
    if dest.exists() {
        fs::rename(&dest, &old)?;
    }
    fs::create_dir_all(&dest)?;
    fs::rename(&root, dest.join("app"))?;
    let _ = fs::remove_dir_all(&old);
    fs::write(dest.join("forge-appimage.txt"), format!("name={name}\nsource={source}\nsha256={sha}\nfile={file_name}\n"))?;

    // 5. the command and the menu entry
    fs::create_dir_all(BIN_DIR)?;
    let launcher = Path::new(BIN_DIR).join(&name);
    if launcher.exists() && !fs::read_to_string(&launcher).is_ok_and(|t| t.contains(MARKER)) {
        bail!("{} already exists and is not an AppImage launcher of forge's: use --name to pick another", launcher.display());
    }
    fs::write(&launcher, format!("#!/bin/sh\n{MARKER}\nAPPDIR={d}/app\nexport APPDIR\nexec \"$APPDIR/AppRun\" \"$@\"\n", d = dest.display()))?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))?;
    install_desktop(&dest.join("app"), &name)?;

    ui::ok(&format!("{name} installed: run `{name}` (or find it in the launcher)"));
    Ok(())
}

fn install_desktop(app: &Path, name: &str) -> Result<()> {
    let Some(desktop) = fs::read_dir(app)?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "desktop")) else { return Ok(()) };
    let text = fs::read_to_string(&desktop)?;
    // the icon: the one the entry names (a file in the AppImage's top folder), else .DirIcon
    let icon_name = text.lines().find_map(|l| l.trim().strip_prefix("Icon=")).map(str::trim).unwrap_or("");
    let icon = ["png", "svg", "xpm"]
        .iter()
        .map(|ext| app.join(format!("{icon_name}.{ext}")))
        .chain(std::iter::once(app.join(icon_name)))
        .chain(std::iter::once(app.join(".DirIcon")))
        .find(|p| p.is_file());
    fs::create_dir_all(DESKTOP_DIR)?;
    let out = Path::new(DESKTOP_DIR).join(format!("appimage-{name}.desktop"));
    fs::write(&out, rewrite_desktop(&text, name, icon.as_deref()))?;
    fs::set_permissions(&out, fs::Permissions::from_mode(0o644))?;
    Ok(())
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

/// Everything readable by all, no write for others, no setuid/setgid/sticky; what was executable stays so.
fn normalize_modes(dir: &Path) -> Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        let ft = e.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let mode = fs::metadata(&p)?.mode() & 0o777;
        let new = if ft.is_dir() { 0o755 } else if mode & 0o111 != 0 { 0o755 } else { 0o644 };
        fs::set_permissions(&p, fs::Permissions::from_mode(new))?;
        if ft.is_dir() {
            normalize_modes(&p)?;
        }
    }
    Ok(())
}

// ---- remove, list ------------------------------------------------------------------------------------------------------

fn remove(name: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("`{name}` is not an installed AppImage name");
    }
    let dir = Path::new(APPS_DIR).join(name);
    if !dir.join("forge-appimage.txt").is_file() {
        bail!("{name} is not an AppImage installed with forge");
    }
    let launcher = Path::new(BIN_DIR).join(name);
    if fs::read_to_string(&launcher).is_ok_and(|t| t.contains(MARKER)) {
        fs::remove_file(&launcher)?;
    }
    let _ = fs::remove_file(Path::new(DESKTOP_DIR).join(format!("appimage-{name}.desktop")));
    fs::remove_dir_all(&dir)?;
    ui::ok(&format!("{name} removed"));
    Ok(())
}

fn list() -> Result<()> {
    let Ok(rd) = fs::read_dir(APPS_DIR) else {
        ui::info("no AppImages installed (try `sudo forge appimage install APP.AppImage`)");
        return Ok(());
    };
    let mut rows: Vec<(String, String)> = Vec::new();
    for e in rd.flatten() {
        let meta = e.path().join("forge-appimage.txt");
        if let Ok(text) = fs::read_to_string(&meta) {
            let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
            rows.push((get("name"), get("source")));
        }
    }
    rows.sort();
    if rows.is_empty() {
        ui::info("no AppImages installed (try `sudo forge appimage install APP.AppImage`)");
    }
    for (n, s) in rows {
        println!("  {n:<24} {s}");
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
        let out = rewrite_desktop(text, "foo", Some(Path::new("/opt/appimages/foo/app/foo.png")));
        assert!(out.contains("Exec=foo --no-sandbox %U\n"), "{out}");
        assert!(out.contains("Icon=/opt/appimages/foo/app/foo.png\n"));
        assert!(!out.contains("TryExec") && !out.contains("X-AppImage"));
        assert!(out.contains("Exec=AppRun --other"), "other sections stay as they are");
        assert!(out.contains("Categories=Utility;"));
    }
}
