//! The files that make a tree a live system: the initramfs `/init`, the live root's
//! `/etc`, and the Limine configuration.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use crate::{IsoError, io_ctx};

/// Where the squashfs sits on the medium (also what `/init` looks for).
pub const SQUASHFS_PATH: &str = "live/rootfs.squashfs";

/// Applets the initramfs needs, linked to the BusyBox binary.
pub const INITRAMFS_APPLETS: &[&str] =
    &["sh", "mount", "umount", "mkdir", "sleep", "switch_root", "echo", "cp", "rm", "ls", "cat", "dmesg", "losetup", "insmod", "modprobe"];

/// The initramfs `/init`: find the medium that holds the squashfs, stack a writable
/// tmpfs over it with overlayfs, and hand over to the live system's init.
pub fn init_script(label: &str) -> String {
    format!(
        r#"#!/bin/sh
# SingularitySYS live initramfs: find the medium, mount the system, switch to it.
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
echo "singularity: looking for the live medium ({label})"

rescue() {{
    echo "singularity: $1"
    echo "singularity: dropping to a shell; exit to retry"
    sh
}}

find_medium() {{
    for dev in /dev/sr* /dev/vd* /dev/sd* /dev/nvme*n* /dev/mmcblk*; do
        [ -b "$dev" ] || continue
        mount -t iso9660 -o ro "$dev" /medium 2>/dev/null || continue
        if [ -f "/medium/{squashfs}" ]; then
            echo "singularity: found it on $dev"
            return 0
        fi
        umount /medium
    done
    return 1
}}

tries=0
until find_medium; do
    tries=$((tries + 1))
    if [ "$tries" -ge 30 ]; then
        rescue "no medium with {squashfs} was found"
        tries=0
    fi
    sleep 1
done

toram=1
init=/sbin/init
for arg in $(cat /proc/cmdline); do
    case "$arg" in
        init=*) init=${{arg#init=}} ;;
        noram|toram=0) toram=0 ;;
    esac
done

image="/medium/{squashfs}"
if [ "$toram" = 1 ]; then
    # Like HaliadeSYS: the whole system runs from memory, so the USB stick can be pulled out afterwards.
    # Fall back to reading from the medium when the machine has too little RAM for it.
    echo "singularity: copying the system into memory (add noram to the kernel line to skip this)"
    mount -t tmpfs -o mode=0755,size=75% tmpfs /ram
    if cp "/medium/{squashfs}" /ram/rootfs.squashfs; then
        image=/ram/rootfs.squashfs
    else
        echo "singularity: not enough memory to copy the system, running from the medium instead"
        rm -f /ram/rootfs.squashfs
        umount /ram
    fi
fi

mount -t squashfs -o ro,loop "$image" /ro || rescue "cannot mount the system image"
if [ "$image" = /ram/rootfs.squashfs ]; then
    umount /medium
    echo "singularity: the system is in memory; the medium can be removed"
fi
mount -t tmpfs -o mode=0755 tmpfs /rw
mkdir -p /rw/upper /rw/work
mount -t overlay overlay -o lowerdir=/ro,upperdir=/rw/upper,workdir=/rw/work /newroot \
    || rescue "cannot assemble the writable system"

mkdir -p /newroot/medium /newroot/dev /newroot/proc /newroot/sys
# The medium is only still mounted when the system runs from it.
[ -d /medium/live ] && mount --move /medium /newroot/medium
mount --move /dev /newroot/dev
mount --move /proc /newroot/proc
mount --move /sys /newroot/sys
# The RAM copy (if any) stays mounted underneath: the squashfs is loop-mounted from it.
# `init=` on the kernel command line (read above) picks the first process (the rescue entry uses init=/bin/sh).
exec switch_root /newroot "$init"
"#,
        squashfs = SQUASHFS_PATH
    )
}

/// Populate an initramfs tree: BusyBox, its applet links and `/init`.
pub fn write_initramfs_tree(root: &Path, busybox: &Path, label: &str) -> Result<(), IsoError> {
    for d in ["bin", "dev", "proc", "sys", "medium", "ram", "ro", "rw", "newroot", "tmp"] {
        fs::create_dir_all(root.join(d)).map_err(io_ctx(format!("creating /{d}")))?;
    }
    let bb = root.join("bin/busybox");
    fs::copy(busybox, &bb).map_err(io_ctx(format!("copying {}", busybox.display())))?;
    fs::set_permissions(&bb, fs::Permissions::from_mode(0o755)).map_err(io_ctx("setting permissions"))?;
    for a in INITRAMFS_APPLETS {
        symlink("busybox", root.join("bin").join(a)).map_err(io_ctx(format!("linking {a}")))?;
    }
    let init = root.join("init");
    fs::write(&init, init_script(label)).map_err(io_ctx("writing /init"))?;
    fs::set_permissions(&init, fs::Permissions::from_mode(0o755)).map_err(io_ctx("setting permissions"))?;
    Ok(())
}

