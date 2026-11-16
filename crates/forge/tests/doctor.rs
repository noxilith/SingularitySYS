//! `forge doctor` against scratch system roots.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn doctor(root: &Path, base: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forge")).args(["doctor", "--root", root.to_str().unwrap()]).env("FORGE_BASE_PACKAGES", base).output().expect("run forge")
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// A system that is as it should be: OpenRC as init, one Limine entry whose files exist, a valid declaration.
fn healthy() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().to_path_buf();
    for d in ["usr/sbin", "boot", "etc/forge", "etc/init.d", "etc/runlevels/default"] {
        fs::create_dir_all(r.join(d)).unwrap();
    }
    fs::write(r.join("usr/sbin/openrc-init"), "x").unwrap();
    symlink("openrc-init", r.join("usr/sbin/init")).unwrap();
    fs::write(r.join("boot/vmlinuz-1"), "k").unwrap();
    fs::write(r.join("boot/limine.conf"), "timeout: 5\n/Singularity\n    protocol: linux\n    path: boot():/vmlinuz-1\n    cmdline: root=x\n").unwrap();
    fs::write(r.join("etc/forge/system.toml"), "hostname = \"nova\"\nlibc = \"glibc\"\n").unwrap();
    fs::write(r.join("etc/hostname"), "nova\n").unwrap();
    fs::write(r.join("etc/init.d/sshd"), "#!/sbin/openrc-run\n").unwrap();
    symlink("/etc/init.d/sshd", r.join("etc/runlevels/default/sshd")).unwrap();
    (t, r)
}

#[test]
fn a_healthy_system_passes() {
    let (_t, r) = healthy();
    let o = doctor(&r, "");
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("starts OpenRC") && t.contains("every kernel file they name is there") && t.contains("1 enabled services"), "{t}");
}

#[test]
fn busybox_init_is_a_problem_that_stops_the_system() {
    let (_t, r) = healthy();
    fs::remove_file(r.join("usr/sbin/init")).unwrap();
    symlink("busybox", r.join("usr/sbin/init")).unwrap();
    let o = doctor(&r, "");
    assert!(!o.status.success());
    assert!(text(&o).contains("can't run /etc/init.d/rcS") && text(&o).contains("run: sudo forge apply"), "{}", text(&o));
}

#[test]
fn a_dangling_init_link_and_a_missing_init_are_problems() {
    let (_t, r) = healthy();
    fs::remove_file(r.join("usr/sbin/openrc-init")).unwrap();
    assert!(text(&doctor(&r, "")).contains("is not installed"));
    fs::remove_file(r.join("usr/sbin/init")).unwrap();
    let o = doctor(&r, "");
    assert!(!o.status.success() && text(&o).contains("no /sbin/init"), "{}", text(&o));
}

#[test]
fn the_declared_init_system_must_match_the_link() {
    let (_t, r) = healthy();
    fs::write(r.join("etc/forge/system.toml"), "hostname = \"nova\"\nlibc = \"glibc\"\ninit = \"systemd\"\n").unwrap();
    let o = doctor(&r, "");
    assert!(o.status.success(), "a pending switch is a warning, not a failure");
    assert!(text(&o).contains("something other than the declared init system"), "{}", text(&o));
}

#[test]
fn limine_naming_a_missing_file_is_a_problem() {
    let (_t, r) = healthy();
    fs::remove_file(r.join("boot/vmlinuz-1")).unwrap();
    let o = doctor(&r, "");
    assert!(!o.status.success() && text(&o).contains("vmlinuz-1"), "{}", text(&o));
}

#[test]
fn an_enabled_service_that_is_not_installed_is_a_warning() {
    let (_t, r) = healthy();
    fs::remove_file(r.join("etc/init.d/sshd")).unwrap();
    let o = doctor(&r, "");
    assert!(o.status.success() && text(&o).contains("enabled but not installed: sshd"), "{}", text(&o));
}

#[test]
fn missing_base_packages_are_named() {
    let (_t, r) = healthy();
    let ids: BTreeSet<_> = ["glibc", "openrc", "chrony"].iter().map(|n| forge_store::StoreId::new(n, "1", &"ab".repeat(32)).unwrap()).collect();
    forge_store::Profile::new(r.join("forge/profiles/system")).record(&ids, 1).unwrap();
    let o = doctor(&r, "glibc,coreutils");
    assert!(!o.status.success() && text(&o).contains("base packages are missing: coreutils"), "{}", text(&o));
    let o = doctor(&r, "glibc");
    assert!(o.status.success() && text(&o).contains("the base system is complete"), "{}", text(&o));
}

#[test]
fn a_declared_user_that_does_not_exist_is_a_warning() {
    let (_t, r) = healthy();
    fs::write(r.join("etc/forge/system.toml"), "hostname = \"nova\"\nlibc = \"glibc\"\n[[users]]\nname = \"ada\"\n").unwrap();
    let o = doctor(&r, "");
    assert!(o.status.success() && text(&o).contains("user ada is declared but does not exist"), "{}", text(&o));
}

