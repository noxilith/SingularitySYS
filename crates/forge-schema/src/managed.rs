//! The files forge keeps right outside the packages: the network and driver-loading services, the `wifi` command, the
//! shell defaults, the OpenRC runlevels. The image builder writes them into a new image and `forge apply` keeps them
//! current on an installed system, so a fix to any of them arrives with `forge upgrade` (the forge package carries
//! them) instead of needing a new image.
//!
//! Two kinds of ownership: *always* (forge rewrites the file when it differs: programs, scripts, and defaults you
//! are not meant to edit) and *if missing* (a starting point you may edit: forge never overwrites it).

use crate::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Own {
    Always,
    IfMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    pub path: String,
    pub entry: Entry,
    pub own: Own,
}

/// OpenRC's runlevels: services are links `/etc/runlevels/<level>/<service>` -> `/etc/init.d/<service>`. (The package's
/// own defaults include fsck, hwclock and swap, which do not suit this system.) forge makes sure these exist; services
/// you add are left alone.
pub const OPENRC_RUNLEVELS: &[(&str, &[&str])] = &[
    ("sysinit", &["devfs", "dmesg", "sysfs"]),
    ("boot", &["procfs", "coldplug", "root", "hostname", "localmount", "bootmisc", "loopback"]),
    ("default", &["network", "chronyd", "agetty.tty1", "agetty.ttyS0", "local"]),
    ("shutdown", &["killprocs", "mount-ro"]),
];

/// One `agetty` service per terminal: (port, baud, TERM, extra options).
pub const AGETTY_PORTS: &[(&str, &str, &str, &str)] = &[("tty1", "38400", "linux", ""), ("ttyS0", "115200", "vt100", "-L")];

/// `/etc/profile.d/forge.sh`: the environment of every login shell.
pub fn profile_script() -> String {
    "# managed by forge: the system's programs are in /usr\n\
     for d in /usr/local/sbin /usr/local/bin /usr/sbin /usr/bin; do\n\
     \x20   case \":$PATH:\" in *\":$d:\"*) ;; *) PATH=\"$PATH:$d\" ;; esac\n\
     done\n\
     export PATH\n\
     export PS1='\\u@\\h:\\w\\$ '\n\
     export EDITOR=${EDITOR:-micro} VISUAL=${VISUAL:-micro}\n\
     export TERM=${TERM:-linux}\n"
        .to_string()
}

const INPUTRC: &str = "# managed by forge. Line editing: arrow keys, history, Home/End/Delete on the Linux console and in terminal\n\
# emulators, and no bracketed-paste escape codes (the console would show them as text).\n\
set enable-bracketed-paste off\nset bell-style none\n\
\"\\e[A\": previous-history\n\"\\e[B\": next-history\n\"\\e[C\": forward-char\n\"\\e[D\": backward-char\n\
\"\\eOA\": previous-history\n\"\\eOB\": next-history\n\"\\eOC\": forward-char\n\"\\eOD\": backward-char\n\
\"\\e[1~\": beginning-of-line\n\"\\e[4~\": end-of-line\n\"\\e[H\": beginning-of-line\n\"\\e[F\": end-of-line\n\
\"\\e[3~\": delete-char\n\"\\e[5~\": history-search-backward\n\"\\e[6~\": history-search-forward\n\
\"\\e[1;5C\": forward-word\n\"\\e[1;5D\": backward-word\n\
# Ctrl+H: edit the system declaration (forge edit checks it and applies it when the editor closes).\n\
\"\\C-h\": \"sudo forge edit\\C-m\"\n";

/// Hardware watchdog drivers are not wanted: nothing here feeds them, and some report "watchdog did not stop!" on every
/// shutdown. (kmod reads /usr/lib/modprobe.d; a machine that wants one overrides it in /etc/modprobe.d.)
const WATCHDOG: &str = "# managed by forge: no hardware watchdogs by default\nblacklist iTCO_wdt\nblacklist iTCO_vendor_support\nblacklist sp5100_tco\nblacklist wdat_wdt\nblacklist softdog\nblacklist intel_oc_wdt\nblacklist ie6xx_wdt\nblacklist w83627hf_wdt\nblacklist nct6775_wdt\n";


const NETWORK_CONF: &str = "# Interfaces to manage, separated by spaces. Empty: every physical interface (wired and wireless).\n\
interfaces=\"\"\n";

const NETWORK_SERVICE: &str = r#"#!/sbin/openrc-run
description="Network: DHCP on wired interfaces, wpa_supplicant and DHCP on Wi-Fi"
command=/usr/libexec/singularity/netwatch
command_background=true
pidfile=/run/netwatch.pid

depend() {
	need localmount
	after bootmisc loopback modules-load
	provide net
}

start_pre() {
	# A radio that is soft-blocked stays off until something unblocks it.
	for f in /sys/class/rfkill/rfkill*/soft; do
		[ -w "$f" ] && echo 0 > "$f"
	done
	mkdir -p /run/netwatch
}