/// The live root's `/etc` and mount points, written into the merged package tree.
pub fn write_live_root(root: &Path, hostname: &str, password_hash: &str) -> Result<(), IsoError> {
    for d in ["dev", "proc", "sys", "run", "tmp", "mnt", "medium", "root", "home", "var/log", "etc/init.d", "etc/modprobe.d", "usr/sbin", "usr/bin"] {
        fs::create_dir_all(root.join(d)).map_err(io_ctx(format!("creating /{d}")))?;
    }
    fs::set_permissions(root.join("tmp"), fs::Permissions::from_mode(0o1777)).map_err(io_ctx("setting permissions"))?;
    fs::set_permissions(root.join("root"), fs::Permissions::from_mode(0o700)).map_err(io_ctx("setting permissions"))?;

    let put = |path: &str, mode: u32, text: String| -> Result<(), IsoError> {
        let p = root.join(path);
        fs::write(&p, text).map_err(io_ctx(format!("writing /{path}")))?;
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).map_err(io_ctx("setting permissions"))
    };
    // BusyBox mount reads the mount table from here.
    let _ = fs::remove_file(root.join("etc/mtab"));
    symlink("/proc/self/mounts", root.join("etc/mtab")).map_err(io_ctx("linking /etc/mtab"))?;
    put("etc/hostname", 0o644, format!("{hostname}\n"))?;
    put("etc/passwd", 0o644, "root:x:0:0:root:/root:/bin/bash\n".into())?;
    put("etc/shadow", 0o600, format!("root:{password_hash}:20000:0:99999:7:::\n"))?;
    put("etc/group", 0o644, STANDARD_GROUPS.iter().map(|(n, g)| format!("{n}:x:{g}:\n")).collect())?;
    // Members of `wheel` may use sudo. Nothing in the base system lets any other group do anything yet.
    put(
        "etc/sudoers",
        0o440,
        "# Written by the image builder. Edit with visudo.\n\
         Defaults secure_path=\"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin\"\n\
         root ALL=(ALL:ALL) ALL\n\
         %wheel ALL=(ALL:ALL) ALL\n"
            .into(),
    )?;
    put("etc/shells", 0o644, "/bin/sh\n/bin/bash\n".into())?;
    // glibc looks names up through this file: local files, then DNS for hosts.
    put(
        "etc/nsswitch.conf",
        0o644,
        "passwd: files\ngroup: files\nshadow: files\nhosts: files dns\nnetworks: files\nprotocols: files\nservices: files\nethers: files\nrpc: files\n".into(),
    )?;
    put(
        "etc/os-release",
        0o644,
        "NAME=\"SingularitySYS\"\nID=singularity\nPRETTY_NAME=\"SingularitySYS (live)\"\nVARIANT=\"live\"\n".into(),
    )?;
    put(
        "etc/issue",
        0o644,
        "\nSingularitySYS live system\nLog in as root (password: root), partition the disk with cfdisk,\nthen run: forge strap <root-partition> <boot-partition>\n\n".into(),
    )?;
    put(
        "etc/profile",
        0o644,
        "# The system is merged /usr: /bin and /sbin are links into /usr, and forge apply copies packages there.\nexport PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin\nfor f in /etc/profile.d/*.sh; do [ -r \"$f\" ] && . \"$f\"; done\n".into(),
    )?;
    if root.join("sbin/openrc-init").exists() {
        write_openrc(root)?;
        return Ok(());
    }
    write_managed(root, "common")?;
    // No OpenRC in the image: BusyBox init with a small rcS keeps it bootable.
    put(
        "etc/inittab",
        0o644,
        "::sysinit:/etc/init.d/rcS\n\
         tty1::respawn:/sbin/getty 38400 tty1\n\
         ttyS0::respawn:/sbin/getty -L 115200 ttyS0 vt100\n\
         ::ctrlaltdel:/sbin/reboot\n\
         ::shutdown:/bin/umount -a -r\n"
            .into(),
    )?;
    put(
        "etc/init.d/rcS",
        0o755,
        // Runs on the live system and on an installed one (where nothing else has mounted the
        // pseudo filesystems), so every mount is conditional. `mount -a` brings up fstab: the
        // btrfs subvolumes of an installed system (the root is already mounted by the kernel).
        "#!/bin/sh\n\
         mountpoint -q /proc || mount -t proc proc /proc\n\
         mountpoint -q /sys || mount -t sysfs sysfs /sys\n\
         mountpoint -q /dev || mount -t devtmpfs devtmpfs /dev\n\
         mkdir -p /dev/pts /dev/shm\n\
         mountpoint -q /dev/pts || mount -t devpts devpts /dev/pts\n\
         mountpoint -q /dev/shm || mount -t tmpfs tmpfs /dev/shm\n\
         mountpoint -q /run || mount -t tmpfs tmpfs /run\n\
         mountpoint -q /tmp || mount -t tmpfs tmpfs /tmp\n\
         chmod 1777 /tmp\n\
         mount -a 2>/dev/null || true\n\
         hostname \"$(cat /etc/hostname)\"\n"
            .into(),
    )?;
    // The overlay's /bin may only exist if a package provided it; /sbin/init is BusyBox's.
    Ok(())
}

