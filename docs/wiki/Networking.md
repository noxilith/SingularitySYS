# Networking

## Wired and Wi-Fi

Wired interfaces get an address by DHCP automatically. Wi-Fi:

```bash
wifi scan
wifi connect "SSID" "passphrase"
wifi status
wifi disconnect
```

Drivers for every Wi-Fi vendor the kernel has are included as modules, and the matching firmware comes with the base
system. Networks you join are saved in `/etc/wpa_supplicant/wpa_supplicant.conf`.

## The firewall

A firewall (nftables) is on by default. Everything arriving from outside is **dropped**, except answers to connections this
machine started, loopback, ping, and DHCP. Allow more in `/etc/forge/system.toml`:

```toml
[firewall]
enable = true
allow  = ["ssh", "443/tcp", "5353/udp", "8000-8100/tcp"]
```

An entry is a name (`ssh`, `http`, `https`, `dns`), a port (`443`, which means TCP and UDP), a port with a protocol
(`443/tcp`, `5353/udp`) or a range (`8000-8100/tcp`). `enable = false` turns the firewall off. The rules are in
`/etc/nftables.conf`, generated from the declaration; edit the declaration, not that file. Check them with
`sudo nft list ruleset`.

### Port forwarding

Traffic that arrives on a port of this machine can be sent to another machine:

```toml
[[firewall.forward]]
port      = 8080
to        = "192.168.1.50:80"
proto     = "tcp"          # the default; or "udp"
interface = "enp3s0"       # optional: only traffic arriving on this interface
```

Add a block per forwarded port. The target is an IPv4 address and port. forge turns on IP forwarding only when something
is forwarded, and masquerades the forwarded traffic.

## SSH

```toml
packages = "openssh"

[firewall]
allow = ["ssh"]
```

Installing `openssh` also sets up the server: an `sshd_config` (created only if you have none, so your edits are never
overwritten), host keys on first start, and the service, enabled. Defaults are conservative: **root cannot log in**, no PAM,
no X11 forwarding; password login is on, so add your public key to `~/.ssh/authorized_keys` and consider setting
`PasswordAuthentication no` in `/etc/ssh/sshd_config`. Remember the firewall: nothing arrives on port 22 until
`allow = ["ssh"]`.