stop_post() {
	dhcpcd -x >/dev/null 2>&1
	for p in /run/netwatch/*; do
		[ -f "$p" ] || continue
		kill "$(cat "$p")" 2>/dev/null
		rm -f "$p"
	done
}
"#;

const NETWATCH: &str = r#"#!/bin/sh
# Keeps every network interface configured as it appears, until it is stopped: brings the link up,
# starts wpa_supplicant on wireless ones (when a configuration exists), and runs DHCP on all of them.
[ -r /etc/conf.d/network ] && . /etc/conf.d/network
RUN=/run/netwatch
mkdir -p "$RUN"

alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

while :; do
	for dir in /sys/class/net/*; do
		name=${dir##*/}
		[ "$name" = lo ] && continue
		if [ -n "$interfaces" ]; then
			case " $interfaces " in *" $name "*) ;; *) continue ;; esac
		else
			[ -e "$dir/device" ] || continue   # virtual: bridges, tunnels...
		fi
		ip link set "$name" up 2>/dev/null
		if [ -d "$dir/wireless" ] || [ -d "$dir/phy80211" ]; then
			if [ -f /etc/wpa_supplicant/wpa_supplicant.conf ] && ! alive "$RUN/wpa.$name"; then
				wpa_supplicant -B -i "$name" -D nl80211 -c /etc/wpa_supplicant/wpa_supplicant.conf \
					-P "$RUN/wpa.$name" >/dev/null 2>&1
			fi
		fi
	done
	# One DHCP client (dhcpcd) looks after every interface, including ones that appear later (a Wi-Fi card whose
	# firmware loads after boot), and waits for a carrier on its own. Started once; restarted if it ever dies.
	pgrep -x dhcpcd >/dev/null 2>&1 || dhcpcd -b -q >/dev/null 2>&1
	sleep 3
done
"#;

const COLDPLUG_SERVICE: &str = r#"#!/sbin/openrc-run
description="Loads drivers for the hardware found, and the modules listed in /etc/modules-load.d"

depend() {
	need procfs
	before localmount hostname
	provide modules-load
}

start() {
	ebegin "Loading drivers for detected hardware"
	# Every device offers a modalias; modprobe resolves it to the module that drives it.
	find /sys/devices -name modalias -type f -exec cat {} + 2>/dev/null | sort -u |
		while read -r alias; do
			modprobe -q "$alias" 2>/dev/null
		done
	for file in /usr/lib/modules-load.d/*.conf /etc/modules-load.d/*.conf; do
		[ -f "$file" ] || continue
		while read -r module _; do
			case "$module" in ''|\#*|\;*) continue ;; esac
			modprobe -q "$module" 2>/dev/null
		done < "$file"
	done
	eend 0
}
"#;

const WPA_CONF: &str = "ctrl_interface=/run/wpa_supplicant\n\
ctrl_interface_group=0\n\
update_config=1\n";

const WIFI_COMMAND: &str = r#"#!/bin/sh
# wifi scan | connect SSID [passphrase] | status | disconnect
# Changing the network settings needs root: ask for it here instead of failing on a file permission.
[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"
conf=/etc/wpa_supplicant/wpa_supplicant.conf

find_interface() {
	for d in /sys/class/net/*; do
		if [ -d "$d/wireless" ] || [ -d "$d/phy80211" ]; then echo "${d##*/}"; return; fi
	done
}

iface=$(find_interface)
if [ -z "$iface" ]; then
	echo "wifi: no wireless interface found." >&2
	echo "      Give the drivers a few seconds after boot, then check: dmesg | grep -i -E 'firmware|wlan|iwl|rtw|ath|mt7|brcm'" >&2
	exit 1
fi

# wpa_cli talks to a running wpa_supplicant. Normally the network service starts one; if it has not (yet), start it here,
# and say so when it cannot start.
ensure_supplicant() {
	wpa_cli -i "$iface" ping >/dev/null 2>&1 && return 0
	[ -f "$conf" ] || { echo "wifi: $conf is missing" >&2; return 1; }
	mkdir -p /run/netwatch
	wpa_supplicant -B -i "$iface" -D nl80211 -c "$conf" -P "/run/netwatch/wpa.$iface" || { echo "wifi: wpa_supplicant could not start on $iface" >&2; return 1; }
	n=0
	while [ "$n" -lt 10 ]; do
		wpa_cli -i "$iface" ping >/dev/null 2>&1 && return 0
		sleep 1; n=$((n + 1))
	done
	echo "wifi: wpa_supplicant did not come up on $iface" >&2
	return 1
}
ensure_supplicant || exit 1