/// Make the tree boot with OpenRC: `/sbin/init` is `openrc-init`, the runlevels are the ones above
/// (the package's defaults include fsck, hwclock and swap, which do not suit a live system), terminals
/// are `agetty` services, and `reboot`/`poweroff`/`halt` go through `openrc-shutdown`
/// (BusyBox's would signal PID 1 in a way openrc-init does not understand).
fn write_openrc(root: &Path) -> Result<(), IsoError> {
    // The package's own runlevels include services that do not suit this system; forge's list replaces them.
    let _ = fs::remove_dir_all(root.join("etc/runlevels"));
    write_managed(root, "openrc")?;
    let _ = fs::remove_file(root.join("etc/inittab"));
    let init = root.join("sbin/init");
    let _ = fs::remove_file(&init);
    symlink("openrc-init", &init).map_err(io_ctx("linking /sbin/init"))?;
    // The power commands work under either init system (the running one decides), so a machine switched to systemd
    // can still be rebooted from the OpenRC session that is running until that reboot.
    for (name, script) in forge_schema::power_commands() {
        let p = root.join("sbin").join(name);
        let _ = fs::remove_file(&p);
        fs::write(&p, script).map_err(io_ctx(format!("writing /sbin/{name}")))?;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).map_err(io_ctx("setting permissions"))?;
    }
    Ok(())
}

