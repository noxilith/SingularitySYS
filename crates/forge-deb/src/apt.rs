//! apt repositories: the list of sources, downloading and checking the indexes, reading the package lists.
//!
//! Trust works as apt's does: the `InRelease` (or `Release` + `Release.gpg`) file is signed by the repository's key,
//! which the user hands over once (`forge deb source add --key`); it lists the SHA-256 of every `Packages` file,
//! and each `Packages` entry lists the SHA-256 of its `.deb`. Nothing is used unless every link of that chain checks.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::control::{Stanza, parse_depends, parse_stanzas};

pub const ARCH: &str = "amd64";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub url: String,
    pub suite: String,
    /// Empty for a "flat" repository (`suite` ends with `/`, the lists sit next to `Release`).
    #[serde(default)]
    pub components: Vec<String>,
    /// File name of the signing key(s) in the keys directory.
    pub key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sources {
    #[serde(default, rename = "source")]
    pub sources: Vec<Source>,
}

impl Sources {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(path, toml::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.name == name)
    }
}

/// One package of a repository's list.
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub version: String,
    pub arch: String,
    pub depends: Vec<Vec<String>>,
    pub provides: Vec<String>,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
    pub description: String,
    pub source: String,
    pub base_url: String,
}

impl Pkg {
    fn from_stanza(s: &Stanza, source: &Source) -> Option<Pkg> {
        let name = s.get("package")?.clone();
        let arch = s.get("architecture")?.clone();
        if arch != ARCH && arch != "all" {
            return None;
        }
        let mut deps = parse_depends(s.get("pre-depends").map(String::as_str).unwrap_or(""));
        deps.extend(parse_depends(s.get("depends").map(String::as_str).unwrap_or("")));
        let provides = parse_depends(s.get("provides").map(String::as_str).unwrap_or("")).into_iter().flatten().collect();
        Some(Pkg {
            name,
            version: s.get("version")?.clone(),
            arch,
            depends: deps,
            provides,
            filename: s.get("filename")?.clone(),
            sha256: s.get("sha256")?.clone(),
            size: s.get("size").and_then(|v| v.parse().ok()).unwrap_or(0),
            description: s.get("description").map(|d| d.lines().next().unwrap_or("").to_string()).unwrap_or_default(),
            source: source.name.clone(),
            base_url: source.url.trim_end_matches('/').to_string(),
        })
    }

    pub fn url(&self) -> String {
        format!("{}/{}", self.base_url, self.filename)
    }
}

// ---- downloads -------------------------------------------------------------------

/// Download `url` to `dest` with curl (the base system ships it with the CA certificates).
pub fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(d) = dest.parent() {
        fs::create_dir_all(d)?;
    }
    let part = dest.with_extension("part");
    let st = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "--connect-timeout", "20", "-o"])
        .arg(&part)
        .arg(url)
        .status()
        .context("running curl (is it installed?)")?;
    if !st.success() {
        let _ = fs::remove_file(&part);
        bail!("could not download {url}");
    }
    fs::rename(&part, dest)?;
    Ok(())
}

