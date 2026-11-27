//! The whole job: resolve, download, convert, pack, publish into the local repository.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use forge_store::{LocalRepo, PackOptions, SecretKey, pack_dir};

use crate::apt::{self, Pkg, Source, Sources};
use crate::convert::{self, Owners, Tree, World};
use crate::resolve::resolve;

/// A forge package made from Debian packages is called `deb-<debian name>`.
pub const PREFIX: &str = "deb-";

pub struct Config {
    pub root: PathBuf,
}

impl Config {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Config { root: root.into() }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
    pub fn sources_file(&self) -> PathBuf {
        self.p("etc/forge/deb/sources.toml")
    }
    pub fn keys_dir(&self) -> PathBuf {
        self.p("etc/forge/deb/keys")
    }
    pub fn cache(&self) -> PathBuf {
        self.p("var/cache/forge/deb")
    }
    /// The local repository the converted packages are published into (add it to `repos`).
    pub fn repo_dir(&self) -> PathBuf {
        self.p("var/lib/forge/deb-repo")
    }
    pub fn signing_dir(&self) -> PathBuf {
        self.p("etc/forge/deb")
    }
    pub fn trusted_keys_dir(&self) -> PathBuf {
        self.p("etc/forge/keys")
    }
    pub fn sources(&self) -> Result<Sources> {
        Sources::load(&self.sources_file())
    }
}

/// The signing key of the local repository: made on first use, its public half trusted by forge.
pub fn ensure_signing_key(cfg: &Config) -> Result<SecretKey> {
    let dir = cfg.signing_dir();
    let secret = dir.join("local.key");
    if !secret.exists() {
        fs::create_dir_all(&dir)?;
        let key = SecretKey::generate().map_err(|e| anyhow!("{e}"))?;
        forge_store::write_key_pair(&dir, "local", &key).map_err(|e| anyhow!("{e}"))?;
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600))?;
    }
    let trusted = cfg.trusted_keys_dir();
    fs::create_dir_all(&trusted)?;
    let pubfile = dir.join("local.pub");
    let dest = trusted.join("forge-deb-local.pub");
    if !dest.exists() {
        fs::copy(&pubfile, &dest).context("trusting the local repository key")?;
    }
    forge_store::load_secret_key(&secret).map_err(|e| anyhow!("{e}"))
}

/// Is there a forge package (glibc) of that name in any of the repositories?
pub fn has_package(repos: &[forge_store::Index], name: &str) -> bool {
    repos.iter().any(|i| i.latest(name, forge_schema::Libc::Glibc).is_some())
}

pub struct Built {
    pub forge_name: String,
    pub version: String,
    pub deb_version: String,
    pub debs: Vec<(String, String)>,
    pub left_out: Vec<(String, &'static str)>,
    pub files: usize,
    pub kept_system: usize,
    pub archive_size: u64,
    /// Forge packages whose files the result uses (the system's libraries): keep them installed.
    pub needs: Vec<String>,
    /// Programs or libraries in the result that need symbols nothing on the system provides: (file, symbols).
    pub unresolved: Vec<(String, Vec<String>)>,
}

/// Debian version -> a forge version (`-` and `:` are not allowed there).
pub fn forge_version(deb: &str) -> String {
    let v: String = deb.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '~') { c } else { '.' }).collect();
    v.chars().take(56).collect()
}

fn publish(repo: &LocalRepo, tree: &Path, name: &str, version: &str, deps: Vec<String>, work: &Path, signer: &SecretKey) -> Result<forge_store::IndexEntry> {
    fs::create_dir_all(work)?;
    let mut ver = version.to_string();
    for rev in 1..100 {
        let archive = work.join(format!("{name}-{ver}.tar.zst"));
        let packed = pack_dir(tree, &PackOptions { name: name.to_string(), version: ver.clone(), libc: forge_schema::Libc::Glibc, hash: None, deps: deps.clone() }, &archive).map_err(|e| anyhow!("{e}"))?;
        let existing = repo.index_unverified().ok().map(|i| i.packages).unwrap_or_default();
        // The same content is already there: nothing to do, unless a later revision of the package (an older build with
        // other content) is there too. The newest version is the one that gets installed, so this one would never be used.
        if let Some(e) = existing.iter().find(|e| e.name == name && e.id().ok().as_ref() == Some(&packed.id)) {
            let superseded = existing.iter().any(|x| x.name == name && forge_store::version_cmp(&x.version, &e.version) == std::cmp::Ordering::Greater);
            if !superseded {
                return Ok(e.clone());
            }
            ver = format!("{version}_r{rev}");
            continue;
        }
        // The same name and version with different content: this is a new revision.
        if existing.iter().any(|e| e.name == name && e.version == ver) {
            ver = format!("{version}_r{rev}");
            continue;
        }
        let e = repo.publish(&archive, Some(signer)).map_err(|e| anyhow!("{e}"))?;
        return Ok(e);
    }
    bail!("could not find a free revision for {name} {version}")
}