case "$1" in
	scan)
		wpa_cli -i "$iface" scan >/dev/null
		sleep 4
		printf 'signal   security            network\n'
		wpa_cli -i "$iface" scan_results | tail -n +2 | sort -t "$(printf '\t')" -k3 -nr |
			awk -F '\t' '{ printf "%4s dBm %-19s %s\n", $3, substr($4, 1, 19), $5 }'
		;;
	connect)
		ssid=$2
		[ -n "$ssid" ] || { echo "usage: wifi connect SSID [passphrase]" >&2; exit 1; }
		pass=$3
		if [ -z "$pass" ]; then
			printf 'Passphrase for %s (empty for an open network): ' "$ssid" >&2
			stty -echo 2>/dev/null; read -r pass; stty echo 2>/dev/null; echo >&2
		fi
		if [ -z "$pass" ]; then
			printf 'network={\n\tssid="%s"\n\tkey_mgmt=NONE\n}\n' "$ssid" >> "$conf"
		else
			wpa_passphrase "$ssid" "$pass" | grep -v '#psk' >> "$conf" || exit 1
		fi
		wpa_cli -i "$iface" reconfigure >/dev/null
		printf 'Connecting to %s ' "$ssid"
		n=0
		while [ "$n" -lt 25 ]; do
			if wpa_cli -i "$iface" status | grep -q 'wpa_state=COMPLETED'; then
				echo "associated."
				sleep 3
				ip -4 addr show "$iface" | grep inet || echo "(no IP address yet: DHCP is still trying)"
				exit 0
			fi
			printf '.'; sleep 1; n=$((n + 1))
		done
		echo " not connected. Check the passphrase, and that the network is in range (wifi scan)."
		exit 1
		;;
	status)
		wpa_cli -i "$iface" status
		ip -4 addr show "$iface" | grep inet
		;;
	disconnect)
		wpa_cli -i "$iface" disconnect
		;;
	*)
		echo "usage: wifi scan | connect SSID [passphrase] | status | disconnect" >&2
		exit 1
		;;
esac
"#;


pub(crate) fn file(path: &str, content: &str, mode: u32, own: Own) -> Managed {
    Managed { path: path.to_string(), entry: Entry::File { content: content.to_string(), mode }, own }
}

pub(crate) fn link(path: String, target: String, own: Own) -> Managed {
    Managed { path, entry: Entry::Link(target), own }
}

/// What forge keeps right on a system with this init system.
pub fn managed(init: &str) -> Vec<Managed> {
    let mut v = vec![
        file("/usr/bin/wifi", WIFI_COMMAND, 0o755, Own::Always),
        file("/etc/inputrc", INPUTRC, 0o644, Own::Always),
        file("/usr/lib/modprobe.d/singularity-watchdog.conf", WATCHDOG, 0o644, Own::Always),
        file("/etc/wpa_supplicant/wpa_supplicant.conf", WPA_CONF, 0o600, Own::IfMissing),
    ];
    if init == "openrc" {
        v.push(file("/etc/init.d/network", NETWORK_SERVICE, 0o755, Own::Always));
        v.push(file("/etc/init.d/coldplug", COLDPLUG_SERVICE, 0o755, Own::Always));
        v.push(file("/usr/libexec/singularity/netwatch", NETWATCH, 0o755, Own::Always));
        v.push(file("/etc/conf.d/network", NETWORK_CONF, 0o644, Own::IfMissing));
        for (port, baud, term, opts) in AGETTY_PORTS {
            v.push(link(format!("/etc/init.d/agetty.{port}"), "agetty".into(), Own::Always));
            let conf = format!("baud=\"{baud}\"\nterm_type=\"{term}\"\nagetty_options=\"{opts}\"\n");
            v.push(file(&format!("/etc/conf.d/agetty.{port}"), &conf, 0o644, Own::IfMissing));
        }
        for (level, services) in OPENRC_RUNLEVELS {
            for s in *services {
                v.push(link(format!("/etc/runlevels/{level}/{s}"), format!("/etc/init.d/{s}"), Own::Always));
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrc_gets_its_services_and_both_inits_the_shared_defaults() {
        let o = managed("openrc");
        let s = managed("systemd");
        let has = |v: &[Managed], p: &str| v.iter().any(|m| m.path == p);
        assert!(has(&o, "/etc/init.d/network") && has(&o, "/etc/runlevels/default/chronyd") && has(&o, "/etc/init.d/agetty.tty1"));
        assert!(!has(&s, "/etc/init.d/network"), "systemd has its own");
        assert!(has(&s, "/usr/bin/wifi") && has(&s, "/etc/inputrc"));
    }

    #[test]
    fn programs_are_always_rewritten_and_your_config_never() {
        let o = managed("openrc");
        let own = |p: &str| o.iter().find(|m| m.path == p).unwrap().own;
        assert_eq!(own("/usr/bin/wifi"), Own::Always);
        assert_eq!(own("/etc/conf.d/network"), Own::IfMissing);
        assert_eq!(own("/etc/wpa_supplicant/wpa_supplicant.conf"), Own::IfMissing);
    }

    #[test]
    fn the_scripts_are_valid_shell() {
        use std::io::Write;
        for m in managed("openrc") {
            if let Entry::File { content, .. } = &m.entry {
                if content.starts_with("#!/bin/sh") {
                    let mut sh = std::process::Command::new("sh").arg("-n").stdin(std::process::Stdio::piped()).spawn().unwrap();
                    sh.stdin.take().unwrap().write_all(content.as_bytes()).unwrap();
                    assert!(sh.wait().unwrap().success(), "{} is not valid shell", m.path);
                }
            }
        }
    }
}
