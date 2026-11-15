//! What differs between init systems, in one place: the packages each one is made of, the few files forge
//! owns so that the system boots with the chosen one (`/sbin/init` and the power commands), and what is
//! enabled by default. Used by `forge apply` when `init` changes, and by the image builder.

/// The init systems forge can set up.
pub const INIT_SYSTEMS: &[&str] = &["openrc", "systemd"];

/// The packages an init system is made of (their dependencies come along).
pub fn init_packages(system: &str) -> &'static [&'static str] {
    match system {
        "openrc" => &["openrc", "chrony"],
        "systemd" => &["systemd", "dbus"],
        _ => &[],
    }
}

/// The packages only that init system uses (dbus is pulled in by systemd but is useful on its own, so it is not here).
fn exclusive_packages(system: &str) -> &'static [&'static str] {
    match system {
        "openrc" => &["openrc", "chrony"],
        "systemd" => &["systemd"],
        _ => &[],
    }
}

/// Every package that belongs to some init system other than `system`: when `system` is chosen these are removed.
pub fn foreign_init_packages(system: &str) -> Vec<&'static str> {
    INIT_SYSTEMS.iter().filter(|s| **s != system).flat_map(|s| exclusive_packages(s).iter().copied()).collect()
}

/// The packages every SingularitySYS system has: forge installs and upgrades them without the declaration naming
/// them (the declaration lists what *you* chose). The kernel (`linux`, managed with `forge kernel`) and the init
/// system's packages (see [`init_packages`]) are not in this list.
pub const BASE_PACKAGES: &[&str] = &[
    "glibc", "filesystem", "busybox", "dash", "bash", "coreutils", "findutils", "grep", "sed", "gawk", "tar", "gzip", "xz",
    "diffutils", "util-linux", "shadow", "sudo", "kmod", "iproute2", "dhcpcd", "wpa_supplicant", "wireless-regdb",
    "linux-firmware-wifi", "btrfs-progs", "limine", "efibootmgr", "rsync", "forge", "ncurses", "libxcrypt", "openssl",
    "tzdata", "glibc-locales", "curl", "ca-certificates", "micro",
];

/// True for a package that is part of the base system (it cannot be removed, and need not be declared).
pub fn is_base_package(name: &str) -> bool {
    BASE_PACKAGES.contains(&name) || INIT_SYSTEMS.iter().any(|s| init_packages(s).contains(&name))
}

/// A file forge owns outside the packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Link(String),
    File { content: String, mode: u32 },
    /// Must not exist.
    Absent,
}

/// `reboot`, `halt` and `poweroff` for either init system. Which init is *running* decides (systemd announces itself
/// with /run/systemd/system), not which one is configured: a machine switched to systemd is still running OpenRC until
/// its next boot and must be able to reboot into the change. By then OpenRC's own tools are gone (the package was
/// removed), so when `openrc-shutdown` is missing the running openrc-init is asked directly, through its control
/// FIFO (which is all `openrc-shutdown` does), and as a last resort BusyBox forces it.
pub fn power_script(action: &str) -> String {
    let (word, flag) = match action {
        "reboot" => ("reboot", "-r"),
        "halt" => ("halt", "-H"),
        _ => ("poweroff", "-p"),
    };
    format!(
        "#!/bin/sh\n\
         if [ -d /run/systemd/system ]; then\n\
         \texec /usr/bin/systemctl {word} \"$@\"\n\
         fi\n\
         if [ -x /usr/sbin/openrc-shutdown ]; then\n\
         \texec /usr/sbin/openrc-shutdown {flag} now\n\
         fi\n\
         sync\n\
         if [ -p /run/openrc/init.ctl ]; then\n\
         \techo {word} > /run/openrc/init.ctl\n\
         \texit 0\n\
         fi\n\
         exec /usr/bin/busybox {word} -f\n"
    )
}

/// `shutdown [-h|-P|-p|-H|-r] [now|+minutes]`: power off by default; `-r` reboots, `-H` halts. A thin front end for
/// `systemctl`, `openrc-shutdown` (which takes the time as a required argument), or the running init directly.
pub const SHUTDOWN_SCRIPT: &str = r#"#!/bin/sh
flag=-p
when=now
action=poweroff
for a in "$@"; do
    case "$a" in
        -h|-P|-p) flag=-p; action=poweroff ;;
        -H) flag=-H; action=halt ;;
        -r) flag=-r; action=reboot ;;
        -*) ;;
        *) when=$a ;;
    esac
done
if [ -d /run/systemd/system ]; then
    exec /usr/bin/systemctl $action
fi
if [ -x /usr/sbin/openrc-shutdown ]; then
    exec /usr/sbin/openrc-shutdown $flag $when
fi
sync
if [ -p /run/openrc/init.ctl ]; then
    echo $action > /run/openrc/init.ctl
    exit 0
fi
exec /usr/bin/busybox $action -f
"#;

/// The power commands as (name, script).
pub fn power_commands() -> Vec<(&'static str, String)> {
    vec![
        ("reboot", power_script("reboot")),
        ("halt", power_script("halt")),
        ("poweroff", power_script("poweroff")),
        ("shutdown", SHUTDOWN_SCRIPT.to_string()),
    ]
}