pub fn build(cfg: &Config, request: &str, forge_indexes: &[forge_store::Index], say: &dyn Fn(&str)) -> Result<Built> {
    let sources = cfg.sources()?;
    if sources.sources.is_empty() {
        bail!("no apt sources yet: add one with `forge deb source add debian` (or your own repository), then `forge deb update`");
    }
    let cache = cfg.cache();
    let pkgs = apt::load_packages(&cache, &sources)?;
    let owners = Owners::load(&cfg.root)?;
    let world = World { root: &cfg.root, owners: &owners };
    let forge_has = |name: &str| has_package(forge_indexes, name);

    say(&format!("resolving {request}"));
    let res = resolve(request, &format!("{PREFIX}{request}"), &pkgs, &world, &cache, &forge_has, say)?;
    let main: &Pkg = res.selected.first().ok_or_else(|| anyhow!("nothing selected for {request}"))?;
    let forge_name = format!("{PREFIX}{}", main.name);
    let signer = ensure_signing_key(cfg)?;
    let repo = LocalRepo::new(cfg.repo_dir());
    let work = cache.join("work");

    // deb-compat: the x86_64-linux-gnu link.
    let compat_dir = work.join("compat-tree");
    let _ = forge_store::remove_tree(&compat_dir);
    convert::compat_tree(&compat_dir)?;
    let compat = publish(&repo, &compat_dir, "deb-compat", "1", Vec::new(), &work, &signer)?;
    let compat_id = compat.id().map_err(|e| anyhow!("{e}"))?.as_str().to_string();

    let tree_dir = convert::tree_dir_for(&cache, &forge_name);
    let _ = forge_store::remove_tree(&tree_dir);
    fs::create_dir_all(&tree_dir)?;
    let mut tree = Tree::default();
    let total = res.selected.len();
    say(&format!("downloading and converting {total} Debian package(s)"));
    for (i, p) in res.selected.iter().enumerate() {
        let deb = apt::fetch_deb(&cache, p).with_context(|| format!("downloading {}", p.name))?;
        say(&format!("  [{}/{total}] {} ({})", i + 1, p.name, convert::human(p.size)));
        convert::unpack(&deb, &tree_dir, &world, &forge_name, &mut tree).with_context(|| format!("unpacking {}", p.name))?;
    }
    convert::claim_paths(&tree.written, &forge_name);
    convert::check_tree_not_empty(&tree, &forge_name)?;
    convert::alternatives_links(&tree_dir)?;
    if let Some(script) = convert::post_apply_script(&tree_dir) {
        let dir = tree_dir.join("usr/lib/forge/post-apply.d");
        fs::create_dir_all(&dir)?;
        let f = dir.join(format!("{forge_name}.sh"));
        fs::write(&f, script)?;
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755))?;
    }

    // Only the tiny compat package is pinned. Pinning system packages by exact build would break every upgrade of them
    // (a repository lists only the newest build); instead the packages that are needed are declared by name.
    let mut needs: Vec<String> = tree.deps.iter().chain(res.system_deps.iter()).filter(|n| **n != forge_name && n.as_str() != "deb-compat").cloned().collect();
    needs.sort();
    needs.dedup();
    let deps: Vec<String> = vec![compat_id];
    say("checking that every library function the programs need exists");
    let unresolved: Vec<(String, Vec<String>)> = crate::check::missing_symbols(&tree_dir, &cfg.root).into_iter().map(|p| (p.file, p.missing)).collect();
    let version = forge_version(&main.version);
    say("packing");
    let entry = publish(&repo, &tree_dir, &forge_name, &version, deps, &work, &signer)?;
    let size = fs::metadata(repo.archive_path(&entry)).map(|m| m.len()).unwrap_or(0);
    let _ = forge_store::remove_tree(&tree_dir);
    let _ = forge_store::remove_tree(&work);
    Ok(Built {
        forge_name,
        version: entry.version.clone(),
        deb_version: main.version.clone(),
        debs: res.selected.iter().map(|p| (p.name.clone(), p.version.clone())).collect(),
        left_out: res.left_out,
        files: tree.files,
        kept_system: tree.kept_system,
        archive_size: size,
        needs,
        unresolved,
    })
}

/// Add a source (or replace one of that name). `keys` are ASCII-armored or binary key files, already downloaded.
pub fn add_source(cfg: &Config, name: &str, url: &str, suite: &str, components: Vec<String>, keys: &[Vec<u8>]) -> Result<Vec<String>> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        bail!("source names are letters, digits, - and _");
    }
    let mut all = Vec::new();
    let mut fps = Vec::new();
    for k in keys {
        let parsed = apt::load_keys(k)?;
        fps.extend(apt::fingerprints(&parsed));
        all.extend_from_slice(k);
        if !k.ends_with(b"\n") {
            all.push(b'\n');
        }
    }
    let kd = cfg.keys_dir();
    fs::create_dir_all(&kd)?;
    let keyfile = format!("{name}.key");
    fs::write(kd.join(&keyfile), &all)?;
    let mut sources = cfg.sources()?;
    sources.sources.retain(|s| s.name != name);
    sources.sources.push(Source { name: name.to_string(), url: url.to_string(), suite: suite.to_string(), components, key: keyfile });
    sources.save(&cfg.sources_file())?;
    Ok(fps)
}
