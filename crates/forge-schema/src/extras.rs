//! The firewall, swap and maintenance sections of the declaration, and what forge writes for them.
//!
//! ```toml
//! swap = "zram"                      # compressed swap in memory ("none" for no swap)
//!
//! [firewall]
//! enable = true                      # inbound traffic is dropped unless it is allowed below
//! allow  = ["ssh", "443/tcp"]        # open to this machine: names (ssh, http, https, dns) or PORT[/tcp|udp] or A-B/tcp
//!
//! [[firewall.forward]]               # port forwarding: traffic arriving on `port` goes to another machine
//! port  = 8080
//! to    = "192.168.1.50:80"
//! proto = "tcp"                      # the default
//!
//! [maintenance]
//! enable           = true            # trim the SSD, scrub btrfs, prune old generations, clean the store
//! keep_generations = 10
//! ```

use serde::{Deserialize, Serialize};

use crate::SchemaError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    /// The port that receives the traffic.
    pub port: u16,
    /// Where it goes: `IPV4:PORT`.
    pub to: String,
    #[serde(default = "tcp")]
    pub proto: String,
    /// Only traffic arriving on this interface (default: any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

fn tcp() -> String {
    "tcp".into()
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirewallDecl {
    #[serde(default = "yes")]
    pub enable: bool,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub forward: Vec<Forward>,
}

impl Default for FirewallDecl {
    fn default() -> Self {
        Self { enable: true, allow: Vec::new(), forward: Vec::new() }
    }
}

fn ten() -> u32 {
    10
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceDecl {
    #[serde(default = "yes")]
    pub enable: bool,
    /// How many generations to keep when old ones are pruned (the booted one and the newest are always kept).
    #[serde(default = "ten")]
    pub keep_generations: u32,
}

impl Default for MaintenanceDecl {
    fn default() -> Self {
        Self { enable: true, keep_generations: 10 }
    }
}

/// Swap kinds: `zram` (compressed, in memory) or `none`.
pub const SWAP_KINDS: &[&str] = &["zram", "none"];

/// One allowed port range with its protocol(s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allow {
    pub first: u16,
    pub last: u16,
    /// `tcp`, `udp`, or both (for a bare port number).
    pub protos: Vec<&'static str>,
}

fn port(s: &str) -> Result<u16, SchemaError> {
    match s.trim().parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(SchemaError::Invalid(format!("`{s}` is not a port (1-65535)"))),
    }
}

/// `ssh`, `443`, `443/tcp`, `5353/udp`, `8000-8100/tcp`.
pub fn parse_allow(rule: &str) -> Result<Allow, SchemaError> {
    let named = match rule {
        "ssh" => Some("22/tcp"),
        "http" => Some("80/tcp"),
        "https" => Some("443/tcp"),
        "dns" => Some("53"),
        _ => None,
    };
    let rule = named.unwrap_or(rule);
    let (ports, proto) = match rule.split_once('/') {
        Some((p, "tcp")) => (p, vec!["tcp"]),
        Some((p, "udp")) => (p, vec!["udp"]),
        Some((_, other)) => return Err(SchemaError::Invalid(format!("`{rule}`: the protocol must be tcp or udp, not `{other}`"))),
        None => (rule, vec!["tcp", "udp"]),
    };
    let (first, last) = match ports.split_once('-') {
        Some((a, b)) => (port(a)?, port(b)?),
        None => (port(ports)?, port(ports)?),
    };
    if first > last {
        return Err(SchemaError::Invalid(format!("`{rule}`: the range runs backwards")));
    }
    Ok(Allow { first, last, protos: proto })
}

/// `IPV4:PORT`.
pub fn parse_target(to: &str) -> Result<(std::net::Ipv4Addr, u16), SchemaError> {
    let (ip, p) = to.rsplit_once(':').ok_or_else(|| SchemaError::Invalid(format!("`{to}`: expected IPADDRESS:PORT, for example 192.168.1.50:80")))?;
    let ip: std::net::Ipv4Addr = ip.parse().map_err(|_| SchemaError::Invalid(format!("`{to}`: `{ip}` is not an IPv4 address")))?;
    Ok((ip, port(p)?))
}

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 15 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl FirewallDecl {
    pub fn validate(&self) -> Result<(), SchemaError> {
        for a in &self.allow {
            parse_allow(a)?;
        }
        for f in &self.forward {
            port(&f.port.to_string())?;
            parse_target(&f.to)?;
            if f.proto != "tcp" && f.proto != "udp" {
                return Err(SchemaError::Invalid(format!("port forwarding {}: the protocol must be tcp or udp, not `{}`", f.port, f.proto)));
            }
            if f.interface.as_deref().is_some_and(|i| !name_ok(i)) {
                return Err(SchemaError::Invalid(format!("port forwarding {}: `{}` is not an interface name", f.port, f.interface.as_deref().unwrap_or(""))));
            }
        }
        Ok(())
    }
}

