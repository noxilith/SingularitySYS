//! What differs between init systems, in one place: the packages each one is made of, the few files forge
//! owns so that the system boots with the chosen one (`/sbin/init` and the power commands), and what is
//! enabled by default. Used by `forge apply` when `init` changes, and by the image builder.

/// The init systems forge can set up.
pub const INIT_SYSTEMS: &[&str] = &["openrc", "systemd"];

/// The packages an init system is made of (their dependencies come along).
pub fn init_packages(system: &str) -> &'static [&'static str] {
    match system {
        "openrc" => &["openrc"],
        "systemd" => &["systemd", "dbus"],
        _ => &[],
    }
}

/// The packages only that init system uses (dbus is pulled in by systemd but is useful on its own, so it is not here).
fn exclusive_packages(system: &str) -> &'static [&'static str] {
    match system {
        "openrc" => &["openrc"],
        "systemd" => &["systemd"],
        _ => &[],
    }
}

/// Every package that belongs to some init system other than `system`: when `system` is chosen these are removed.
pub fn foreign_init_packages(system: &str) -> Vec<&'static str> {
    INIT_SYSTEMS.iter().filter(|s| **s != system).flat_map(|s| exclusive_packages(s).iter().copied()).collect()
}

/// A file forge owns outside the packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Link(String),
    File { content: String, mode: u32 },
    /// Must not exist.
    Absent,
}

/// `shutdown [-h|-P|-p|-H|-r] [now|+minutes]`: power off by default; `-r` reboots, `-H` halts.
/// A thin front end for `openrc-shutdown`, which takes the time as a required argument.
pub const OPENRC_SHUTDOWN: &str = r#"#!/bin/sh
flag=-p
when=now
for a in "$@"; do
    case "$a" in
        -h|-P|-p) flag=-p ;;
        -H) flag=-H ;;
        -r) flag=-r ;;
        -*) ;;
        *) when=$a ;;
    esac
done
exec /sbin/openrc-shutdown $flag $when
"#;

/// `reboot` and `halt` under OpenRC (BusyBox's would signal PID 1 in a way openrc-init does not understand).
pub fn openrc_power(flag: &str) -> String {
    format!("#!/bin/sh\nexec /sbin/openrc-shutdown {flag} now\n")
}

/// The files that make the system start with `system`: `/sbin/init` (a link in the merged `/usr/sbin`) and the
/// commands that stop the machine. Packages never install these, so two init systems cannot clash over them.
pub fn plumbing(system: &str) -> Vec<(&'static str, Entry)> {
    let file = |content: String| Entry::File { content, mode: 0o755 };
    match system {
        "openrc" => vec![
            ("/usr/sbin/init", Entry::Link("openrc-init".into())),
            ("/usr/sbin/reboot", file(openrc_power("-r"))),
            ("/usr/sbin/halt", file(openrc_power("-H"))),
            ("/usr/sbin/shutdown", file(OPENRC_SHUTDOWN.into())),
            ("/usr/sbin/poweroff", Entry::Absent),
        ],
        "systemd" => vec![
            ("/usr/sbin/init", Entry::Link("../lib/systemd/systemd".into())),
            ("/usr/sbin/reboot", Entry::Link("../bin/systemctl".into())),
            ("/usr/sbin/halt", Entry::Link("../bin/systemctl".into())),
            ("/usr/sbin/poweroff", Entry::Link("../bin/systemctl".into())),
            ("/usr/sbin/shutdown", Entry::Link("../bin/systemctl".into())),
        ],
        _ => Vec::new(),
    }
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
        assert_eq!(foreign_init_packages("systemd"), ["openrc"]);
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
    fn unit_names() {
        assert_eq!(systemd_unit("sshd"), "sshd.service");
        assert_eq!(systemd_unit("dbus.socket"), "dbus.socket");
    }
}
