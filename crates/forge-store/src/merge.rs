//! Merging packages into one tree.
//!
//! Every place that assembles a system from packages (the profile builder, the image builder, and
//! [`crate::materialize`]) needs the same answer to "which package owns which path", so the rules live here:
//!
//! - directories are shared;
//! - two packages providing the same file (or a directory and a file, or a symlink) is a conflict, except that a package made
//!   from Debian packages never wins one against a system package: its copy is left out;
//! - a BusyBox applet link is only a fallback: a real package's file replaces it, and it never replaces one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::StoreError;
use crate::id::StoreId;
use crate::manifest::Kind;
use crate::store::Store;

/// One path of the merged tree and the package that provides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub package: StoreId,
    pub kind: Kind,
    /// Permission bits as the package recorded them (never setuid or setgid: the store strips those).
    pub mode: u32,
    /// Symlinks only.
    pub target: Option<String>,
    /// A link to `busybox`, which any real file may replace.
    pub applet: bool,
}

/// Packages made from Debian packages (`forge deb`) carry copies of libraries the system has of its own. Wherever one of them
/// provides a path another (non-Debian) package provides too, the system's package keeps it and the Debian copy is left out:
/// the Debian package was made to use the system's libraries anyway, and which version of a library it was made against must
/// never stop an upgrade.
fn is_debian_made(id: &StoreId) -> bool {
    id.name().starts_with("deb-")
}

/// The merged tree of `packages`: path (relative, parents sort first) to its owner. Fails on a conflict.
pub fn merge_plan(store: &Store, packages: &BTreeSet<StoreId>) -> Result<BTreeMap<String, Merged>, StoreError> {
    let mut plan: BTreeMap<String, Merged> = BTreeMap::new();
    // The system's own packages first, so that anything a Debian-made one also carries is already taken.
    let ordered: Vec<&StoreId> = packages.iter().filter(|p| !is_debian_made(p)).chain(packages.iter().filter(|p| is_debian_made(p))).collect();
    for pkg in ordered {
        let manifest = store.manifest(pkg)?;
        let debian = is_debian_made(pkg);
        // Paths of this package that the system's packages hold, with everything below them.
        let mut left_out: Vec<String> = Vec::new();
        for f in &manifest.files {
            if debian && left_out.iter().any(|d| f.path.starts_with(&format!("{d}/"))) {
                continue;
            }
            if debian && under_system_file(&plan, &f.path) {
                continue;
            }
            let applet = f.kind == Kind::Symlink
                && f.target.as_deref().is_some_and(|t| Path::new(t).file_name().is_some_and(|n| n == "busybox"));
            let entry = Merged { package: pkg.clone(), kind: f.kind, mode: f.mode, target: f.target.clone(), applet };
            match plan.get(&f.path) {
                // Directories are shared: both packages just need it to exist.
                Some(Merged { kind: Kind::Dir, .. }) if f.kind == Kind::Dir => {}
                Some(_) if applet => {}
                Some(existing) if existing.applet => {
                    plan.insert(f.path.clone(), entry);
                }
                // the system's package keeps it; a directory of ours there is left out with its contents
                Some(existing) if debian && !is_debian_made(&existing.package) => {
                    if f.kind == Kind::Dir {
                        left_out.push(f.path.clone());
                    }
                }
                Some(existing) => {
                    return Err(StoreError::Conflict { path: f.path.clone(), a: existing.package.clone(), b: pkg.clone() });
                }
                None => {
                    plan.insert(f.path.clone(), entry);
                }
            }
        }
    }
    Ok(plan)
}

/// Is a parent of `path` something a system package made a file or link, so that nothing can be put below it?
fn under_system_file(plan: &BTreeMap<String, Merged>, path: &str) -> bool {
    let mut p = path;
    while let Some((parent, _)) = p.rsplit_once('/') {
        if let Some(m) = plan.get(parent) {
            if m.kind != Kind::Dir && !is_debian_made(&m.package) {
                return true;
            }
        }
        p = parent;
    }
    false
}
