//! Putting a set of packages into a real directory tree (the system's `/`).
//!
//! The system is an ordinary filesystem tree, not a farm of links into the store: `forge apply` copies each
//! package's files into `/usr` and friends. On btrfs a whole-file copy is a reflink (no extra space, even across
//! subvolumes), and the kernel does that for `std::fs::copy`; elsewhere it is a real copy. What this buys:
//!
//! - setuid programs work: the copy in `/usr` can carry the bit, which the store itself never does;
//! - third-party software and installers see a normal FHS system;
//! - rollback is the btrfs generation machinery (a snapshot of the root before and after).
//!
//! What it costs: an upgrade does file operations instead of flipping a link, so a state file records every
//! path forge put there. A path in that file is forge's to replace or remove; a path that is not there is the
//! user's (or another tool's) and is never overwritten (unless the caller says to adopt it).
//!
//! Replacing a file is atomic per file (copy beside it, then rename over it), so a running program keeps its
//! old inode and nothing ever sees a half-written file.
//!
//! `etc/` is configuration, not content: a package's file there is created if missing and never overwritten
//! or removed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::Kind;
use crate::merge::{Merged, merge_plan};
use crate::store::Store;
use crate::{StoreError, StoreId, io_ctx};

/// Programs that must be setuid root. The store never holds a setuid bit, so the bit is set on the copies in
/// the tree. Nothing outside this list is ever made setuid, and a symlink is refused (the bit would land on
/// whatever it points to).
pub const SETUID_FILES: &[&str] = &["usr/bin/sudo", "usr/bin/passwd"];

/// Where the state file lives, relative to the root.
pub const STATE_PATH: &str = "var/lib/forge/materialized.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatEntry {
    pub path: String,
    pub kind: Kind,
}

/// What forge has put into a tree: the packages, and every path they own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Materialized {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default, rename = "entry")]
    pub entries: Vec<MatEntry>,
}

impl Materialized {
    /// Read the state of `root`; no file means nothing has been put there yet.
    pub fn load(root: &Path) -> Result<Self, StoreError> {
        let p = root.join(STATE_PATH);
        match fs::read_to_string(&p) {
            Ok(t) => toml::from_str(&t).map_err(|e| StoreError::Invalid { what: "materialized state", value: format!("{}: {e}", p.display()) }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_ctx(format!("reading {}", p.display()))(e)),
        }
    }

