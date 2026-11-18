//! Services forge sets up from the declaration: the firewall, zram swap, routine maintenance and (when `openssh` is
//! chosen) the SSH server. The programs behind them live in `/usr/libexec/singularity`; the init system only starts them.

use crate::init::Entry;
use crate::managed::{Managed, Own, file, link, managed};
use crate::system::SystemDecl;

const ZRAM: &str = r#"#!/bin/sh
# managed by forge: compressed swap in memory, half the size of the RAM (it holds about twice that after compression)
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin; export PATH
case "$1" in
start)
	grep -q '^/dev/zram' /proc/swaps 2>/dev/null && exit 0
	modprobe zram 2>/dev/null
	kb=$(awk '/^MemTotal:/ { print int($2 / 2) }' /proc/meminfo)
	dev=$(zramctl --find --size "${kb}K" --algorithm zstd 2>/dev/null) || dev=$(zramctl --find --size "${kb}K") || exit 1
	mkswap "$dev" >/dev/null && swapon --priority 100 "$dev"
	;;
stop)
	for dev in $(awk '/^\/dev\/zram/ { print $1 }' /proc/swaps); do
		swapoff "$dev" && zramctl --reset "$dev"
	done
	;;
*)
	echo "usage: $0 start|stop" >&2
	exit 2
	;;
esac
"#;

const MAINTENANCE_LOOP: &str = r#"#!/bin/sh
# managed by forge: look once an hour for routine work that is due (forge keeps track of when each task last ran)
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin; export PATH
while :; do
	forge maintenance run --due
	sleep 3600
done
"#;

const OPENRC_ZRAM: &str = r#"#!/sbin/openrc-run
description="Compressed swap in memory"

depend() {
	need localmount
	before network
}

start() {
	ebegin "Setting up zram swap"
	/usr/libexec/singularity/zram start
	eend $?
}

stop() {
	ebegin "Removing zram swap"
	/usr/libexec/singularity/zram stop
	eend $?
}
"#;

const OPENRC_MAINTENANCE: &str = r#"#!/sbin/openrc-run
description="Routine maintenance: trim, scrub, prune old generations"
command=/usr/libexec/singularity/maintenance-loop
command_background=true
pidfile=/run/forge-maintenance.pid

depend() {
	need localmount
	after network
}
"#;

const OPENRC_SSHD: &str = r#"#!/sbin/openrc-run
description="OpenSSH server"
command=/usr/sbin/sshd
command_args="-D"
command_background=true
pidfile=/run/sshd.pid

depend() {
	need net
	use logger
}

start_pre() {
	mkdir -p /var/empty
	getent passwd sshd >/dev/null || useradd --system --home-dir /var/empty --shell /usr/sbin/nologin sshd
	ssh-keygen -A >/dev/null 2>&1
	/usr/sbin/sshd -t
}
"#;

const SSHD_CONFIG: &str = "# SSH server. Safe defaults: root cannot log in, only the users and groups you name below can.\n\
# Open the port in the firewall too: allow = [\"ssh\"] in the [firewall] section of /etc/forge/system.toml.\n\
PermitRootLogin no\n\
PasswordAuthentication yes\n\
KbdInteractiveAuthentication no\n\
UsePAM no\n\
X11Forwarding no\n\
PrintMotd no\n\
Subsystem sftp /usr/libexec/sftp-server\n";

fn openrc_nftables(forwarding: bool) -> String {
    let fwd = if forwarding { "\techo 1 > /proc/sys/net/ipv4/ip_forward\n" } else { "" };
    format!(
        "#!/sbin/openrc-run\n\
         description=\"Firewall (rules from the [firewall] section of the system declaration)\"\n\
         \n\
         depend() {{\n\
         \tneed localmount\n\
         \tbefore network\n\
         }}\n\
         \n\
         start() {{\n\
         \tebegin \"Loading firewall rules\"\n\
         {fwd}\
         \tnft -f /etc/nftables.conf\n\
         \teend $?\n\
         }}\n\
         \n\
         stop() {{\n\
         \tebegin \"Removing firewall rules\"\n\
         \tnft flush ruleset\n\
         \teend $?\n\
         }}\n"
    )
}

fn unit(description: &str, service: &str) -> String {
    format!("# managed by forge\n[Unit]\nDescription={description}\n{service}\n[Install]\nWantedBy=multi-user.target\n")
}

fn systemd_nftables(forwarding: bool) -> String {
    let fwd = if forwarding { "ExecStartPre=/usr/bin/sysctl -w net.ipv4.ip_forward=1\n" } else { "" };
    unit(
        "Firewall",
        &format!("Before=network-pre.target\nWants=network-pre.target\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\n{fwd}ExecStart=/usr/sbin/nft -f /etc/nftables.conf\nExecStop=/usr/sbin/nft flush ruleset\n"),
    )
}

