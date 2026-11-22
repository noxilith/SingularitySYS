//! Turning Debian packages into one forge package.
//!
//! Debian's layout differs from this system's in a few ways, and this module papers over them:
//! - libraries live in `usr/lib/x86_64-linux-gnu` (and `lib/x86_64-linux-gnu`); here they are flat in `usr/lib`.
//!   Files are moved, symlinks rewritten, and a tiny `deb-compat` package adds `usr/lib/x86_64-linux-gnu -> .`
//!   so paths that programs have built in (plugin directories and the like) still work.
//! - `/bin`, `/sbin` and `/lib` are merged into `/usr`.
//! - Documentation is left out.
//!
//! A file that the system already has (from a forge package, or the base system) is never replaced by Debian's
//! copy: the package then depends on the forge package that owns it. That is how Debian's libc, zlib, openssl,
//! dbus and so on stay the system's own.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use forge_store::{Kind, Profile, Store};

use crate::debfile;

/// Debian's multiarch directory name for this architecture.
const MULTIARCH: &str = "x86_64-linux-gnu";

/// Where a Debian path goes on this system; `None` for files that are left out.
pub fn map_path(p: &str) -> Option<String> {
    let p = p.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/');
    if p.is_empty() || p == "." {
        return None;
    }
    for skip in ["usr/share/doc/", "usr/share/doc-base/", "usr/share/lintian/", "usr/share/bug/", "usr/share/menu/", "var/lib/dpkg/", "usr/share/info/"] {
        if p.starts_with(skip) || format!("{p}/") == skip {
            return None;
        }
    }
    let multi_usr = format!("usr/lib/{MULTIARCH}");
    let multi = format!("lib/{MULTIARCH}");
    let m = if p == multi_usr || p == multi {
        "usr/lib".to_string()
    } else if let Some(rest) = p.strip_prefix(&format!("{multi_usr}/")) {
        format!("usr/lib/{rest}")
    } else if let Some(rest) = p.strip_prefix(&format!("{multi}/")) {
        format!("usr/lib/{rest}")
    } else if let Some(rest) = p.strip_prefix("lib64/") {
        format!("usr/lib64/{rest}")
    } else if let Some(rest) = p.strip_prefix("lib/") {
        format!("usr/lib/{rest}")
    } else if let Some(rest) = p.strip_prefix("bin/") {
        format!("usr/bin/{rest}")
    } else if let Some(rest) = p.strip_prefix("sbin/") {
        format!("usr/sbin/{rest}")
    } else if p == "lib" || p == "lib64" || p == "bin" || p == "sbin" {
        return None;
    } else {
        p.to_string()
    };
    Some(m)
}

fn normalize(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}

/// A symlink at `from` (Debian path) pointing at `target`, translated to the link's new place.
pub fn map_link(from: &str, target: &str) -> String {
    let from = from.trim_start_matches("./").trim_start_matches('/');
    if let Some(abs) = target.strip_prefix('/') {
        return match map_path(abs) {
            Some(m) => format!("/{m}"),
            None => target.to_string(),
        };
    }
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let resolved = normalize(&format!("{dir}/{target}")).join("/");
    let (Some(new_from), Some(new_target)) = (map_path(from), map_path(&resolved)) else { return target.to_string() };
    let from_dir: Vec<&str> = new_from.rsplit_once('/').map(|(d, _)| d.split('/').collect()).unwrap_or_default();
    let to: Vec<&str> = new_target.split('/').collect();
    let common = from_dir.iter().zip(&to).take_while(|(a, b)| a == b).count();
    // Keep a plain sibling name as it was.
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_dir.len() {
        parts.push("..".into());
    }
    for t in &to[common..] {
        parts.push((*t).to_string());
    }
    if parts.is_empty() { ".".into() } else { parts.join("/") }
}

// ---- what the system already has ------------------------------------------------------

/// Who owns which path among the installed forge packages: path -> (package name, store id).
pub struct Owners {
    by_path: HashMap<String, (String, String)>,
}

impl Owners {
    pub fn empty() -> Self {
        Owners { by_path: HashMap::new() }
    }