    /// Write the state atomically.
    pub fn save(&self, root: &Path) -> Result<(), StoreError> {
        let p = root.join(STATE_PATH);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).map_err(io_ctx(format!("creating {}", d.display())))?;
        }
        let text = toml::to_string(self).map_err(|e| StoreError::Invalid { what: "materialized state", value: e.to_string() })?;
        let tmp = p.with_extension("toml.forge-tmp");
        fs::write(&tmp, text).map_err(io_ctx(format!("writing {}", tmp.display())))?;
        fs::rename(&tmp, &p).map_err(io_ctx(format!("saving {}", p.display())))
    }

    fn owned(&self) -> BTreeMap<&str, Kind> {
        self.entries.iter().map(|e| (e.path.as_str(), e.kind)).collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MaterializeOptions {
    /// Overwrite files and links that exist but that forge did not put there.
    pub adopt: bool,
}

fn is_config(path: &str) -> bool {
    path == "etc" || path.starts_with("etc/")
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Put `packages` into `root`, replacing what `old` says forge put there before, and return the new state.
/// Nothing is removed until everything new is in place.
pub fn materialize(
    store: &Store,
    packages: &BTreeSet<StoreId>,
    root: &Path,
    old: &Materialized,
    opts: &MaterializeOptions,
) -> Result<Materialized, StoreError> {
    let plan = merge_plan(store, packages)?;
    let owned = old.owned();
    let mut entries = Vec::new();

    for (path, m) in &plan {
        let dest = root.join(path);
        let tracked = !is_config(path);
        match m.kind {
            Kind::Dir => {
                match fs::symlink_metadata(&dest) {
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => return Err(unmanaged(path, "a file or link is in the way of a directory")),
                    Err(_) => fs::create_dir(&dest).map_err(io_ctx(format!("creating {}", dest.display())))?,
                }
                if tracked {
                    entries.push(MatEntry { path: path.clone(), kind: Kind::Dir });
                }
            }
            Kind::File | Kind::Symlink => {
                if !tracked && exists(&dest) {
                    continue; // configuration the system already has stays as it is
                }
                if let Ok(meta) = fs::symlink_metadata(&dest) {
                    if meta.is_dir() {
                        return Err(unmanaged(path, "a directory is in the way of a file or link"));
                    }
                    if !owned.contains_key(path.as_str()) && !opts.adopt && !same_link(&dest, m) {
                        return Err(unmanaged(path, "it exists and forge did not put it there"));
                    }
                }
                place(store, m, path, &dest)?;
                if tracked {
                    entries.push(MatEntry { path: path.clone(), kind: m.kind });
                }
            }
        }
    }

    // Everything new is in. Now remove what the old state owned and the new plan no longer has.
    let new_paths: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let mut dirs = Vec::new();
    for e in &old.entries {
        if new_paths.contains(e.path.as_str()) {
            continue;
        }
        let p = root.join(&e.path);
        if e.kind == Kind::Dir {
            dirs.push(p);
        } else if let Err(err) = fs::remove_file(&p)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(io_ctx(format!("removing {}", p.display()))(err));
        }
    }
    dirs.sort();
    for d in dirs.into_iter().rev() {
        let _ = fs::remove_dir(&d); // only if empty: someone else's files in it stay
    }

    set_setuid(root, &plan)?;
    Ok(Materialized { packages: packages.iter().map(ToString::to_string).collect(), entries })
}

fn unmanaged(path: &str, why: &str) -> StoreError {
    StoreError::Unmanaged { path: path.to_string(), why: why.to_string() }
}

/// A link that already is what the package wants (so re-running changes nothing).
fn same_link(dest: &Path, m: &Merged) -> bool {
    m.kind == Kind::Symlink && m.target.as_deref().is_some_and(|t| fs::read_link(dest).is_ok_and(|cur| cur == Path::new(t)))
}

/// Write one file or symlink at `dest`, atomically replacing what is there.
fn place(store: &Store, m: &Merged, path: &str, dest: &Path) -> Result<(), StoreError> {
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".forge-new");
    let tmp = PathBuf::from(tmp);
    let _ = fs::remove_file(&tmp);
    match m.kind {
        Kind::Symlink => {
            let target = m.target.as_deref().ok_or_else(|| StoreError::Invalid { what: "symlink without a target", value: path.to_string() })?;
            symlink(target, &tmp).map_err(io_ctx(format!("linking {}", dest.display())))?;
        }
        _ => {
            let src = store.path(&m.package).join(path);
            // On btrfs the kernel makes this a reflink; elsewhere it is a copy. Either way a new inode.
            fs::copy(&src, &tmp).map_err(io_ctx(format!("copying {} to {}", src.display(), dest.display())))?;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(m.mode & 0o777))
                .map_err(io_ctx(format!("setting the mode of {}", dest.display())))?;
        }
    }
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_ctx(format!("replacing {}", dest.display()))(e)
    })
}

