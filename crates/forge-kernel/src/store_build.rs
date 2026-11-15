//! Building a kernel with the system's own packages, in the same sandbox every package build uses:
//! no compiler, `bc`, `perl` or `libelf` has to be installed first. The tools are fetched from the
//! repositories (verified like any package), a throwaway recipe describes the build, and the result is
//! the usual staged kernel (`boot/`, `usr/lib/modules/`).

use std::fs;
use std::path::{Path, PathBuf};

use forge_build::{BuildOptions, Tools};
use forge_schema::Libc;
use std::collections::BTreeSet;

use forge_store::{FetchHandle, Store, StoreId, Trust};

use crate::build::{Mode, Staged, lsmod_file};
use crate::install::staged_release;
use crate::{CONFIG_SCRIPT, BUILD_SCRIPT, KernelError, io_ctx, write};

/// What the kernel build needs from the store.
pub const BUILD_PACKAGES: &[&str] = &[
    "busybox", "make", "binutils", "gcc", "glibc", "linux-headers", "bc", "perl", "libelf", "flex", "bison", "m4", "zlib",
    "pkgconf", "bash", "openssl", "kmod",
];

pub struct StoreBuildSpec {
    pub version: String,
    /// The verified source tarball, already in `source_cache` under its checksum.
    pub tarball_sha256: String,
    pub source_cache: PathBuf,
    pub work: PathBuf,
    pub mode: Mode,
    pub jobs: usize,
    pub libc: Libc,
    /// Directories or `https://` base URLs the build tools come from.
    pub repos: Vec<String>,
    pub trust: Trust,
    pub fetch: FetchHandle,
    /// The package store the tools are installed into.
    pub store: PathBuf,
    /// The `forge` executable (re-run as the sandbox helper).
    pub helper: PathBuf,
    pub keep_work: bool,
    /// Keep the build tools this run installed (a cache for the next build). By default they are removed again.
    pub keep_tools: bool,
}

fn configure_step(mode: &Mode) -> Result<String, KernelError> {
    let cc = r#"make ARCH=x86_64 CC="gcc -std=gnu11""#;
    Ok(match mode {
        Mode::Full => "sh \"$RECIPE_DIR/full-config.sh\" full".to_string(),
        Mode::Auto { base_config, .. } => {
            let start = match base_config {
                Some(_) => format!("cp \"$RECIPE_DIR/base-config\" .config\n{cc} olddefconfig\n"),
                // No configuration of the running kernel: start from the standard one (every driver a module).
                None => "sh \"$RECIPE_DIR/full-config.sh\" full\n".to_string(),
            };
            format!("{start}yes '' | {cc} LSMOD=\"$RECIPE_DIR/lsmod.txt\" localmodconfig\nsh \"$RECIPE_DIR/full-config.sh\" essential\n")
        }
        Mode::File(_) => format!("cp \"$RECIPE_DIR/base-config\" .config\n{cc} olddefconfig\nsh \"$RECIPE_DIR/full-config.sh\" check\n"),
        Mode::Menu { .. } => {
            return Err(KernelError::Invalid(
                "--menuconfig needs an interactive terminal, which the sandbox does not give it yet: use --host-tools (and the build tools installed on this machine)"
                    .into(),
            ))
        }
    })
}

/// The throwaway recipe directory for this build.
pub fn write_recipe(dir: &Path, spec: &StoreBuildSpec) -> Result<(), KernelError> {
    fs::create_dir_all(dir).map_err(io_ctx(format!("creating {}", dir.display())))?;
    let libc = spec.libc.as_str();
    let deps: Vec<String> = BUILD_PACKAGES.iter().map(|p| format!("\"{p}\"")).collect();
    let recipe = format!(
        "name = \"linux-local\"\nversion = \"{v}\"\ndescription = \"A kernel built on this machine\"\nlibc = [\"{libc}\"]\n\n\
         [depends]\nbuild = [{deps}]\n\n\
         [[source]]\nurl = \"{url}\"\nsha256 = \"{sha}\"\n\n\
         [build]\nconfigure = '''\n{configure}'''\ncompile = \"sh \\\"$RECIPE_DIR/kernel-build.sh\\\" compile\"\ninstall = \"sh \\\"$RECIPE_DIR/kernel-build.sh\\\" install\"\n",
        v = spec.version,
        deps = deps.join(", "),
        url = crate::tarball_url(&spec.version),
        sha = spec.tarball_sha256,
        configure = configure_step(&spec.mode)?,
    );
    write(&dir.join("recipe.toml"), &recipe)?;
    write(&dir.join("full-config.sh"), CONFIG_SCRIPT)?;
    write(&dir.join("kernel-build.sh"), BUILD_SCRIPT)?;
    match &spec.mode {
        Mode::Auto { base_config, loaded_modules, keep } => {
            if let Some(base) = base_config {
                fs::copy(base, dir.join("base-config")).map_err(io_ctx(format!("copying {}", base.display())))?;
            }
            write(&dir.join("lsmod.txt"), &lsmod_file(loaded_modules, keep))?;
        }
        Mode::File(f) | Mode::Menu { base_config: f } => {
            fs::copy(f, dir.join("base-config")).map_err(io_ctx(format!("copying {}", f.display())))?;
        }
        Mode::Full => {}
    }
    Ok(())
}

