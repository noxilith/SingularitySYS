//! Working out which Debian packages an install needs.
//!
//! Debian's `Depends` are followed, except where the system already has the thing:
//! - names that must never come from Debian (libc, the dpkg machinery);
//! - packages whose name is also a forge package (dbus, systemd, bash...): the forge one is used;
//! - library packages whose shared libraries are all already in `/usr/lib`: the system's are used.
//!
//! Recommends are not followed, with a few exceptions that make desktop libraries work (the tools that build
//! GLib's schema cache and GDK-Pixbuf's loader cache, the MIME database, a base icon theme).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Result, anyhow, bail};

use crate::apt::{Pkg, fetch_deb, version_cmp};
use crate::convert::{World, list_package};

/// Packages that are never taken from Debian: the C library (the system has its own), and the package-management
/// and maintainer-script machinery that has no meaning here.
const NEVER: &[&str] = &[
    "libc6", "libc-bin", "libc6-dev", "libc-dev-bin", "libgcc-s1", "libgcc1", "libstdc++6", "gcc-12-base", "gcc-13-base", "gcc-14-base",
    "gcc-15-base", "base-files", "base-passwd", "dpkg", "debconf", "debconf-2.0", "cdebconf", "perl-base", "init-system-helpers",
    "lsb-base", "sysvinit-utils", "adduser", "passwd", "login", "debianutils", "mawk", "ucf", "dash", "bash", "coreutils", "sed",
    "grep", "findutils", "tar", "gzip", "hostname", "systemd-sysv", "libpam-runtime", "tzdata", "ca-certificates", "util-linux",
    "udev",
];

/// Debian's GL stack. The system has its own (libglvnd and Mesa, one version of each); a second copy from Debian puts
/// a different libEGL, libOpenGL, libGLX and libgallium next to it and desktop OpenGL stops working (Xwayland: no glamor).
const SYSTEM_GL: &[&str] = &[
    "libglvnd0", "libegl1", "libgl1", "libglx0", "libopengl0", "libgles1", "libgles2", "libglx-mesa0", "libegl-mesa0",
    "libgl1-mesa-dri", "libgl1-mesa-glx", "libglapi-mesa", "mesa-libgallium", "libgbm1", "libglx-dev", "libegl-dev",
    "libgl-dev", "libopengl-dev", "libgles-dev",
];

/// When a package of the first kind is in the set, these come along (Recommends that make the desktop stack work).
const EXTRAS: &[(&str, &[&str])] = &[
    ("libglib2.0-0t64", &["libglib2.0-bin", "shared-mime-info"]),
    ("libglib2.0-0", &["libglib2.0-bin", "shared-mime-info"]),
    ("libgdk-pixbuf-2.0-0", &["libgdk-pixbuf2.0-bin"]),
    ("libgtk-3-0t64", &["libgtk-3-bin", "hicolor-icon-theme", "adwaita-icon-theme"]),
    ("libgtk-3-0", &["libgtk-3-bin", "hicolor-icon-theme", "adwaita-icon-theme"]),
];

pub struct Resolution {
    /// Debian packages to unpack, the requested one first.
    pub selected: Vec<Pkg>,
    /// Names of forge packages the result relies on.
    pub system_deps: BTreeSet<String>,
    /// Packages left to the system, with the reason (for the report).
    pub left_out: Vec<(String, &'static str)>,
}

struct Index<'a> {
    by_name: HashMap<&'a str, Vec<&'a Pkg>>,
    provides: HashMap<&'a str, Vec<&'a Pkg>>,
}

impl<'a> Index<'a> {
    fn new(pkgs: &'a [Pkg]) -> Self {
        let mut by_name: HashMap<&str, Vec<&Pkg>> = HashMap::new();
        let mut provides: HashMap<&str, Vec<&Pkg>> = HashMap::new();
        for p in pkgs {
            by_name.entry(p.name.as_str()).or_default().push(p);
            for v in &p.provides {
                provides.entry(v.as_str()).or_default().push(p);
            }
        }
        Index { by_name, provides }
    }

    /// The newest package of that name (earlier sources win a tie), or a package that provides it.
    fn pick(&self, name: &str) -> Option<&'a Pkg> {
        if let Some(v) = self.by_name.get(name) {
            let mut best = v[0];
            for p in &v[1..] {
                if version_cmp(&p.version, &best.version) == std::cmp::Ordering::Greater {
                    best = p;
                }
            }
            return Some(best);
        }
        let v = self.provides.get(name)?;
        let mut best = v[0];
        for p in &v[1..] {
            if version_cmp(&p.version, &best.version) == std::cmp::Ordering::Greater {
                best = p;
            }
        }
        Some(best)
    }
}