    pub fn load(root: &Path) -> Result<Self> {
        let store = Store::new(root.join("forge/store"));
        let profile = Profile::new(root.join("forge/profiles/system"));
        let mut by_path = HashMap::new();
        for id in profile.current_packages().unwrap_or_default() {
            let Ok(m) = store.manifest(&id) else { continue };
            for f in &m.files {
                if !matches!(f.kind, Kind::Dir) {
                    by_path.insert(f.path.clone(), (m.name.clone(), id.as_str().to_string()));
                }
            }
        }
        for (path, owner) in CLAIMED.lock().unwrap().iter() {
            by_path.insert(path.clone(), (owner.clone(), String::new()));
        }
        Ok(Owners { by_path })
    }

    pub fn owner(&self, path: &str) -> Option<&(String, String)> {
        self.by_path.get(path)
    }
}

/// Files the Debian packages built earlier in this run put in their trees. A run that makes several packages (`forge
/// upgrade` remaking them all) sees the same installed system for each; without this, two of them would each carry a
/// library the system has no copy of yet, and the system tree would refuse the overlap.
static CLAIMED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

pub fn claim_paths(paths: &HashSet<String>, owner: &str) {
    let mut c = CLAIMED.lock().unwrap();
    c.retain(|(_, o)| o != owner);
    c.extend(paths.iter().map(|p| (p.clone(), owner.to_string())));
}

pub struct World<'a> {
    pub root: &'a Path,
    pub owners: &'a Owners,
}

impl World<'_> {
    pub fn exists(&self, mapped: &str) -> bool {
        fs::symlink_metadata(self.root.join(mapped)).is_ok()
    }

    /// Does the system have this path from something other than the package being (re)built?
    pub fn has_foreign(&self, mapped: &str, self_name: &str) -> bool {
        match self.owners.owner(mapped) {
            Some((owner, _)) => owner != self_name,
            None => self.exists(mapped),
        }
    }

    /// Is some directory above `mapped` a link (or not a directory) on this system? Then the path is really somewhere
    /// else, and a package cannot put its own directory there.
    pub fn under_link(&self, mapped: &str, cache: &mut HashMap<String, bool>) -> bool {
        let Some((dir, _)) = mapped.rsplit_once('/') else { return false };
        if let Some(v) = cache.get(dir) {
            return *v;
        }
        let here = match fs::symlink_metadata(self.root.join(dir)) {
            Ok(m) => !m.is_dir(),
            Err(_) => false,
        };
        let v = here || self.under_link(dir, cache);
        cache.insert(dir.to_string(), v);
        v
    }
}

// ---- looking inside a library package ---------------------------------------------------

pub struct Listing {
    /// Shared-library file names (`libfoo.so.1`) the package provides.
    pub sonames: Vec<String>,
    /// Anything else than shared libraries (programs, data, plugin directories).
    pub has_other: bool,
}

pub fn list_package(deb: &Path) -> Result<Listing> {
    let mut ar = tar::Archive::new(debfile::data_reader(deb)?);
    let mut sonames = Vec::new();
    let mut has_other = false;
    for e in ar.entries()? {
        let e = e?;
        let t = e.header().entry_type();
        if !(t.is_file() || t.is_symlink() || t.is_hard_link()) {
            continue;
        }
        let path = e.path()?.to_string_lossy().into_owned();
        let Some(m) = map_path(&path) else { continue };
        let name = m.rsplit('/').next().unwrap_or("");
        if m.starts_with("usr/lib/") && m.matches('/').count() == 2 && name.contains(".so.") {
            sonames.push(name.to_string());
        } else {
            has_other = true;
        }
    }
    Ok(Listing { sonames, has_other })
}

// ---- building the tree --------------------------------------------------------------------

#[derive(Default)]
pub struct Tree {
    /// Paths written so far (a later package never overwrites an earlier one).
    pub written: HashSet<String>,
    /// Names of the forge packages whose files were used instead of Debian's.
    pub deps: BTreeSet<String>,
    pub files: usize,
    pub kept_system: usize,
    link_cache: HashMap<String, bool>,
}

fn mkparent(p: &Path) -> Result<()> {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    Ok(())
}

