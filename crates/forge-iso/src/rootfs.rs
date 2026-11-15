//! Assembling a root filesystem tree from repository packages.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use forge_schema::Libc;
use forge_store::{LocalRepo, Materialized, MaterializeOptions, Store, StoreId, Trust};

use crate::{IsoError, io_ctx};

/// Resolve `names` (and their dependencies) in the repositories, verify each archive
/// as it is installed into `store_dir`, and return the store paths in dependency order.
/// Indexes are checked against `trust` like any other consumer.
pub fn fetch(repos: &[PathBuf], trust: &Trust, names: &[String], libc: Libc, store_dir: &Path) -> Result<Vec<PathBuf>, IsoError> {
    let store = Store::new(store_dir);
    let loaded: Vec<(LocalRepo, forge_store::Index)> = repos
        .iter()
        .map(|r| {
            let repo = LocalRepo::new(r);
            let index = repo.load(trust)?.index;
            Ok((repo, index))
        })
        .collect::<Result<_, forge_store::StoreError>>()?;

    // One merged view, so a package can depend on another repository's package.
    let mut merged = forge_store::Index::default();
    for (_, idx) in &loaded {
        for e in &idx.packages {
            merged.insert(e.clone());
        }
    }
    let entries = forge_store::resolve(&merged, names, libc)?;
    let mut paths = Vec::new();
    for e in &entries {
        let (repo, _) = loaded
            .iter()
            .find(|(_, idx)| idx.packages.iter().any(|p| p == e))
            .expect("resolved entries come from a loaded repository");
        let id = store.install(repo.open_archive(e)?, Some(&e.archive_sha256))?;
        paths.push(store.path(&id));
    }
    Ok(paths)
}