fn fetch_text(url: &str, scratch: &Path) -> Result<Option<String>> {
    let tmp = scratch.join("fetch.tmp");
    let _ = fs::remove_file(&tmp);
    let ok = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .context("running curl (is it installed?)")?
        .success();
    if !ok {
        return Ok(None);
    }
    let bytes = fs::read(&tmp)?;
    let _ = fs::remove_file(&tmp);
    Ok(Some(String::from_utf8(bytes).map_err(|_| anyhow!("{url} is not text"))?))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---- signatures ------------------------------------------------------------------

mod sig {
    use anyhow::{Result, anyhow};
    use pgp::composed::{CleartextSignedMessage, Deserializable, SignedPublicKey, StandaloneSignature};

    /// Every public key in a key file (ASCII-armored or binary, one or many keys).
    pub fn load_keys(bytes: &[u8]) -> Result<Vec<SignedPublicKey>> {
        let text = std::str::from_utf8(bytes).ok().filter(|t| t.contains("-----BEGIN PGP PUBLIC KEY BLOCK-----"));
        let mut keys = Vec::new();
        if let Some(t) = text {
            let (iter, _) = SignedPublicKey::from_armor_many(std::io::Cursor::new(t.as_bytes())).map_err(|e| anyhow!("reading the key: {e}"))?;
            for k in iter {
                keys.push(k.map_err(|e| anyhow!("reading the key: {e}"))?);
            }
        } else {
            for k in SignedPublicKey::from_bytes_many(std::io::Cursor::new(bytes)).map_err(|e| anyhow!("reading the key: {e}"))? {
                keys.push(k.map_err(|e| anyhow!("reading the key: {e}"))?);
            }
        }
        if keys.is_empty() {
            return Err(anyhow!("the key file holds no public key"));
        }
        Ok(keys)
    }

    /// `InRelease`: a clear-signed message. Returns the signed text.
    pub fn verify_inrelease(text: &str, keys: &[SignedPublicKey]) -> Result<String> {
        let (msg, _) = CleartextSignedMessage::from_string(text).map_err(|e| anyhow!("reading InRelease: {e}"))?;
        for k in keys {
            if msg.verify(k).is_ok() || k.public_subkeys.iter().any(|s| msg.verify(s).is_ok()) {
                return Ok(msg.text().to_string());
            }
        }
        Err(anyhow!("the signature on InRelease does not match the repository key"))
    }

    /// `Release` with a detached `Release.gpg`.
    pub fn verify_detached(data: &[u8], sig: &[u8], keys: &[SignedPublicKey]) -> Result<()> {
        let sig = if sig.starts_with(b"-----BEGIN") {
            StandaloneSignature::from_armor_single(std::io::Cursor::new(sig)).map_err(|e| anyhow!("reading Release.gpg: {e}"))?.0
        } else {
            StandaloneSignature::from_bytes(std::io::Cursor::new(sig)).map_err(|e| anyhow!("reading Release.gpg: {e}"))?
        };
        for k in keys {
            if sig.verify(k, data).is_ok() || k.public_subkeys.iter().any(|s| sig.verify(s, data).is_ok()) {
                return Ok(());
            }
        }
        Err(anyhow!("the signature on Release does not match the repository key"))
    }

    /// A short description of each key: fingerprint of the primary key.
    pub fn fingerprints(keys: &[SignedPublicKey]) -> Vec<String> {
        use pgp::types::KeyDetails;
        keys.iter().map(|k| k.fingerprint().as_bytes().iter().map(|b| format!("{b:02X}")).collect()).collect()
    }
}

pub use sig::fingerprints;
pub use sig::load_keys;

// ---- one repository ----------------------------------------------------------------

fn dists_url(s: &Source) -> String {
    let base = s.url.trim_end_matches('/');
    if s.suite.ends_with('/') {
        format!("{base}/{}", s.suite.trim_start_matches("./").trim_end_matches('/'))
            .trim_end_matches('/')
            .to_string()
    } else {
        format!("{base}/dists/{}", s.suite)
    }
}

fn decompress(name: &str, data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if name.ends_with(".xz") {
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(data), &mut out).map_err(|e| anyhow!("decompressing {name}: {e:?}"))?;
    } else if name.ends_with(".gz") {
        flate2::read::GzDecoder::new(data).read_to_end(&mut out)?;
    } else if name.ends_with(".bz2") {
        bzip2::read::BzDecoder::new(data).read_to_end(&mut out)?;
    } else {
        out = data.to_vec();
    }
    Ok(out)
}

/// Where the cached, checked `Packages` text of a repository's component is kept.
pub fn list_path(cache: &Path, source: &Source, component: &str) -> PathBuf {
    let c = if component.is_empty() { "flat" } else { component };
    cache.join("lists").join(&source.name).join(format!("{c}.Packages"))
}