pub fn limine_conf(cmdline: &str) -> String {
    let entry = |name: &str, extra: &str| {
        format!(
            "/{name}\n    protocol: linux\n    path: boot():/boot/vmlinuz\n    cmdline: {cmdline}{extra}\n    module_path: boot():/boot/initramfs.img\n"
        )
    };
    // The normal entry boots quietly (only errors and warnings from the kernel, so the first thing on screen is the
    // system starting); the others show everything the kernel prints, for when something goes wrong. The rescue entry
    // also skips the init system entirely: a shell on the console, to look around if OpenRC does not come up.
    format!(
        "timeout: 5\n\n{}\n{}\n{}",
        entry("SingularitySYS live", " quiet loglevel=3"),
        entry("SingularitySYS live (verbose boot)", ""),
        entry("SingularitySYS live (rescue shell, no init system)", " init=/bin/sh")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_looks_for_the_squashfs_and_hands_over() {
        let s = init_script("SINGULARITY_LIVE");
        assert!(s.starts_with("#!/bin/sh"));
        assert!(s.contains("/medium/live/rootfs.squashfs"));
        assert!(s.contains("mount -t overlay"));
        assert!(s.contains("mount -t tmpfs -o mode=0755,size=75% tmpfs /ram"), "the system is copied into memory by default");
        assert!(s.contains("noram|toram=0"), "and the copy can be skipped");
        assert!(s.trim_end().ends_with("exec switch_root /newroot \"$init\""));
        assert!(s.contains("init=/sbin/init"), "the default first process");
        assert!(s.contains("init=*) init=${arg#init=}"), "init= from the command line is honoured");
    }

    #[test]
    fn live_root_has_root_login_and_a_tmp_that_everyone_can_use() {
        let t = tempfile::tempdir().unwrap();
        write_live_root(t.path(), "singularity-live", "$6$salt$hash").unwrap();
        assert!(fs::read_to_string(t.path().join("etc/shadow")).unwrap().starts_with("root:$6$salt$hash:"));
        assert_eq!(fs::metadata(t.path().join("etc/shadow")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(t.path().join("tmp")).unwrap().permissions().mode() & 0o7777, 0o1777);
        assert!(fs::read_to_string(t.path().join("etc/inittab")).unwrap().contains("getty"));
        let profile = fs::read_to_string(t.path().join("etc/profile")).unwrap();
        assert!(profile.contains("export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin"), "the merged /usr is on PATH: {profile}");
        assert!(!profile.contains("forge/profiles"), "the store links (no setuid) must never shadow the real programs");
        assert!(fs::read_to_string(t.path().join("etc/nsswitch.conf")).unwrap().contains("hosts: files dns"));
        assert!(profile.contains("/etc/profile.d/*.sh"), "forge apply writes its environment there");
        let inputrc = fs::read_to_string(t.path().join("etc/inputrc")).unwrap();
        assert!(inputrc.contains("\"\\C-h\": \"sudo forge edit\\C-m\""), "Ctrl+H edits the declaration: {inputrc}");
        assert!(inputrc.contains("previous-history"), "the arrow keys still work");
        let rcs = fs::read_to_string(t.path().join("etc/init.d/rcS")).unwrap();
        assert!(rcs.contains("mountpoint -q /proc || mount -t proc proc /proc"), "installed systems need /proc too");
        assert!(rcs.contains("mount -a"));
        assert_eq!(fs::read_link(t.path().join("etc/mtab")).unwrap(), Path::new("/proc/self/mounts"));
    }

    #[test]
    fn initramfs_tree_links_applets_to_busybox() {
        let t = tempfile::tempdir().unwrap();
        let bb = t.path().join("bb");
        fs::write(&bb, "ELF").unwrap();
        let root = t.path().join("rd");
        write_initramfs_tree(&root, &bb, "L").unwrap();
        assert_eq!(fs::read_link(root.join("bin/switch_root")).unwrap(), Path::new("busybox"));
        assert!(root.join("init").exists());
        assert_eq!(fs::metadata(root.join("init")).unwrap().permissions().mode() & 0o111, 0o111);
    }

    #[test]
    fn limine_entry_names_kernel_initramfs_and_cmdline() {
        let c = limine_conf("console=ttyS0");
        assert!(c.contains("path: boot():/boot/vmlinuz"));
        assert!(c.contains("cmdline: console=ttyS0 quiet loglevel=3\n"), "the normal entry boots quietly: {c}");
        assert!(c.contains("cmdline: console=ttyS0\n"), "a verbose entry shows everything: {c}");
        assert!(c.contains("cmdline: console=ttyS0 init=/bin/sh\n"), "a rescue entry without the init system: {c}");
        assert_eq!(c.matches("module_path").count(), 3);
    }
}

#[cfg(test)]
mod openrc_tests {
    use super::*;

    #[test]
    fn with_openrc_the_tree_boots_through_it() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("sbin")).unwrap();
        fs::write(t.path().join("sbin/openrc-init"), "ELF").unwrap();
        // What the busybox and openrc packages leave behind.
        symlink("../bin/busybox", t.path().join("sbin/init")).unwrap();
        symlink("../bin/busybox", t.path().join("sbin/reboot")).unwrap();
        symlink("../bin/busybox", t.path().join("sbin/poweroff")).unwrap();
        fs::create_dir_all(t.path().join("etc/runlevels/boot")).unwrap();
        symlink("/etc/init.d/fsck", t.path().join("etc/runlevels/boot/fsck")).unwrap();
        write_live_root(t.path(), "singularity-live", "$6$s$h").unwrap();

        assert_eq!(fs::read_link(t.path().join("sbin/init")).unwrap(), Path::new("openrc-init"));
        assert!(!t.path().join("etc/inittab").exists());
        assert!(!t.path().join("etc/init.d/rcS").exists(), "no BusyBox rcS when OpenRC runs the system");
        // The package's default runlevels are replaced, not extended.
        assert!(fs::symlink_metadata(t.path().join("etc/runlevels/boot/fsck")).is_err());
        assert_eq!(
            fs::read_link(t.path().join("etc/runlevels/boot/localmount")).unwrap(),
            Path::new("/etc/init.d/localmount")
        );
        assert!(fs::symlink_metadata(t.path().join("etc/runlevels/default/agetty.tty1")).is_ok());
        assert!(fs::symlink_metadata(t.path().join("etc/runlevels/default/network")).is_ok(), "networking starts by default");
        assert!(fs::symlink_metadata(t.path().join("etc/runlevels/boot/coldplug")).is_ok(), "drivers are loaded for the hardware found");
        let svc = fs::read_to_string(t.path().join("etc/init.d/network")).unwrap();
        assert!(svc.starts_with("#!/sbin/openrc-run") && svc.contains("netwatch"), "{svc}");
        let watch = fs::read_to_string(t.path().join("usr/libexec/singularity/netwatch")).unwrap();
        assert!(watch.contains("wpa_supplicant -B") && watch.contains("dhcpcd -b -q") && !watch.contains("udhcpc"), "{watch}");
        assert!(fs::metadata(t.path().join("usr/bin/wifi")).unwrap().permissions().mode() & 0o111 != 0);
        assert!(fs::read_to_string(t.path().join("etc/wpa_supplicant/wpa_supplicant.conf")).unwrap().contains("update_config=1"));
        assert_eq!(fs::read_link(t.path().join("etc/init.d/agetty.tty1")).unwrap(), Path::new("agetty"));
        let conf = fs::read_to_string(t.path().join("etc/conf.d/agetty.ttyS0")).unwrap();
        assert!(conf.contains("baud=\"115200\"") && conf.contains("-L"), "{conf}");
        // BusyBox's reboot would signal PID 1 in a way openrc-init does not handle.
        let reboot = fs::read_to_string(t.path().join("sbin/reboot")).unwrap();
        assert!(reboot.contains("openrc-shutdown -r now"), "{reboot}");
        // `poweroff` is ours too, and works under either init system.
        let poweroff = fs::read_to_string(t.path().join("sbin/poweroff")).unwrap();
        assert!(poweroff.contains("systemctl poweroff") && poweroff.contains("openrc-shutdown -p now"), "{poweroff}");
        let sd = fs::read_to_string(t.path().join("sbin/shutdown")).unwrap();
        assert!(sd.contains("openrc-shutdown $flag $when") && sd.contains("flag=-p"), "{sd}");
        assert_eq!(fs::metadata(t.path().join("sbin/shutdown")).unwrap().permissions().mode() & 0o111, 0o111);
    }

    #[test]
    fn without_openrc_busybox_init_is_the_fallback() {
        let t = tempfile::tempdir().unwrap();
        write_live_root(t.path(), "h", "x").unwrap();
        assert!(t.path().join("etc/inittab").exists());
        assert!(t.path().join("etc/init.d/rcS").exists());
        assert!(!t.path().join("etc/runlevels").exists());
    }
}