/// Put the packages (store paths from [`fetch`]) into `dest`, the live root, exactly as `forge apply` puts
/// packages into a system: real copies, the same merge rules (two packages providing one file is an error),
/// setuid on the allowlisted programs, and the state file recording which files forge owns, so that the first
/// `forge apply` on a system installed from this image knows what it may replace. `dest` must start out empty.
pub fn materialize_into(store_dir: &Path, trees: &[PathBuf], dest: &Path) -> Result<(), IsoError> {
    fs::create_dir_all(dest).map_err(io_ctx(format!("creating {}", dest.display())))?;
    if fs::read_dir(dest).map_err(io_ctx("reading the root directory"))?.next().is_some() {
        return Err(IsoError::Invalid { what: "root directory", value: format!("{} is not empty", dest.display()) });
    }
    let mut ids = BTreeSet::new();
    for t in trees {
        let name = t.file_name().and_then(|n| n.to_str()).unwrap_or("");
        ids.insert(StoreId::parse(name).ok_or_else(|| IsoError::Invalid { what: "package directory", value: t.display().to_string() })?);
    }
    let store = Store::new(store_dir);
    let state = forge_store::materialize(&store, &ids, dest, &Materialized::default(), &MaterializeOptions::default())?;
    state.save(dest)?;

    // The image carries the packages too (the squashfs stores identical files once), so that a system installed
    // from it has them in its store: a later `forge apply` needs every package of the set to merge from, and
    // downloads nothing when the repository has the same builds.
    let dest_store = dest.join("forge/store");
    fs::create_dir_all(&dest_store).map_err(io_ctx("creating /forge/store"))?;
    for t in trees {
        let name = t.file_name().expect("checked above");
        copy_tree(t, &dest_store.join(name))?;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    forge_store::Profile::new(dest.join("forge/profiles/system")).record(&ids, now)?;
    Ok(())
}

/// Copy a package tree, keeping modes and symlinks; directories get their (read-only) modes last.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), IsoError> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let meta = fs::symlink_metadata(src).map_err(io_ctx(format!("reading {}", src.display())))?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(src).map_err(io_ctx(format!("reading {}", src.display())))?;
        symlink(target, dst).map_err(io_ctx(format!("linking {}", dst.display())))?;
    } else if meta.is_dir() {
        fs::create_dir_all(dst).map_err(io_ctx(format!("creating {}", dst.display())))?;
        let mut names: Vec<_> = fs::read_dir(src).map_err(io_ctx(format!("reading {}", src.display())))?.collect::<Result<_, _>>().map_err(io_ctx("reading a directory"))?;
        names.sort_by_key(|e| e.file_name());
        for e in names {
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        fs::set_permissions(dst, fs::Permissions::from_mode(meta.permissions().mode() & 0o7777)).map_err(io_ctx(format!("setting the mode of {}", dst.display())))?;
    } else {
        fs::copy(src, dst).map_err(io_ctx(format!("copying {}", src.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn install(store: &Store, scratch: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let src = scratch.join(format!("src-{name}"));
        for (path, content) in files {
            let p = src.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
            if path.contains("bin/") {
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let out = scratch.join(format!("{name}.tar.zst"));
        let opts = forge_store::PackOptions { name: name.into(), version: "1.0".into(), libc: Libc::Glibc, hash: None, deps: vec![] };
        forge_store::pack_dir(&src, &opts, &out).unwrap();
        let id = store.install(fs::File::open(&out).unwrap(), None).unwrap();
        store.path(&id)
    }

    #[test]
    fn packages_share_directories_keep_modes_and_record_what_forge_owns() {
        let t = tempfile::tempdir().unwrap();
        let store_dir = t.path().join("store");
        let store = Store::new(&store_dir);
        let a = install(&store, t.path(), "a", &[("usr/bin/a", "A"), ("usr/share/doc/a", "doc")]);
        let b = install(&store, t.path(), "b", &[("usr/bin/b", "B")]);
        let dest = t.path().join("root");
        materialize_into(&store_dir, &[a, b], &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("usr/bin/a")).unwrap(), "A");
        assert_eq!(fs::read_to_string(dest.join("usr/bin/b")).unwrap(), "B");
        assert_eq!(fs::metadata(dest.join("usr/bin/a")).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(!dest.join(".forge").exists(), "the package bookkeeping is not part of the system");
        let state = Materialized::load(&dest).unwrap();
        assert_eq!(state.packages.len(), 2);
        assert!(state.entries.iter().any(|e| e.path == "usr/bin/a"));
        // The packages travel with the image, and the generation records which ones they are.
        let ids = forge_store::Profile::new(dest.join("forge/profiles/system")).current_packages().unwrap();
        assert_eq!(ids.len(), 2);
        for id in &ids {
            assert!(dest.join("forge/store").join(id.as_str()).join(".forge/manifest.toml").is_file(), "{id} is in the image's store");
        }
    }

    #[test]
    fn the_live_root_stays_editable() {
        // The store's trees are read-only; the live root is written to afterwards (hostname, passwd, ...).
        let t = tempfile::tempdir().unwrap();
        let store_dir = t.path().join("store");
        let store = Store::new(&store_dir);
        let a = install(&store, t.path(), "a", &[("usr/bin/a", "A")]);
        let dest = t.path().join("root");
        materialize_into(&store_dir, &[a], &dest).unwrap();
        fs::write(dest.join("usr/new-file"), "x").unwrap();
        fs::create_dir(dest.join("usr/bin/../newdir")).unwrap();
    }

    #[test]
    fn a_file_in_two_packages_is_a_conflict() {
        let t = tempfile::tempdir().unwrap();
        let store_dir = t.path().join("store");
        let store = Store::new(&store_dir);
        let a = install(&store, t.path(), "a", &[("usr/bin/x", "1")]);
        let b = install(&store, t.path(), "b", &[("usr/bin/x", "2")]);
        assert!(materialize_into(&store_dir, &[a, b], &t.path().join("root")).is_err());
    }

    #[test]
    fn a_non_empty_destination_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let store_dir = t.path().join("store");
        let store = Store::new(&store_dir);
        let a = install(&store, t.path(), "a", &[("usr/bin/a", "A")]);
        let dest = t.path().join("root");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("stray"), "x").unwrap();
        assert!(materialize_into(&store_dir, &[a], &dest).is_err());
    }
}
