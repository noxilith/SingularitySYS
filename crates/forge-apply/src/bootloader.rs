//! Keeping the boot loader current. `forge strap` installs Limine's files onto the boot partition (and, on a BIOS
//! machine, its boot sector onto the disk) and records how in `/etc/forge/boot.toml`; when the `limine` package changes,
//! `forge apply` puts the new files there too. Without this the boot loader would stay at the version that was installed.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::{Change, Kind, Op, Planner, under};

/// `/etc/forge/boot.toml`.
#[derive(Debug, Deserialize)]
pub struct BootRecord {
    /// `uefi` or `bios`.
    pub firmware: String,
    /// BIOS only: the disk Limine's boot sector was written to.
    #[serde(default)]
    pub disk: Option<String>,
}

pub(crate) fn plan(p: &mut Planner, limine_installed: bool, packages_changing: bool) {
    if !limine_installed || !under(p.root, "/boot").is_dir() {
        return;
    }
    let boot = under(p.root, "/boot");
    let share = under(p.root, "/usr/share/limine");
    let record: Option<BootRecord> = fs::read_to_string(under(p.root, "/etc/forge/boot.toml")).ok().and_then(|t| toml::from_str(&t).ok());

    // (the file in the package, where it is on the boot partition)
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let uefi_dest = boot.join("EFI/BOOT/BOOTX64.EFI");
    let bios_dest = boot.join("limine-bios.sys");
    let firmware = match record.as_ref().map(|r| r.firmware.as_str()) {
        Some(f) => f.to_string(),
        None if uefi_dest.exists() => "uefi".into(),
        None if bios_dest.exists() => "bios".into(),
        None => return,
    };
    // Secure Boot set up: the loader on the boot partition is the signed one, with the hash of limine.conf inside. The
    // package's copy must never be put over it; instead, whenever packages change, `forge secureboot refresh` makes the
    // signed loader from the package's new copy and pins the boot files again.
    if firmware == "uefi" && under(p.root, "/etc/forge/secureboot/enabled").is_file() {
        if packages_changing && p.root == std::path::Path::new("/") {
            p.changes.push(Change {
                section: "boot",
                kind: Kind::Modify,
                summary: "the signed boot loader and the pinned boot menu (Secure Boot)".into(),
                op: Op::Run { program: under(p.root, "/usr/bin/forge"), args: vec!["secureboot".into(), "refresh".into()], best_effort: false },
            });
        }
        return;
    }
    if firmware == "uefi" {
        files.push((share.join("BOOTX64.EFI"), uefi_dest));
    } else {
        files.push((share.join("limine-bios.sys"), bios_dest));
    }

    let mut changed = false;
    for (src, dest) in files {
        let same = matches!((fs::read(&src), fs::read(&dest)), (Ok(a), Ok(b)) if a == b);
        if (src.is_file() && !same) || (packages_changing && !src.is_file()) {
            changed = true;
            p.changes.push(Change {
                section: "boot",
                kind: if dest.exists() { Kind::Modify } else { Kind::Add },
                summary: format!("{} (the Limine boot loader, from the limine package)", dest.strip_prefix(p.root).map(|r| format!("/{}", r.display())).unwrap_or_else(|_| dest.display().to_string())),
                op: Op::CopyFile { from: src, to: dest, optional: true },
            });
        }
    }
    // The BIOS boot sector must match limine-bios.sys: write it again whenever the file changes.
    if changed && firmware == "bios" && p.root == std::path::Path::new("/") {
        if let Some(disk) = record.and_then(|r| r.disk) {
            p.changes.push(Change {
                section: "boot",
                kind: Kind::Modify,
                summary: format!("Limine's boot sector on {disk}"),
                op: Op::Run { program: under(p.root, "/usr/bin/limine"), args: vec!["bios-install".into(), disk], best_effort: false },
            });
        }
    }
}
