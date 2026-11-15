//! Merging packages into one tree.
//!
//! Every place that assembles a system from packages (the profile builder, the image builder, and
//! [`crate::materialize`]) needs the same answer to "which package owns which path", so the rules live here:
//!
//! - directories are shared;
//! - two packages providing the same file (or a directory and a file, or a symlink) is a conflict;
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

/// The merged tree of `packages`: path (relative, parents sort first) to its owner. Fails on a conflict.
pub fn merge_plan(store: &Store, packages: &BTreeSet<StoreId>) -> Result<BTreeMap<String, Merged>, StoreError> {
    let mut plan: BTreeMap<String, Merged> = BTreeMap::new();
    for pkg in packages {
        let manifest = store.manifest(pkg)?;
        for f in &manifest.files {
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