pub fn resolve(
    requested: &str,
    self_name: &str,
    pkgs: &[Pkg],
    world: &World,
    cache: &Path,
    forge_has: &dyn Fn(&str) -> bool,
    say: &dyn Fn(&str),
) -> Result<Resolution> {
    let idx = Index::new(pkgs);
    let mut res = Resolution { selected: Vec::new(), system_deps: BTreeSet::new(), left_out: Vec::new() };
    let mut satisfied: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, Option<String>)> = VecDeque::new();
    queue.push_back((requested.to_string(), None));
    let mut extras_done: HashSet<&str> = HashSet::new();

    loop {
        while let Some((name, needed_by)) = queue.pop_front() {
            if satisfied.contains(&name) {
                continue;
            }
            let Some(pkg) = idx.pick(&name) else {
                match &needed_by {
                    None => bail!("no Debian package called {name} in the configured sources (try `forge deb search {name}`)"),
                    Some(by) if by.starts_with("(extra)") => continue,
                    Some(by) => bail!("{by} needs {name}, which is in none of the configured sources"),
                }
            };
            let is_request = needed_by.is_none();
            if !is_request {
                if NEVER.contains(&pkg.name.as_str()) {
                    satisfied.insert(name.clone());
                    satisfied.insert(pkg.name.clone());
                    res.left_out.push((pkg.name.clone(), "the system has its own"));
                    continue;
                }
                if SYSTEM_GL.contains(&pkg.name.as_str()) && forge_has("mesa") && forge_has("libglvnd") {
                    res.system_deps.insert("mesa".to_string());
                    res.system_deps.insert("libglvnd".to_string());
                    satisfied.insert(name.clone());
                    satisfied.insert(pkg.name.clone());
                    res.left_out.push((pkg.name.clone(), "the system has its own GL libraries (libglvnd, Mesa)"));
                    continue;
                }
                if forge_has(&pkg.name) {
                    res.system_deps.insert(pkg.name.clone());
                    satisfied.insert(name.clone());
                    satisfied.insert(pkg.name.clone());
                    res.left_out.push((pkg.name.clone(), "a forge package of that name"));
                    continue;
                }
                // A library package whose libraries the system already has is not needed.
                let deb = fetch_deb(cache, pkg)?;
                let listing = list_package(&deb)?;
                if !listing.has_other && !listing.sonames.is_empty() && listing.sonames.iter().all(|s| world.has_foreign(&format!("usr/lib/{s}"), self_name)) {
                    for s in &listing.sonames {
                        if let Some((owner, _)) = world.owners.owner(&format!("usr/lib/{s}")).filter(|(o, _)| o != self_name) {
                            res.system_deps.insert(owner.clone());
                        }
                    }
                    satisfied.insert(name.clone());
                    satisfied.insert(pkg.name.clone());
                    res.left_out.push((pkg.name.clone(), "the system has these libraries"));
                    continue;
                }
            }
            say(&format!("  {} {}", pkg.name, pkg.version));
            satisfied.insert(name.clone());
            satisfied.insert(pkg.name.clone());
            for p in &pkg.provides {
                satisfied.insert(p.clone());
            }
            for group in &pkg.depends {
                if group.iter().any(|a| satisfied.contains(a)) {
                    continue;
                }
                match group.iter().find(|a| idx.pick(a).is_some()) {
                    Some(alt) => queue.push_back((alt.clone(), Some(pkg.name.clone()))),
                    None => return Err(anyhow!("{} needs {}, which is in none of the configured sources", pkg.name, group.join(" or "))),
                }
            }
            res.selected.push(pkg.clone());
        }
        // The extras for what has been selected so far; if any were queued, go round again.
        let mut queued = false;
        for (trigger, add) in EXTRAS {
            if extras_done.contains(trigger) || !res.selected.iter().any(|p| p.name == *trigger) {
                continue;
            }
            extras_done.insert(trigger);
            for a in *add {
                queue.push_back(((*a).to_string(), Some("(extra)".to_string())));
                queued = true;
            }
        }
        if !queued {
            break;
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::Owners;

    fn pkg(name: &str, depends: &[&str]) -> Pkg {
        Pkg {
            name: name.into(),
            version: "1".into(),
            arch: "amd64".into(),
            depends: depends.iter().map(|d| vec![(*d).to_string()]).collect(),
            provides: vec![],
            filename: String::new(),
            sha256: String::new(),
            size: 0,
            description: String::new(),
            source: String::new(),
            base_url: String::new(),
        }
    }

    #[test]
    fn debians_gl_stack_is_left_to_the_systems_mesa_and_libglvnd() {
        let pkgs = [pkg("app", &["libegl1", "libgl1", "libglx-mesa0", "libopengl0"]), pkg("libegl1", &[]), pkg("libgl1", &[]), pkg("libglx-mesa0", &[]), pkg("libopengl0", &[])];
        let owners = Owners::empty();
        let world = World { root: Path::new("/nonexistent"), owners: &owners };
        let forge_has = |n: &str| n == "mesa" || n == "libglvnd";
        let res = resolve("app", "deb-app", &pkgs, &world, Path::new("/nonexistent"), &forge_has, &|_| {}).unwrap();
        assert_eq!(res.selected.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["app"]);
        assert!(res.system_deps.contains("mesa") && res.system_deps.contains("libglvnd"));
        assert_eq!(res.left_out.len(), 4);
    }
}