/// Unpack one `.deb` into `tree_dir` (mapped paths), following the rules in the module comment.
/// `self_name` is the forge package being built: files it owned in an earlier version are replaced, not kept.
pub fn unpack(deb: &Path, tree_dir: &Path, world: &World, self_name: &str, tree: &mut Tree) -> Result<()> {
    let mut ar = tar::Archive::new(debfile::data_reader(deb)?);
    for e in ar.entries().with_context(|| format!("reading {}", deb.display()))? {
        let mut e = e.with_context(|| format!("reading {}", deb.display()))?;
        let t = e.header().entry_type();
        let path = e.path()?.to_string_lossy().into_owned();
        let Some(mapped) = map_path(&path) else { continue };
        if mapped.starts_with(".forge") || mapped.split('/').any(|c| c == "..") {
            continue;
        }
        let dest = tree_dir.join(&mapped);
        // A directory of the system's that is a link (or a place a link sits at) cannot be a real directory here.
        if world.under_link(&mapped, &mut tree.link_cache) || (t.is_dir() && fs::symlink_metadata(world.root.join(&mapped)).is_ok_and(|m| !m.is_dir())) {
            if !t.is_dir() {
                tree.kept_system += 1;
            }
            continue;
        }
        if t.is_dir() {
            fs::create_dir_all(&dest)?;
            continue;
        }
        if !(t.is_file() || t.is_symlink() || t.is_hard_link()) {
            continue;
        }
        // Does the system (or an earlier package of this set) already have this path?
        if tree.written.contains(&mapped) {
            continue;
        }
        if let Some((owner, _id)) = world.owners.owner(&mapped) {
            if owner != self_name {
                tree.deps.insert(owner.clone());
                tree.kept_system += 1;
                continue;
            }
        } else if !mapped.starts_with("etc/") && world.exists(&mapped) {
            tree.kept_system += 1;
            continue;
        }
        mkparent(&dest)?;
        if t.is_symlink() {
            let target = e.link_name()?.ok_or_else(|| anyhow!("symlink {path} has no target"))?.to_string_lossy().into_owned();
            let new = map_link(&path, &target);
            let _ = fs::remove_file(&dest);
            symlink(&new, &dest).with_context(|| format!("linking {mapped}"))?;
        } else if t.is_hard_link() {
            let target = e.link_name()?.ok_or_else(|| anyhow!("hard link {path} has no target"))?.to_string_lossy().into_owned();
            let Some(tm) = map_path(&target) else { continue };
            let src = tree_dir.join(&tm);
            if fs::metadata(&src).is_ok() {
                let _ = fs::remove_file(&dest);
                fs::copy(&src, &dest).with_context(|| format!("copying {tm} to {mapped}"))?;
            } else {
                continue;
            }
        } else {
            let mut mode = e.header().mode().unwrap_or(0o644) & 0o755;
            // Debian ships its shared libraries 0644; the system's own are 0755, so these match.
            let file_name = mapped.rsplit('/').next().unwrap_or("");
            if mapped.starts_with("usr/lib/") && (file_name.ends_with(".so") || file_name.contains(".so.")) {
                mode |= 0o111;
            }
            let _ = fs::remove_file(&dest);
            let mut out = fs::File::create(&dest).with_context(|| format!("creating {mapped}"))?;
            std::io::copy(&mut e, &mut out).with_context(|| format!("unpacking {mapped}"))?;
            drop(out);
            fs::set_permissions(&dest, fs::Permissions::from_mode(mode | 0o444 & !0o022))?;
        }
        tree.written.insert(mapped);
        tree.files += 1;
    }
    Ok(())
}

/// What a freshly installed Debian package would have run in its maintainer scripts and triggers, for the
/// things that matter on a desktop: compiled caches. Each line only runs when the tool is there.
pub fn post_apply_script(tree_dir: &Path) -> Option<String> {
    let has = |p: &str| tree_dir.join(p).exists();
    let mut lines: Vec<String> = Vec::new();
    let mut tool = |guard: &str, cmd: &str| lines.push(format!("{guard} && {cmd} >/dev/null 2>&1 || true"));
    if has("usr/share/glib-2.0/schemas") {
        tool("[ -x /usr/bin/glib-compile-schemas ]", "/usr/bin/glib-compile-schemas /usr/share/glib-2.0/schemas");
    }
    if has("usr/lib/gdk-pixbuf-2.0") {
        tool("[ -x /usr/lib/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders ]", "/usr/lib/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders --update-cache");
    }
    if has("usr/lib/libgtk-3-0") {
        tool("[ -x /usr/lib/libgtk-3-0/gtk-query-immodules-3.0 ]", "/usr/lib/libgtk-3-0/gtk-query-immodules-3.0 --update-cache");
    }
    if has("usr/share/mime") || has("usr/share/mime/packages") {
        tool("[ -x /usr/bin/update-mime-database ]", "/usr/bin/update-mime-database /usr/share/mime");
    }
    if has("usr/share/applications") {
        tool("[ -x /usr/bin/update-desktop-database ]", "/usr/bin/update-desktop-database -q /usr/share/applications");
    }
    if has("usr/share/icons") {
        tool("[ -x /usr/bin/gtk-update-icon-cache ]", "for d in /usr/share/icons/*/; do [ -f \"$d/index.theme\" ] && /usr/bin/gtk-update-icon-cache -q -f -t \"$d\"; done");
    }
    if has("usr/share/fonts") {
        tool("command -v fc-cache >/dev/null 2>&1", "fc-cache -s");
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("#!/bin/sh\n# written by forge deb: what Debian's package scripts would have done\n{}\n", lines.join("\n")))
}