/// Download and check the indexes of `source` into `cache`. Returns the number of packages.
pub fn update_source(cache: &Path, keys_dir: &Path, source: &Source) -> Result<usize> {
    let scratch = cache.join("tmp");
    fs::create_dir_all(&scratch)?;
    let keyfile = keys_dir.join(&source.key);
    let keys = sig::load_keys(&fs::read(&keyfile).with_context(|| format!("reading the key {}", keyfile.display()))?)?;
    let base = dists_url(source);

    let release_text = match fetch_text(&format!("{base}/InRelease"), &scratch)? {
        Some(t) => sig::verify_inrelease(&t, &keys).with_context(|| format!("checking {base}/InRelease"))?,
        None => {
            let r = fetch_text(&format!("{base}/Release"), &scratch)?.ok_or_else(|| anyhow!("{base} has neither InRelease nor Release"))?;
            let sigtext = {
                let tmp = scratch.join("sig.tmp");
                download(&format!("{base}/Release.gpg"), &tmp).with_context(|| format!("{base}/Release is not signed"))?;
                let b = fs::read(&tmp)?;
                let _ = fs::remove_file(&tmp);
                b
            };
            sig::verify_detached(r.as_bytes(), &sigtext, &keys).with_context(|| format!("checking {base}/Release"))?;
            r
        }
    };

    let stanza = parse_stanzas(&release_text).into_iter().next().ok_or_else(|| anyhow!("{base}: empty Release file"))?;
    check_valid_until(&stanza, &base)?;
    // path -> (sha256, size)
    let mut files: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for line in stanza.get("sha256").map(String::as_str).unwrap_or("").lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() == 3 {
            files.insert(f[2].to_string(), (f[0].to_string(), f[1].parse().unwrap_or(0)));
        }
    }
    if files.is_empty() {
        bail!("{base}: the Release file lists no SHA256 sums");
    }

    let comps: Vec<String> = if source.components.is_empty() { vec![String::new()] } else { source.components.clone() };
    let mut total = 0;
    for comp in comps {
        let rel = |ext: &str| if comp.is_empty() { format!("Packages{ext}") } else { format!("{comp}/binary-{ARCH}/Packages{ext}") };
        let pick = [".xz", ".gz", ""].into_iter().map(rel).find(|p| files.contains_key(p));
        let Some(path) = pick else { bail!("{base} lists no Packages file for {}", if comp.is_empty() { "this repository" } else { &comp }) };
        let (sha, size) = files[&path].clone();
        let tmp = scratch.join("packages.tmp");
        download(&format!("{base}/{path}"), &tmp)?;
        let got = sha256_file(&tmp)?;
        if got != sha || (size != 0 && fs::metadata(&tmp)?.len() != size) {
            let _ = fs::remove_file(&tmp);
            bail!("{path} does not match the checksum in the signed Release file");
        }
        let text = decompress(&path, &fs::read(&tmp)?)?;
        let _ = fs::remove_file(&tmp);
        let dest = list_path(cache, source, &comp);
        if let Some(d) = dest.parent() {
            fs::create_dir_all(d)?;
        }
        total += parse_stanzas(&String::from_utf8_lossy(&text)).len();
        fs::write(&dest, text)?;
    }
    Ok(total)
}

/// Every package of the cached lists, newest version of each name per source first.
pub fn load_packages(cache: &Path, sources: &Sources) -> Result<Vec<Pkg>> {
    let mut out = Vec::new();
    for s in &sources.sources {
        let comps: Vec<String> = if s.components.is_empty() { vec![String::new()] } else { s.components.clone() };
        for c in comps {
            let p = list_path(cache, s, &c);
            let text = match fs::read_to_string(&p) {
                Ok(t) => t,
                Err(_) => bail!("no package list for {} yet: run `forge deb update`", s.name),
            };
            for st in parse_stanzas(&text) {
                if let Some(p) = Pkg::from_stanza(&st, s) {
                    out.push(p);
                }
            }
        }
    }
    Ok(out)
}

/// Download a package into the cache and check its SHA-256 against the signed list.
pub fn fetch_deb(cache: &Path, pkg: &Pkg) -> Result<PathBuf> {
    let name = Path::new(&pkg.filename).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("{}.deb", pkg.name));
    let dest = cache.join("debs").join(&pkg.source).join(name);
    if dest.exists() && sha256_file(&dest).ok().as_deref() == Some(pkg.sha256.as_str()) {
        return Ok(dest);
    }
    download(&pkg.url(), &dest)?;
    let got = sha256_file(&dest)?;
    if got != pkg.sha256 {
        let _ = fs::remove_file(&dest);
        bail!("{} does not match the checksum in the signed package list", pkg.filename);
    }
    Ok(dest)
}