/// Everything in [`managed`] plus what the declaration asks for: the firewall (and port forwarding), zram swap, routine
/// maintenance and the SSH server. A service the declaration turns off is made absent.
pub fn managed_for(decl: &SystemDecl) -> Vec<Managed> {
    let init = decl.init.system.as_str();
    let mut v = managed(init);
    v.push(file("/etc/hosts", &crate::managed::hosts_file(&decl.hostname), 0o644, Own::IfMissing));
    let ssh = decl.packages.iter().any(|p| p == "openssh");
    let forwarding = decl.firewall.enable && !decl.firewall.forward.is_empty();

    // A service is a unit or script plus its enable link; (name, wanted, openrc script, systemd unit text, runlevel).
    struct Svc<'a> {
        name: &'a str,
        wanted: bool,
        openrc: String,
        systemd: String,
    }
    let services = [
        Svc { name: "nftables", wanted: decl.firewall.enable, openrc: openrc_nftables(forwarding), systemd: systemd_nftables(forwarding) },
        Svc {
            name: "zram",
            wanted: decl.swap == "zram",
            openrc: OPENRC_ZRAM.into(),
            systemd: unit("Compressed swap in memory", "\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/usr/libexec/singularity/zram start\nExecStop=/usr/libexec/singularity/zram stop\n"),
        },
        Svc {
            name: "forge-maintenance",
            wanted: decl.maintenance.enable,
            openrc: OPENRC_MAINTENANCE.into(),
            systemd: unit("Routine maintenance", "\n[Service]\nExecStart=/usr/libexec/singularity/maintenance-loop\nRestart=always\n"),
        },
        Svc {
            name: "sshd",
            wanted: ssh,
            openrc: OPENRC_SSHD.into(),
            systemd: unit(
                "OpenSSH server",
                "After=network.target\n\n[Service]\nExecStartPre=/bin/sh -c 'PATH=/usr/sbin:/usr/bin:$PATH; mkdir -p /var/empty; getent passwd sshd >/dev/null || useradd --system --home-dir /var/empty --shell /usr/sbin/nologin sshd; ssh-keygen -A; /usr/sbin/sshd -t'\nExecStart=/usr/sbin/sshd -D\nRestart=on-failure\n",
            ),
        },
    ];

    if decl.firewall.enable {
        v.push(file("/etc/nftables.conf", &crate::extras::nftables_conf(&decl.firewall), 0o600, Own::Always));
    } else {
        v.push(Managed { path: "/etc/nftables.conf".into(), entry: Entry::Absent, own: Own::Always });
    }
    if decl.swap == "zram" {
        v.push(file("/usr/libexec/singularity/zram", ZRAM, 0o755, Own::Always));
    }
    if decl.maintenance.enable {
        v.push(file("/usr/libexec/singularity/maintenance-loop", MAINTENANCE_LOOP, 0o755, Own::Always));
    }
    if ssh {
        v.push(file("/etc/ssh/sshd_config", SSHD_CONFIG, 0o644, Own::IfMissing));
    }

    for s in services {
        // A package may ship its own service of the same name (sshd, with a different OpenSSH build): leave that alone.
        if !s.wanted && s.name == "sshd" {
            continue;
        }
        if init == "openrc" {
            let script = format!("/etc/init.d/{}", s.name);
            let enable = format!("/etc/runlevels/default/{}", s.name);
            if s.wanted {
                v.push(file(&script, &s.openrc, 0o755, Own::Always));
                v.push(link(enable, script, Own::Always));
            } else {
                v.push(Managed { path: enable, entry: Entry::Absent, own: Own::Always });
                v.push(Managed { path: script, entry: Entry::Absent, own: Own::Always });
            }
        } else {
            let path = format!("/etc/systemd/system/{}.service", s.name);
            let enable = format!("/etc/systemd/system/multi-user.target.wants/{}.service", s.name);
            if s.wanted {
                v.push(file(&path, &s.systemd, 0o644, Own::Always));
                v.push(link(enable, path, Own::Always));
            } else {
                v.push(Managed { path: enable, entry: Entry::Absent, own: Own::Always });
                v.push(Managed { path, entry: Entry::Absent, own: Own::Always });
            }
        }
    }
    // The desktop brings NetworkManager, which must be the only thing configuring interfaces: forge's own network setup
    // (the `network` service under OpenRC, systemd-networkd and the per-interface supplicant under systemd) is switched off
    // while the desktop is installed and comes back, exactly as before, when it is removed.
    let networkd_link = "/etc/systemd/system/multi-user.target.wants/systemd-networkd.service".to_string();
    if decl.desktop() {
        if init == "openrc" {
            for m in v.iter_mut().filter(|m| m.path == "/etc/runlevels/default/network") {
                m.entry = Entry::Absent;
            }
        } else {
            for path in [networkd_link.as_str(), "/etc/udev/rules.d/80-wifi.rules"] {
                v.push(Managed { path: path.into(), entry: Entry::Absent, own: Own::Always });
            }
        }
    } else if init != "openrc" {
        v.push(link(networkd_link.clone(), "/usr/lib/systemd/system/systemd-networkd.service".into(), Own::Always));
        for (path, content) in crate::SYSTEMD_NETWORK_FILES.iter().chain(crate::SYSTEMD_WIFI_FILES.iter()) {
            v.push(file(path, content, 0o644, Own::Always));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(extra: &str) -> SystemDecl {
        SystemDecl::parse(&format!("hostname = \"t\"\nlibc = \"glibc\"\n{extra}")).unwrap()
    }

    fn find<'a>(v: &'a [Managed], p: &str) -> Option<&'a Managed> {
        v.iter().find(|m| m.path == p)
    }

    #[test]
    fn the_desktop_swaps_forges_network_setup_for_networkmanager_and_gives_it_back() {
        let on = |init: &str| managed_for(&decl(&format!("init = \"{init}\"\npackages = [\"vesper\"]")).with_desktop());
        let off = |init: &str| managed_for(&decl(&format!("init = \"{init}\"")));
        let rl = "/etc/runlevels/default/network";
        assert_eq!(find(&on("openrc"), rl).unwrap().entry, Entry::Absent);
        assert!(matches!(find(&off("openrc"), rl).unwrap().entry, Entry::Link(_)));
        let nd = "/etc/systemd/system/multi-user.target.wants/systemd-networkd.service";
        assert_eq!(find(&on("systemd"), nd).unwrap().entry, Entry::Absent);
        assert!(matches!(find(&off("systemd"), nd).unwrap().entry, Entry::Link(_)));
        assert_eq!(find(&on("systemd"), "/etc/udev/rules.d/80-wifi.rules").unwrap().entry, Entry::Absent);
        assert!(matches!(find(&off("systemd"), "/etc/udev/rules.d/80-wifi.rules").unwrap().entry, Entry::File { .. }));
    }

    #[test]
    fn firewall_swap_and_maintenance_are_on_by_default_and_ssh_only_when_chosen() {
        let v = managed_for(&decl(""));
        for s in ["nftables", "zram", "forge-maintenance"] {
            assert!(matches!(find(&v, &format!("/etc/runlevels/default/{s}")).unwrap().entry, Entry::Link(_)), "{s}");
        }
        assert!(find(&v, "/etc/runlevels/default/sshd").is_none(), "sshd only when openssh is chosen");
        let v = managed_for(&decl("packages = [\"openssh\"]"));
        assert!(matches!(find(&v, "/etc/runlevels/default/sshd").unwrap().entry, Entry::Link(_)));
        assert!(find(&v, "/etc/ssh/sshd_config").unwrap().own == Own::IfMissing, "never overwrite the user's SSH settings");
    }

    #[test]
    fn what_the_declaration_turns_off_is_made_absent() {
        let v = managed_for(&decl("swap = \"none\"\n[firewall]\nenable = false\n[maintenance]\nenable = false\n"));
        for s in ["nftables", "zram", "forge-maintenance"] {
            assert!(matches!(find(&v, &format!("/etc/runlevels/default/{s}")).unwrap().entry, Entry::Absent), "{s}");
        }
        assert!(matches!(find(&v, "/etc/nftables.conf").unwrap().entry, Entry::Absent));
    }

    #[test]
    fn port_forwarding_turns_on_ip_forwarding_for_both_init_systems() {
        let fwd = "[firewall]\nallow = [\"ssh\"]\n[[firewall.forward]]\nport = 8080\nto = \"192.168.1.50:80\"\n";
        assert!(text(&managed_for(&decl(fwd)), "/etc/init.d/nftables").contains("ip_forward"));
        let sd = managed_for(&decl(&format!("init = \"systemd\"\n{fwd}")));
        assert!(text(&sd, "/etc/systemd/system/nftables.service").contains("net.ipv4.ip_forward=1"));
        assert!(!text(&managed_for(&decl("")), "/etc/init.d/nftables").contains("ip_forward"), "no forwarding unless something is forwarded");
    }

    fn text(v: &[Managed], p: &str) -> String {
        match &find(v, p).unwrap().entry {
            Entry::File { content, .. } => content.clone(),
            other => panic!("{p}: {other:?}"),
        }
    }

    #[test]
    fn the_scripts_find_the_system_programs_whatever_the_init_systems_path_is() {
        for m in managed_for(&decl("")) {
            if m.path == "/usr/libexec/singularity/zram" || m.path == "/usr/libexec/singularity/maintenance-loop" {
                let Entry::File { content, .. } = &m.entry else { panic!() };
                assert!(content.contains("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin"), "{}", m.path);
            }
        }
    }

    #[test]
    fn the_scripts_are_valid_shell() {
        use std::io::Write;
        for m in managed_for(&decl("packages = [\"openssh\"]")) {
            if let Entry::File { content, .. } = &m.entry {
                if content.starts_with("#!/bin/sh") || content.starts_with("#!/sbin/openrc-run") {
                    let mut sh = std::process::Command::new("sh").arg("-n").stdin(std::process::Stdio::piped()).spawn().unwrap();
                    sh.stdin.take().unwrap().write_all(content.as_bytes()).unwrap();
                    assert!(sh.wait().unwrap().success(), "{} is not valid shell", m.path);
                }
            }
        }
    }
}