/// Build, then unpack the resulting package into a private store and return it as a staged kernel.
pub fn build_in_store(spec: &StoreBuildSpec) -> Result<Staged, KernelError> {
    if spec.libc != Libc::Glibc {
        return Err(KernelError::Invalid("building in the store needs the glibc toolchain (musl is no longer supported)".into()));
    }
    let recipe_dir = spec.work.join(format!("recipe-{}", spec.version));
    if recipe_dir.exists() {
        return Err(KernelError::Exists(recipe_dir));
    }
    write_recipe(&recipe_dir, spec)?;

    let out_dir = spec.work.join(format!("package-{}", spec.version));
    let opts = BuildOptions {
        recipe_dir,
        libc: spec.libc,
        out_dir: out_dir.clone(),
        work_dir: spec.work.join("build"),
        source_cache: spec.source_cache.clone(),
        repos: spec.repos.iter().map(PathBuf::from).collect(),
        trust: spec.trust.clone(),
        fetch: spec.fetch.clone(),
        tools: Tools::Store,
        store: spec.store.clone(),
        jobs: spec.jobs.max(1) as u32,
        helper: spec.helper.clone(),
        keep_work: spec.keep_work,
    };
    // Remember what the store held, so that only what this run installs is removed afterwards.
    let tools = Store::new(&spec.store);
    let before: BTreeSet<StoreId> = tools.list().map_err(|e| KernelError::Invalid(e.to_string()))?.into_iter().collect();
    let result = forge_build::build(&opts).map_err(|e| KernelError::Fetch(format!("the kernel build failed: {e}")));
    if !spec.keep_tools {
        let n = remove_new(&tools, &before);
        if n > 0 {
            eprintln!("removed the {n} build tool package(s) this build had installed (use --keep-tools to keep them for next time)");
        }
    }
    let built = result?;

    // The package is a verified tree of boot/ and usr/lib/modules/: unpack it into a store of its own.
    let kstore = Store::new(spec.work.join("kernel-store"));
    let file = fs::File::open(&built.archive).map_err(io_ctx(format!("opening {}", built.archive.display())))?;
    let id = kstore.install(file, Some(&built.archive_sha256)).map_err(|e| KernelError::Invalid(e.to_string()))?;
    let dir = kstore.path(&id);
    let release = staged_release(&dir)?;
    Ok(Staged { release, dir })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(t: &Path, mode: Mode) -> StoreBuildSpec {
        StoreBuildSpec {
            version: "7.3.3".into(),
            tarball_sha256: "ab".repeat(32),
            source_cache: t.join("cache"),
            work: t.join("work"),
            mode,
            jobs: 4,
            libc: Libc::Glibc,
            repos: vec!["https://example.test/repo".into()],
            trust: Trust::AllowUnsigned,
            fetch: FetchHandle::default(),
            store: t.join("store"),
            helper: "/usr/bin/forge".into(),
            keep_work: false,
            keep_tools: false,
        }
    }

    #[test]
    fn the_throwaway_recipe_is_valid_names_the_tools_and_the_verified_tarball() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("r");
        write_recipe(&dir, &spec(t.path(), Mode::Full)).unwrap();
        let text = fs::read_to_string(dir.join("recipe.toml")).unwrap();
        let recipe = forge_schema::Recipe::parse(&text).expect("the generated recipe parses");
        assert_eq!(recipe.version, "7.3.3");
        for pkg in ["gcc", "bc", "perl", "libelf", "kmod", "openssl", "bash"] {
            assert!(recipe.depends.build.iter().any(|d| d == pkg), "{pkg}");
        }
        assert_eq!(recipe.sources[0].url, "https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.3.3.tar.xz");
        assert_eq!(recipe.sources[0].sha256, "ab".repeat(32));
        assert!(text.contains("full-config.sh\\\" full") || text.contains("full-config.sh\" full"), "{text}");
        assert!(dir.join("kernel-build.sh").exists() && dir.join("full-config.sh").exists());
    }

    #[test]
    fn auto_mode_ships_the_base_config_and_the_module_list_next_to_the_recipe() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("cfg");
        fs::write(&base, "CONFIG_X=y\n").unwrap();
        let mode = Mode::Auto { base_config: Some(base), loaded_modules: "e1000e 1 0 - Live 0x0\n".into(), keep: vec!["usb_storage".into()] };
        let dir = t.path().join("r");
        write_recipe(&dir, &spec(t.path(), mode)).unwrap();
        let text = fs::read_to_string(dir.join("recipe.toml")).unwrap();
        assert!(text.contains("localmodconfig") && text.contains("full-config.sh\" essential"), "{text}");
        assert!(forge_schema::Recipe::parse(&text).is_ok());
        assert_eq!(fs::read_to_string(dir.join("base-config")).unwrap(), "CONFIG_X=y\n");
        let lsmod = fs::read_to_string(dir.join("lsmod.txt")).unwrap();
        assert!(lsmod.contains("e1000e 1 0\n") && lsmod.contains("usb_storage 0 0\n"), "{lsmod}");
    }

    #[test]
    fn auto_mode_without_a_running_config_starts_from_the_standard_one() {
        let t = tempfile::tempdir().unwrap();
        let mode = Mode::Auto { base_config: None, loaded_modules: "e1000e 1 0 - Live 0x0\n".into(), keep: vec![] };
        let dir = t.path().join("r");
        write_recipe(&dir, &spec(t.path(), mode)).unwrap();
        let text = fs::read_to_string(dir.join("recipe.toml")).unwrap();
        assert!(forge_schema::Recipe::parse(&text).is_ok());
        assert!(text.contains("full-config.sh\" full") && text.contains("localmodconfig"), "{text}");
        assert!(!text.contains("base-config") && !dir.join("base-config").exists());
    }

    #[test]
    fn menuconfig_is_refused_here_with_a_pointer_to_the_host_mode() {
        let t = tempfile::tempdir().unwrap();
        let e = write_recipe(&t.path().join("r"), &spec(t.path(), Mode::Menu { base_config: t.path().join("x") })).unwrap_err();
        assert!(e.to_string().contains("--host-tools"), "{e}");
    }

    #[test]
    fn only_glibc_is_supported_and_an_existing_recipe_directory_is_not_overwritten() {
        let t = tempfile::tempdir().unwrap();
        let mut s = spec(t.path(), Mode::Full);
        s.libc = Libc::Musl; // no longer supported
        assert!(build_in_store(&s).is_err());
        s.libc = Libc::Glibc;
        fs::create_dir_all(t.path().join("work/recipe-7.3.3")).unwrap();
        assert!(matches!(build_in_store(&s), Err(KernelError::Exists(_))));
    }
}

