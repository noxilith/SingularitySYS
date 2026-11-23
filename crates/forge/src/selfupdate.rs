//! A newer forge that an upgrade has just downloaded, but cannot run yet.
//!
//! `forge upgrade` downloads every package of the plan into the store first, and only then copies them into the system. When the
//! plan includes a newer forge, the copy is still done by the old one, so whatever the newer one learned (a conflict it
//! resolves, a layout it understands) does not help yet: the upgrade fails the way it would have before. Instead of leaving that
//! to the person to work around by hand, the old forge hands over to the newer one from the store and lets it finish.

use std::path::{Path, PathBuf};

/// The directory name of a store entry for the `forge` package, as `(version, name)`; None for any other package.
fn forge_entry(dir_name: &str) -> Option<&str> {
    let mut parts = dir_name.rsplitn(3, '-');
    let (_hash, version, name) = (parts.next()?, parts.next()?, parts.next()?);
    (name == "forge").then_some(version)
}

/// Of the store entries `dirs`, the forge newer than the one named `current` (None when `current` is the newest, or is not a
/// store entry of forge at all).
pub fn newer_than<'a>(dirs: &'a [String], current: &str) -> Option<&'a str> {
    let current_version = forge_entry(current)?;
    dirs.iter()
        .filter_map(|d| forge_entry(d).map(|v| (v, d.as_str())))
        .filter(|(v, _)| forge_store::version_cmp(v, current_version) == std::cmp::Ordering::Greater)
        .max_by(|a, b| forge_store::version_cmp(a.0, b.0))
        .map(|(_, d)| d)
}

/// The `forge` binary of a newer forge in the store under `root`, when the running one is an older store entry.
pub fn newer_forge(root: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let store = root.join("forge/store");
    let current = exe.strip_prefix(&store).ok()?.components().next()?.as_os_str().to_string_lossy().into_owned();
    let dirs: Vec<String> = std::fs::read_dir(&store).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let next = newer_than(&dirs, &current)?;
    let bin = store.join(next).join("usr/bin/forge");
    bin.is_file().then_some(bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_newest_forge_in_the_store_is_picked_when_it_is_newer_than_the_running_one() {
        let d = dirs(&["forge-0.1.0_r162-aaa", "forge-0.1.0_r167-bbb", "forge-0.1.0_r168-ccc", "mesa-25.2.5_r3-ddd", "forge-extras-1.0-eee"]);
        assert_eq!(newer_than(&d, "forge-0.1.0_r167-bbb"), Some("forge-0.1.0_r168-ccc"));
        assert_eq!(newer_than(&d, "forge-0.1.0_r162-aaa"), Some("forge-0.1.0_r168-ccc"), "r10 style numbers compare as numbers");
        assert_eq!(newer_than(&d, "forge-0.1.0_r168-ccc"), None, "already the newest");
        assert_eq!(newer_than(&d, "mesa-25.2.5_r3-ddd"), None, "not a forge entry");
    }

    #[test]
    fn revisions_compare_as_numbers() {
        let d = dirs(&["forge-0.1.0_r99-a", "forge-0.1.0_r100-b"]);
        assert_eq!(newer_than(&d, "forge-0.1.0_r99-a"), Some("forge-0.1.0_r100-b"));
    }
}