/// `/etc/nftables.conf`: drop what comes in unless it answers something we sent or is allowed, and forward ports.
pub fn nftables_conf(fw: &FirewallDecl) -> String {
    let mut s = String::from(
        "#!/usr/sbin/nft -f\n\
         # managed by forge from the [firewall] section of /etc/forge/system.toml: edit that, not this file\n\
         flush ruleset\n\n\
         table inet forge {\n\
         \tchain input {\n\
         \t\ttype filter hook input priority 0; policy drop;\n\
         \t\tct state established,related accept\n\
         \t\tct state invalid drop\n\
         \t\tiifname \"lo\" accept\n\
         \t\tip protocol icmp accept\n\
         \t\tip6 nexthdr icmpv6 accept\n\
         \t\tudp sport 67 udp dport 68 accept\n\
         \t\tudp sport 547 udp dport 546 accept\n",
    );
    for a in &fw.allow {
        let Ok(a) = parse_allow(a) else { continue };
        let ports = if a.first == a.last { a.first.to_string() } else { format!("{}-{}", a.first, a.last) };
        for p in &a.protos {
            s.push_str(&format!("\t\t{p} dport {ports} accept\n"));
        }
    }
    s.push_str("\t}\n\n\tchain forward {\n\t\ttype filter hook forward priority 0; policy drop;\n\t\tct state established,related accept\n");
    if !fw.forward.is_empty() {
        s.push_str("\t\tct status dnat accept\n");
    }
    s.push_str("\t}\n\n\tchain output {\n\t\ttype filter hook output priority 0; policy accept;\n\t}\n}\n");
    if !fw.forward.is_empty() {
        s.push_str("\ntable ip forge_nat {\n\tchain prerouting {\n\t\ttype nat hook prerouting priority -100; policy accept;\n");
        for f in &fw.forward {
            let iface = f.interface.as_deref().map(|i| format!("iifname \"{i}\" ")).unwrap_or_default();
            s.push_str(&format!("\t\t{iface}{} dport {} dnat to {}\n", f.proto, f.port, f.to));
        }
        s.push_str("\t}\n\n\tchain postrouting {\n\t\ttype nat hook postrouting priority 100; policy accept;\n\t\tct status dnat masquerade\n\t}\n}\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_rules_parse_names_ports_ranges_and_protocols() {
        assert_eq!(parse_allow("ssh").unwrap(), Allow { first: 22, last: 22, protos: vec!["tcp"] });
        assert_eq!(parse_allow("443/tcp").unwrap().protos, ["tcp"]);
        assert_eq!(parse_allow("5353/udp").unwrap().protos, ["udp"]);
        assert_eq!(parse_allow("53").unwrap().protos, ["tcp", "udp"]);
        let r = parse_allow("8000-8100/tcp").unwrap();
        assert_eq!((r.first, r.last), (8000, 8100));
        for bad in ["", "0", "70000", "22/icmp", "9-3/tcp", "ftp", "22/"] {
            assert!(parse_allow(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn forward_targets_are_ipv4_and_port() {
        assert_eq!(parse_target("192.168.1.50:80").unwrap(), ("192.168.1.50".parse().unwrap(), 80));
        for bad in ["192.168.1.50", "host:80", "192.168.1.50:0", "300.1.1.1:80"] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_default_firewall_drops_inbound_and_allows_replies_loopback_and_dhcp() {
        let c = nftables_conf(&FirewallDecl::default());
        assert!(c.contains("policy drop") && c.contains("ct state established,related accept") && c.contains("iifname \"lo\" accept"));
        assert!(c.contains("udp sport 67 udp dport 68 accept"), "DHCP replies must get through: {c}");
        assert!(!c.contains("forge_nat"), "no NAT table without forwards");
    }

    #[test]
    fn allowed_ports_and_forwards_become_rules() {
        let fw = FirewallDecl {
            enable: true,
            allow: vec!["ssh".into(), "8000-8100/tcp".into(), "53".into()],
            forward: vec![Forward { port: 8080, to: "192.168.1.50:80".into(), proto: "tcp".into(), interface: Some("enp3s0".into()) }],
        };
        fw.validate().unwrap();
        let c = nftables_conf(&fw);
        assert!(c.contains("tcp dport 22 accept") && c.contains("tcp dport 8000-8100 accept") && c.contains("udp dport 53 accept"), "{c}");
        assert!(c.contains("iifname \"enp3s0\" tcp dport 8080 dnat to 192.168.1.50:80") && c.contains("ct status dnat accept") && c.contains("masquerade"), "{c}");
    }

    #[test]
    fn a_bad_forward_is_refused() {
        let mut fw = FirewallDecl::default();
        fw.forward.push(Forward { port: 80, to: "nowhere".into(), proto: "tcp".into(), interface: None });
        assert!(fw.validate().is_err());
        fw.forward[0] = Forward { port: 80, to: "10.0.0.2:80".into(), proto: "sctp".into(), interface: None };
        assert!(fw.validate().is_err());
        fw.forward[0] = Forward { port: 80, to: "10.0.0.2:80".into(), proto: "tcp".into(), interface: Some("bad name".into()) };
        assert!(fw.validate().is_err());
    }
}