/// Remove every package that is in `store` now but was not in `before`; returns how many. A package that was
/// already there (installed for the system, or by an earlier build) is never touched.
pub fn remove_new(store: &Store, before: &BTreeSet<StoreId>) -> usize {
    let mut removed = 0;
    for id in store.list().unwrap_or_default() {
        if !before.contains(&id) && store.remove(&id).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use forge_store::{PackOptions, pack_dir};

    /// Pack a one-file package named `name` and install it into `store`.
    fn add(scratch: &Path, store: &Store, name: &str) -> StoreId {
        let src = scratch.join(format!("src-{name}"));
        fs::create_dir_all(src.join("usr/bin")).unwrap();
        fs::write(src.join("usr/bin").join(name), name).unwrap();
        let out = scratch.join(format!("{name}.tar.zst"));
        let opts = PackOptions { name: name.into(), version: "1".into(), libc: Libc::Glibc, hash: None, deps: vec![] };
        pack_dir(&src, &opts, &out).unwrap();
        store.install(fs::File::open(&out).unwrap(), None).unwrap()
    }

    #[test]
    fn only_packages_added_since_the_snapshot_are_removed() {
        let t = tempfile::tempdir().unwrap();
        let store = Store::new(t.path().join("store"));
        let old = add(t.path(), &store, "old");
        let before: BTreeSet<StoreId> = store.list().unwrap().into_iter().collect();
        let a = add(t.path(), &store, "tool-a");
        let b = add(t.path(), &store, "tool-b");
        assert_eq!(remove_new(&store, &before), 2);
        assert!(store.has(&old), "what was there before stays");
        assert!(!store.has(&a) && !store.has(&b), "what the build installed goes");
        assert_eq!(remove_new(&store, &before), 0, "nothing left to remove");
    }
}
