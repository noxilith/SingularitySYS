//! Secure Boot with a key of your own. forge makes the key, signs the boot loader with it, pins the files the loader
//! may load by hash, and (in the firmware's setup mode) tells the firmware to trust the key: no one else's signature
//! is involved. See [`BootSetup`] for what is kept where.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use blake2::{Blake2b512, Digest};

mod cms;
mod der;
pub mod efi;
mod keys;
pub mod pe;

pub use keys::{CERT_FILE, Identity, KEY_FILE};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{0}")]
    Crypto(String),
    #[error("{0}")]
    Format(String),
    #[error("this machine did not start through UEFI (no efivarfs): Secure Boot needs UEFI")]
    NoEfi,
    #[error("the firmware is not in setup mode, so it will not accept a new key from here; clear the Secure Boot keys in the firmware's settings (\"setup mode\", \"custom mode\", \"delete all keys\") and run this again, or enroll /boot/EFI/forge-secureboot/ from the firmware menu")]
    NotSetupMode,
    #[error("`{0}` failed")]
    Command(String),
}

impl Error {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io { path: path.to_path_buf(), source }
    }
}

/// BLAKE2b-512 as 128 hex digits: what Limine's `#hash` suffix and its enrolled config hash use.
pub fn blake2b_hex(data: &[u8]) -> String {
    Blake2b512::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn blake2b_file(path: &Path) -> Result<String, Error> {
    Ok(blake2b_hex(&fs::read(path).map_err(|e| Error::io(path, e))?))
}

/// Where everything lives. The defaults are for an installed system.
#[derive(Debug, Clone)]
pub struct BootSetup {
    /// The key and certificate, and the marker that says Secure Boot is set up (`enabled`).
    pub dir: PathBuf,
    /// The boot partition.
    pub boot: PathBuf,
    /// The `limine` program (it writes the config hash into the boot loader).
    pub limine: PathBuf,
    /// The boot loader as the package ships it, unsigned.
    pub loader_source: PathBuf,
    pub efivars: PathBuf,
}

impl Default for BootSetup {
    fn default() -> Self {
        BootSetup {
            dir: PathBuf::from("/etc/forge/secureboot"),
            boot: PathBuf::from("/boot"),
            limine: PathBuf::from("/usr/bin/limine"),
            loader_source: PathBuf::from("/usr/share/limine/BOOTX64.EFI"),
            efivars: PathBuf::from("/sys/firmware/efi/efivars"),
        }
    }
}

impl BootSetup {
    pub fn marker(&self) -> PathBuf {
        self.dir.join("enabled")
    }

    pub fn is_enabled(&self) -> bool {
        self.marker().is_file()
    }

    pub fn loader(&self) -> PathBuf {
        self.boot.join("EFI/BOOT/BOOTX64.EFI")
    }

    /// Where the files for enrolling by hand (firmware menu) are written.
    pub fn export_dir(&self) -> PathBuf {
        self.boot.join("EFI/forge-secureboot")
    }

    /// Create the key if there is none; returns whether one was made.
    pub fn ensure_identity(&self, host: &str) -> Result<(Identity, bool), Error> {
        if self.dir.join(KEY_FILE).is_file() {
            return Ok((Identity::load(&self.dir)?, false));
        }
        let id = Identity::generate(&format!("SingularitySYS Secure Boot key ({host})"))?;
        id.save(&self.dir)?;
        Ok((id, true))
    }

    /// The setup for a boot directory (tests and `--boot-dir` use a directory that is not `/boot`). The key directory can
    /// be moved with `FORGE_SECUREBOOT_DIR`.
    pub fn for_boot(boot: &Path) -> Self {
        let mut s = BootSetup { boot: boot.to_path_buf(), ..BootSetup::default() };
        if let Some(d) = std::env::var_os("FORGE_SECUREBOOT_DIR") {
            s.dir = PathBuf::from(d);
        }
        s
    }

    /// The signed boot loader for a `limine.conf` with this BLAKE2b hash, written next to where it goes: the loader
    /// refuses any configuration but the one whose hash is inside it. [`BootSetup::commit_loader`] moves it into place.
    pub fn prepare_loader(&self, conf_hash: &str) -> Result<PathBuf, Error> {
        let id = Identity::load(&self.dir)?;
        let dest = self.loader();
        let tmp = dest.with_extension("EFI.new");
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        fs::copy(&self.loader_source, &tmp).map_err(|e| Error::io(&self.loader_source, e))?;
        let status = Command::new(&self.limine).arg("enroll-config").arg(&tmp).arg(conf_hash).arg("--quiet").status().map_err(|e| Error::io(&self.limine, e))?;
        if !status.success() {
            let _ = fs::remove_file(&tmp);
            return Err(Error::Command(format!("{} enroll-config", self.limine.display())));
        }
        let signed = pe::sign(&fs::read(&tmp).map_err(|e| Error::io(&tmp, e))?, &id)?;
        fs::write(&tmp, signed).map_err(|e| Error::io(&tmp, e))?;
        Ok(tmp)
    }

    pub fn commit_loader(&self, prepared: &Path) -> Result<(), Error> {
        fs::rename(prepared, self.loader()).map_err(|e| Error::io(&self.loader(), e))
    }

    /// Sign the boot loader for the `limine.conf` that is there now.
    pub fn sign_loader(&self) -> Result<(), Error> {
        let hash = blake2b_file(&self.boot.join("limine.conf"))?;
        let tmp = self.prepare_loader(&hash)?;
        self.commit_loader(&tmp)
    }

    /// Files to enroll from the firmware's own menu (when setup mode cannot be entered from here).
    pub fn export(&self, unix: u64) -> Result<PathBuf, Error> {
        let id = Identity::load(&self.dir)?;
        let dir = self.export_dir();
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let list = efi::esl(&id.cert);
        let w = |name: &str, bytes: &[u8]| fs::write(dir.join(name), bytes).map_err(|e| Error::io(&dir.join(name), e));
        w("singularity.cer", &id.cert)?;
        w("db.esl", &list)?;
        w("db.auth", &efi::authenticated(&id, "db", efi::DB_GUID, &list, unix)?)?;
        w("KEK.auth", &efi::authenticated(&id, "KEK", efi::GLOBAL_GUID, &list, unix)?)?;
        w("PK.auth", &efi::authenticated(&id, "PK", efi::GLOBAL_GUID, &list, unix)?)?;
        Ok(dir)
    }

    pub fn enroll(&self, unix: u64) -> Result<(), Error> {
        efi::enroll(&Identity::load(&self.dir)?, &self.efivars, unix)
    }

    /// What is true now, for `forge secureboot status`.
    pub fn status(&self) -> Status {
        let id = Identity::load(&self.dir).ok();
        let loader = fs::read(self.loader()).ok();
        Status {
            uefi: self.efivars.is_dir(),
            key: id.is_some(),
            enabled: self.is_enabled(),
            loader_signed: matches!((&id, &loader), (Some(i), Some(l)) if pe::signed_by(l, &i.cert)),
            setup_mode: efi::setup_mode(&self.efivars),
            secure_boot: efi::secure_boot_on(&self.efivars),
        }
    }
}

#[derive(Debug)]
pub struct Status {
    pub uefi: bool,
    pub key: bool,
    pub enabled: bool,
    pub loader_signed: bool,
    pub setup_mode: Option<bool>,
    pub secure_boot: Option<bool>,
}

#[cfg(test)]
pub fn cms_test_identity() -> &'static Identity {
    static ID: std::sync::OnceLock<Identity> = std::sync::OnceLock::new();
    ID.get_or_init(|| Identity::generate("forge test").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake2b_is_128_hex_digits_and_matches_the_reference() {
        assert_eq!(blake2b_hex(b"").len(), 128);
        assert!(blake2b_hex(b"").starts_with("786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419"));
    }
}