/// The directory tree of the `deb-compat` package (the `x86_64-linux-gnu` link).
pub fn compat_tree(dir: &Path) -> Result<()> {
    let p = dir.join("usr/lib");
    fs::create_dir_all(&p)?;
    let link = p.join(MULTIARCH);
    let _ = fs::remove_file(&link);
    symlink(".", &link)?;
    Ok(())
}

/// A readable size, for messages.
pub fn human(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KiB", n >> 10)
    }
}

pub fn check_tree_not_empty(tree: &Tree, name: &str) -> Result<()> {
    if tree.files == 0 {
        bail!("{name}: nothing to install (the system already has everything in it)");
    }
    Ok(())
}

pub fn read_all(mut r: impl Read) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    r.read_to_end(&mut v)?;
    Ok(v)
}

pub fn tree_dir_for(cache: &Path, name: &str) -> PathBuf {
    cache.join("trees").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_flattened_and_merged() {
        assert_eq!(map_path("./usr/lib/x86_64-linux-gnu/libz.so.1").as_deref(), Some("usr/lib/libz.so.1"));
        assert_eq!(map_path("lib/x86_64-linux-gnu/libm.so.6").as_deref(), Some("usr/lib/libm.so.6"));
        assert_eq!(map_path("bin/sh").as_deref(), Some("usr/bin/sh"));
        assert_eq!(map_path("usr/share/doc/foo/changelog.gz"), None);
        assert_eq!(map_path("usr/lib/foo/bar").as_deref(), Some("usr/lib/foo/bar"));
        assert_eq!(map_path("./"), None);
        assert_eq!(map_path("usr/lib/x86_64-linux-gnu").as_deref(), Some("usr/lib"));
    }

    #[test]
    fn symlinks_follow_the_move() {
        // a sibling link stays a sibling link
        assert_eq!(map_link("usr/lib/x86_64-linux-gnu/libz.so", "libz.so.1.3"), "libz.so.1.3");
        // up and across: from a subdirectory of the multiarch dir to the multiarch dir itself
        assert_eq!(map_link("usr/lib/x86_64-linux-gnu/foo/bar.so", "../libbar.so.1"), "../libbar.so.1");
        // from usr/lib to usr/lib/x86_64-linux-gnu/x
        assert_eq!(map_link("usr/lib/libx.so", "x86_64-linux-gnu/libx.so.1"), "libx.so.1");
        // absolute
        assert_eq!(map_link("usr/lib/x86_64-linux-gnu/a", "/usr/lib/x86_64-linux-gnu/b"), "/usr/lib/b");
        // from /usr/bin into /bin-moved paths
        assert_eq!(map_link("usr/bin/x", "../../bin/y"), "y");
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    #[test]
    fn a_package_built_earlier_in_the_run_owns_what_it_wrote() {
        let mut written = HashSet::new();
        written.insert("usr/lib/libclaimtest.so.1.0".to_string());
        claim_paths(&written, "deb-first");
        let owners = Owners::load(Path::new("/nonexistent-root")).unwrap();
        assert_eq!(owners.owner("usr/lib/libclaimtest.so.1.0").map(|o| o.0.as_str()), Some("deb-first"));
        // making the same package again replaces its claim
        claim_paths(&HashSet::new(), "deb-first");
        let owners = Owners::load(Path::new("/nonexistent-root")).unwrap();
        assert!(owners.owner("usr/lib/libclaimtest.so.1.0").is_none());
    }
}