#[test]
fn an_invalid_declaration_is_a_problem() {
    let (_t, r) = healthy();
    fs::write(r.join("etc/forge/system.toml"), "this is not toml").unwrap();
    let o = doctor(&r, "");
    assert!(!o.status.success() && text(&o).contains("is not valid"), "{}", text(&o));
}

#[test]
fn a_microcode_image_that_is_not_in_boot_or_not_loaded_is_a_warning() {
    let (_t, r) = healthy();
    fs::create_dir_all(r.join("usr/lib/microcode")).unwrap();
    fs::write(r.join("usr/lib/microcode/early.cpio"), "IMG").unwrap();
    fs::write(r.join("usr/lib/microcode/early-amd.cpio"), "IMG").unwrap();
    fs::write(r.join("usr/lib/microcode/early-intel.cpio"), "IMG").unwrap();
    let o = doctor(&r, "");
    assert!(o.status.success() && text(&o).contains("microcode.img is missing or old"), "{}", text(&o));
    fs::write(r.join("boot/microcode.img"), "IMG").unwrap();
    assert!(text(&doctor(&r, "")).contains("no Limine entry loads it"));
    let conf = fs::read_to_string(r.join("boot/limine.conf")).unwrap() + "    module_path: boot():/microcode.img\n";
    fs::write(r.join("boot/limine.conf"), conf).unwrap();
    assert!(text(&doctor(&r, "")).contains("loaded at boot"));
}

#[test]
fn stale_forge_owned_files_are_a_warning() {
    let (_t, r) = healthy();
    fs::create_dir_all(r.join("usr/bin")).unwrap();
    fs::write(r.join("usr/bin/wifi"), "an old version").unwrap();
    let o = doctor(&r, "");
    assert!(o.status.success() && text(&o).contains("out of date:") && text(&o).contains("/usr/bin/wifi"), "{}", text(&o));
}

#[test]
fn an_old_boot_loader_is_a_warning() {
    let (_t, r) = healthy();
    fs::create_dir_all(r.join("boot/EFI/BOOT")).unwrap();
    fs::create_dir_all(r.join("usr/share/limine")).unwrap();
    fs::write(r.join("boot/EFI/BOOT/BOOTX64.EFI"), "OLD").unwrap();
    fs::write(r.join("usr/share/limine/BOOTX64.EFI"), "NEW").unwrap();
    let o = doctor(&r, "");
    assert!(o.status.success() && text(&o).contains("is older than the limine package"), "{}", text(&o));
    fs::write(r.join("boot/EFI/BOOT/BOOTX64.EFI"), "NEW").unwrap();
    assert!(text(&doctor(&r, "")).contains("is the one in the limine package"));
}

#[test]
fn pinned_secure_boot_paths_and_the_signed_loader_are_not_problems() {
    let (_t, r) = healthy();
    let hash = "ab".repeat(64);
    fs::write(r.join("boot/limine.conf"), format!("timeout: 5\n/Singularity\n    protocol: linux\n    path: boot():/vmlinuz-1#{hash}\n    cmdline: root=x\n")).unwrap();
    // A signed loader differs from the package's copy by design.
    for d in ["boot/EFI/BOOT", "usr/share/limine", "etc/forge/secureboot"] {
        fs::create_dir_all(r.join(d)).unwrap();
    }
    fs::write(r.join("boot/EFI/BOOT/BOOTX64.EFI"), "signed").unwrap();
    fs::write(r.join("usr/share/limine/BOOTX64.EFI"), "plain").unwrap();
    fs::write(r.join("etc/forge/secureboot/enabled"), "").unwrap();
    let t = text(&doctor(&r, ""));
    assert!(!t.contains("not in /boot") && !t.contains("older than the limine package"), "{t}");
    assert!(t.contains("every kernel file they name is there"), "{t}");
}

#[test]
fn the_declared_locale_and_keyboard_layout_must_exist() {
    let (_t, r) = healthy();
    fs::create_dir_all(r.join("usr/lib/locale/en_US.utf8")).unwrap();
    fs::create_dir_all(r.join("usr/share/keymaps/i386/qwerty")).unwrap();
    fs::write(r.join("usr/share/keymaps/i386/qwerty/us.map.gz"), "x").unwrap();
    let t = text(&doctor(&r, ""));
    assert!(t.contains("the locale en_US.UTF-8 is installed") && t.contains("the keyboard layout us is installed"), "{t}");
    fs::write(r.join("etc/forge/system.toml"), "hostname = \"nova\"\nlibc = \"glibc\"\nlocale = \"de_DE.UTF-8\"\nkeymap = \"de\"\n").unwrap();
    let t = text(&doctor(&r, ""));
    assert!(t.contains("the locale de_DE.UTF-8 is not installed") && t.contains("no console keyboard layout called de"), "{t}");
}