/// Debian version comparison (epoch, upstream, revision with `~` sorting before everything).
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn split(v: &str) -> (u64, &str, &str) {
        let (epoch, rest) = match v.split_once(':') {
            Some((e, r)) if e.chars().all(|c| c.is_ascii_digit()) => (e.parse().unwrap_or(0), r),
            _ => (0, v),
        };
        match rest.rsplit_once('-') {
            Some((u, r)) => (epoch, u, r),
            None => (epoch, rest, ""),
        }
    }
    fn order(c: Option<u8>) -> i32 {
        match c {
            None => 0,
            Some(b'~') => -1,
            Some(c) if c.is_ascii_alphabetic() => c as i32,
            Some(c) => c as i32 + 256,
        }
    }
    fn part(a: &str, b: &str) -> Ordering {
        let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
        loop {
            // non-digit prefix
            let na = a.iter().take_while(|c| !c.is_ascii_digit()).count();
            let nb = b.iter().take_while(|c| !c.is_ascii_digit()).count();
            let (pa, pb) = (&a[..na], &b[..nb]);
            for i in 0..pa.len().max(pb.len()) {
                let (x, y) = (order(pa.get(i).copied()), order(pb.get(i).copied()));
                if x != y {
                    return x.cmp(&y);
                }
            }
            a = &a[na..];
            b = &b[nb..];
            let da = a.iter().take_while(|c| c.is_ascii_digit()).count();
            let db = b.iter().take_while(|c| c.is_ascii_digit()).count();
            let va: u128 = std::str::from_utf8(&a[..da]).unwrap_or("").parse().unwrap_or(0);
            let vb: u128 = std::str::from_utf8(&b[..db]).unwrap_or("").parse().unwrap_or(0);
            if va != vb {
                return va.cmp(&vb);
            }
            a = &a[da..];
            b = &b[db..];
            if a.is_empty() && b.is_empty() {
                return Ordering::Equal;
            }
        }
    }
    let (ea, ua, ra) = split(a);
    let (eb, ub, rb) = split(b);
    ea.cmp(&eb).then_with(|| part(ua, ub)).then_with(|| part(ra, rb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn debian_versions_sort() {
        assert_eq!(version_cmp("1.0-1", "1.0-2"), Less);
        assert_eq!(version_cmp("1.0~rc1-1", "1.0-1"), Less);
        assert_eq!(version_cmp("1:0.9-1", "2.0-1"), Greater);
        assert_eq!(version_cmp("1.10", "1.9"), Greater);
        assert_eq!(version_cmp("2.41-12", "2.41-12"), Equal);
    }

    #[test]
    fn flat_and_normal_dists_urls() {
        let mut s = Source { name: "x".into(), url: "https://h/r/".into(), suite: "stable".into(), components: vec!["main".into()], key: "k".into() };
        assert_eq!(dists_url(&s), "https://h/r/dists/stable");
        s.suite = "./".into();
        assert_eq!(dists_url(&s), "https://h/r");
    }
}

/// Seconds since the epoch for `Thu, 26 Nov 2026 17:17:13 UTC` (the form `Valid-Until` has).
fn parse_release_date(s: &str) -> Option<i64> {
    let f: Vec<&str> = s.split_whitespace().collect();
    let f = if f.first().is_some_and(|x| x.ends_with(',')) { &f[1..] } else { &f[..] };
    if f.len() < 4 {
        return None;
    }
    let day: i64 = f[0].parse().ok()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == f[1])? as i64 + 1;
    let year: i64 = f[2].parse().ok()?;
    let t: Vec<i64> = f[3].split(':').filter_map(|x| x.parse().ok()).collect();
    if t.len() != 3 {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + t[0] * 3600 + t[1] * 60 + t[2])
}

/// A signed Release file says until when it may be believed; an old copy replayed by an attacker is refused.
fn check_valid_until(stanza: &Stanza, base: &str) -> Result<()> {
    let Some(v) = stanza.get("valid-until") else { return Ok(()) };
    let Some(until) = parse_release_date(v) else { return Ok(()) };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    if now > until {
        bail!("{base}: the signed Release file expired ({v}); check this computer's clock, or the repository is no longer kept up to date");
    }
    Ok(())
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn release_dates_parse() {
        assert_eq!(parse_release_date("Thu, 01 Jan 1970 00:00:00 UTC"), Some(0));
        assert_eq!(parse_release_date("Sun, 09 Sep 2001 01:46:40 UTC"), Some(1_000_000_000));
    }
}