/// Make the allowlisted programs that this tree has setuid root (mode 4755).
fn set_setuid(root: &Path, plan: &BTreeMap<String, Merged>) -> Result<(), StoreError> {
    for f in SETUID_FILES {
        let Some(m) = plan.get(*f) else { continue };
        if m.kind != Kind::File {
            return Err(StoreError::Invalid { what: "setuid file", value: format!("/{f} is not a regular file") });
        }
        let p = root.join(f);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o4755)).map_err(io_ctx(format!("making /{f} setuid")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::make_pkg;
    use std::os::unix::fs::MetadataExt;

    struct Fx {
        d: tempfile::TempDir,
        store: Store,
        root: PathBuf,
    }

    impl Fx {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            let store = Store::new(d.path().join("store"));
            let root = d.path().join("root");
            fs::create_dir_all(&root).unwrap();
            Fx { d, store, root }
        }
        fn add(&self, name: &str, version: &str, files: &[(&str, &str)]) -> StoreId {
            let (bytes, _) = make_pkg(self.d.path(), name, version, files, &[]);
            self.store.install(std::io::Cursor::new(bytes), None).unwrap()
        }
        fn set(ids: &[&StoreId]) -> BTreeSet<StoreId> {
            ids.iter().map(|i| (*i).clone()).collect()
        }
        fn run(&self, ids: &[&StoreId], old: &Materialized) -> Result<Materialized, StoreError> {
            materialize(&self.store, &Self::set(ids), &self.root, old, &MaterializeOptions::default())
        }
    }

    #[test]
    fn files_are_real_copies_not_links_into_the_store() {
        let fx = Fx::new();
        let tool = fx.add("tool", "1", &[("usr/bin/tool", "T1")]);
        fx.run(&[&tool], &Materialized::default()).unwrap();
        let p = fx.root.join("usr/bin/tool");
        assert_eq!(fs::read_to_string(&p).unwrap(), "T1");
        assert!(!fs::symlink_metadata(&p).unwrap().file_type().is_symlink());
        assert_ne!(fs::metadata(&p).unwrap().ino(), fs::metadata(fx.store.path(&tool).join("usr/bin/tool")).unwrap().ino());
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn an_upgrade_replaces_changed_files_and_removes_dropped_ones() {
        let fx = Fx::new();
        let v1 = fx.add("tool", "1", &[("usr/bin/tool", "T1"), ("usr/share/tool/old.txt", "old")]);
        let st1 = fx.run(&[&v1], &Materialized::default()).unwrap();
        let v2 = fx.add("tool", "2", &[("usr/bin/tool", "T2")]);
        let st2 = fx.run(&[&v2], &st1).unwrap();
        assert_eq!(fs::read_to_string(fx.root.join("usr/bin/tool")).unwrap(), "T2");
        assert!(!fx.root.join("usr/share/tool/old.txt").exists(), "a file the new version dropped is removed");
        assert!(!fx.root.join("usr/share/tool").exists(), "and so is the directory it leaves empty");
        assert!(st2.entries.iter().all(|e| e.path != "usr/share/tool/old.txt"));
        assert_eq!(st2.packages, vec![v2.to_string()]);
    }

    #[test]
    fn a_file_forge_did_not_put_there_is_never_overwritten() {
        let fx = Fx::new();
        fs::create_dir_all(fx.root.join("usr/bin")).unwrap();
        fs::write(fx.root.join("usr/bin/tool"), "MINE").unwrap();
        let tool = fx.add("tool", "1", &[("usr/bin/tool", "T1")]);
        let e = fx.run(&[&tool], &Materialized::default()).unwrap_err();
        assert!(e.to_string().contains("usr/bin/tool"), "{e}");
        assert_eq!(fs::read_to_string(fx.root.join("usr/bin/tool")).unwrap(), "MINE");
        // Adopting says "replace it".
        let st = materialize(&fx.store, &Fx::set(&[&tool]), &fx.root, &Materialized::default(), &MaterializeOptions { adopt: true }).unwrap();
        assert_eq!(fs::read_to_string(fx.root.join("usr/bin/tool")).unwrap(), "T1");
        assert!(st.entries.iter().any(|e| e.path == "usr/bin/tool"));
    }

    #[test]
    fn files_the_user_added_beside_ours_survive_an_upgrade() {
        let fx = Fx::new();
        let v1 = fx.add("tool", "1", &[("usr/bin/tool", "T1")]);
        let st1 = fx.run(&[&v1], &Materialized::default()).unwrap();
        fs::write(fx.root.join("usr/bin/mine"), "mine").unwrap();
        let st2 = fx.run(&[], &st1).unwrap();
        assert!(!fx.root.join("usr/bin/tool").exists(), "the removed package's file is gone");
        assert_eq!(fs::read_to_string(fx.root.join("usr/bin/mine")).unwrap(), "mine", "the user's file stays");
        assert!(fx.root.join("usr/bin").is_dir(), "and so does the directory it is in");
        assert!(st2.packages.is_empty());
    }

    #[test]
    fn etc_files_are_created_if_missing_and_never_overwritten() {
        let fx = Fx::new();
        let a = fx.add("a", "1", &[("etc/a.conf", "default"), ("usr/bin/a", "A")]);
        let st1 = fx.run(&[&a], &Materialized::default()).unwrap();
        assert_eq!(fs::read_to_string(fx.root.join("etc/a.conf")).unwrap(), "default");
        fs::write(fx.root.join("etc/a.conf"), "edited").unwrap();
        let a2 = fx.add("a", "2", &[("etc/a.conf", "newer default"), ("usr/bin/a", "A2")]);
        let st = fx.run(&[&a2], &st1).unwrap();
        assert_eq!(fs::read_to_string(fx.root.join("etc/a.conf")).unwrap(), "edited");
        assert!(st.entries.iter().all(|e| !e.path.starts_with("etc")), "etc is not tracked, so never removed");
    }

    #[test]
    fn the_allowlisted_programs_become_setuid_and_the_store_copy_does_not() {
        let fx = Fx::new();
        let s = fx.add("sudo", "1", &[("usr/bin/sudo", "S"), ("usr/bin/other", "O")]);
        fx.run(&[&s], &Materialized::default()).unwrap();
        let mode = |p: &str| fs::metadata(fx.root.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("usr/bin/sudo"), 0o4755);
        assert_eq!(mode("usr/bin/other"), 0o755, "nothing outside the list");
        let in_store = fs::metadata(fx.store.path(&s).join("usr/bin/sudo")).unwrap().permissions().mode() & 0o7777;
        assert_eq!(in_store & 0o4000, 0, "the store keeps no setuid bit");
    }

    #[test]
    fn symlinks_are_recreated_as_symlinks_and_replaced_atomically() {
        let fx = Fx::new();
        let (bytes, _) = {
            let src = fx.d.path().join("srclink");
            fs::create_dir_all(src.join("usr/lib")).unwrap();
            fs::write(src.join("usr/lib/libz.so.1.3"), "Z").unwrap();
            symlink("libz.so.1.3", src.join("usr/lib/libz.so.1")).unwrap();
            let out = fx.d.path().join("z.tar.zst");
            let opts = crate::PackOptions { name: "z".into(), version: "1".into(), libc: forge_schema::Libc::Glibc, hash: None, deps: vec![] };
            let packed = crate::pack_dir(&src, &opts, &out).unwrap();
            (fs::read(&out).unwrap(), packed)
        };
        let z = fx.store.install(std::io::Cursor::new(bytes), None).unwrap();
        let st = fx.run(&[&z], &Materialized::default()).unwrap();
        assert_eq!(fs::read_link(fx.root.join("usr/lib/libz.so.1")).unwrap(), Path::new("libz.so.1.3"));
        // Running it again (state saved and loaded) changes nothing and does not trip over its own links.
        st.save(&fx.root).unwrap();
        let again = fx.run(&[&z], &Materialized::load(&fx.root).unwrap()).unwrap();
        assert_eq!(again, st);
    }

    #[test]
    fn two_packages_providing_one_file_is_still_a_conflict() {
        let fx = Fx::new();
        let a = fx.add("a", "1", &[("usr/bin/x", "1")]);
        let b = fx.add("b", "1", &[("usr/bin/x", "2")]);
        assert!(fx.run(&[&a, &b], &Materialized::default()).is_err());
        assert!(!fx.root.join("usr/bin/x").exists(), "nothing is written when the merge fails");
    }
}