/// Groups every system gets, with fixed ids (so a file's group means the same on every install). `wheel`
/// is the administrators; the rest exist so that users can be put in them: devices only honour them once
/// something (udev, mdev rules) assigns them, which the base system does not do yet.
pub const STANDARD_GROUPS: &[(&str, u32)] = &[
    ("root", 0),
    ("daemon", 1),
    ("tty", 5),
    ("disk", 6),
    ("wheel", 10),
    ("cdrom", 11),
    ("audio", 18),
    ("video", 27),
    ("input", 97),
    ("users", 100),
    ("nogroup", 65534),
];


/// Write what forge keeps right (`forge_schema::managed`) into a new image: the same files `forge apply` keeps current
/// on an installed system, plus the login shells' environment.
fn write_managed(root: &Path, init: &str) -> Result<(), IsoError> {
    let put = |abs: &str, entry: &forge_schema::Entry| -> Result<(), IsoError> {
        let p = root.join(abs.trim_start_matches('/'));
        fs::create_dir_all(p.parent().expect("has parent")).map_err(io_ctx(format!("creating the directory of {abs}")))?;
        let _ = fs::remove_file(&p);
        match entry {
            forge_schema::Entry::File { content, mode } => {
                fs::write(&p, content).map_err(io_ctx(format!("writing {abs}")))?;
                fs::set_permissions(&p, fs::Permissions::from_mode(*mode)).map_err(io_ctx("setting permissions"))
            }
            forge_schema::Entry::Link(target) => symlink(target, &p).map_err(io_ctx(format!("linking {abs}"))),
            forge_schema::Entry::Absent => Ok(()),
        }
    };
    for m in forge_schema::managed(init) {
        put(&m.path, &m.entry)?;
    }
    put("/etc/profile.d/forge.sh", &forge_schema::Entry::File { content: forge_schema::profile_script(), mode: 0o644 })
}