/// The files that make the system start with `system`: `/sbin/init` (a link in the merged `/usr/sbin`) and the
/// commands that stop the machine. Packages never install these, so two init systems cannot clash over them.
pub fn plumbing(system: &str) -> Vec<(&'static str, Entry)> {
    let init = match system {
        "openrc" => "openrc-init",
        "systemd" => "../lib/systemd/systemd",
        _ => return Vec::new(),
    };
    let mut v = vec![("/usr/sbin/init", Entry::Link(init.into()))];
    v.extend(power_plumbing());
    v
}

/// The power commands alone: the same for both init systems (see [`power_script`]), so they are kept current on every
/// apply, not only when the init system changes.
pub fn power_plumbing() -> Vec<(&'static str, Entry)> {
    power_commands()
        .into_iter()
        .map(|(name, content)| {
            let path: &'static str = match name {
                "reboot" => "/usr/sbin/reboot",
                "halt" => "/usr/sbin/halt",
                "poweroff" => "/usr/sbin/poweroff",
                _ => "/usr/sbin/shutdown",
            };
            (path, Entry::File { content, mode: 0o755 })
        })
        .collect()
}

/// Units enabled on every systemd system: (unit, the target that wants it). Package units are only
/// *installed* by their packages; nothing runs until it is enabled, which is what these links do.
pub const SYSTEMD_BASELINE: &[(&str, &str)] = &[
    ("dbus.socket", "sockets.target"),
    ("dbus.service", "multi-user.target"),
    ("systemd-networkd.service", "multi-user.target"),
    ("systemd-resolved.service", "multi-user.target"),
    ("systemd-timesyncd.service", "sysinit.target"),
    ("systemd-logind.service", "multi-user.target"),
];

/// `foo` and `foo.service` both name the unit `foo.service`; `foo.socket` and the like are kept as written.
pub fn systemd_unit(name: &str) -> String {
    const SUFFIXES: &[&str] = &[".service", ".socket", ".timer", ".target", ".path", ".mount", ".automount", ".swap"];
    if SUFFIXES.iter().any(|s| name.ends_with(s)) { name.to_string() } else { format!("{name}.service") }
}

/// Network configuration for systemd-networkd: DHCP on every wired and wireless interface.
/// (Joining a Wi-Fi network needs wpa_supplicant, started per interface.)
pub const SYSTEMD_NETWORK_FILES: &[(&str, &str)] = &[
    (
        "/etc/systemd/network/20-wired.network",
        "# managed by forge\n[Match]\nName=en* eth*\n\n[Network]\nDHCP=yes\n\n[DHCPv4]\nRouteMetric=100\n",
    ),
    (
        "/etc/systemd/network/25-wireless.network",
        "# managed by forge\n[Match]\nName=wl*\n\n[Network]\nDHCP=yes\n\n[DHCPv4]\nRouteMetric=600\n",
    ),
];

/// Wi-Fi under systemd: a supplicant per wireless interface, started by udev when the interface appears. It uses the
/// same `/etc/wpa_supplicant/wpa_supplicant.conf` as under OpenRC, so the `wifi` command works unchanged.
pub const SYSTEMD_WIFI_FILES: &[(&str, &str)] = &[
    (
        "/etc/systemd/system/wpa_supplicant@.service",
        "# managed by forge\n[Unit]\nDescription=WPA supplicant for %I\nAfter=sys-subsystem-net-devices-%i.device\nBefore=network.target\nWants=network.target\n\n[Service]\nType=simple\nExecStart=/usr/sbin/wpa_supplicant -c/etc/wpa_supplicant/wpa_supplicant.conf -i%I\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n",
    ),
    (
        "/etc/udev/rules.d/80-wifi.rules",
        "# managed by forge\nSUBSYSTEM==\"net\", ACTION==\"add\", ENV{DEVTYPE}==\"wlan\", TAG+=\"systemd\", ENV{SYSTEMD_WANTS}+=\"wpa_supplicant@%k.service\"\n",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_init_owns_its_packages_and_removes_the_others() {
        assert_eq!(init_packages("systemd"), ["systemd", "dbus"]);
        assert_eq!(foreign_init_packages("systemd"), ["openrc", "chrony"]);
        assert_eq!(foreign_init_packages("openrc"), ["systemd"]);
    }

    #[test]
    fn plumbing_differs_per_init_and_covers_the_same_paths() {
        let mut a: Vec<_> = plumbing("openrc").into_iter().map(|(p, _)| p).collect();
        let mut b: Vec<_> = plumbing("systemd").into_iter().map(|(p, _)| p).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "switching must replace every path, or a stale script would be left behind");
        assert!(plumbing("systemd").iter().any(|(p, e)| *p == "/usr/sbin/init" && *e == Entry::Link("../lib/systemd/systemd".into())));
    }


    #[test]
    fn the_power_scripts_are_valid_shell_and_do_not_need_openrcs_tools() {
        use std::io::Write;
        for (name, script) in power_commands() {
            let mut sh = std::process::Command::new("sh").arg("-n").stdin(std::process::Stdio::piped()).spawn().unwrap();
            sh.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
            assert!(sh.wait().unwrap().success(), "{name} is not valid shell:\n{script}");
            // With openrc-shutdown gone (OpenRC removed, still running) they talk to the running init.
            assert!(script.contains("/run/openrc/init.ctl") && script.contains("busybox"), "{name}:\n{script}");
        }
    }
    #[test]
    fn unit_names() {
        assert_eq!(systemd_unit("sshd"), "sshd.service");
        assert_eq!(systemd_unit("dbus.socket"), "dbus.socket");
    }
}
