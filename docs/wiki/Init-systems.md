# Init systems

OpenRC is the default. systemd is a setting:

```toml
init = "systemd"
```

## Switching

1. Edit the declaration (`sudo forge edit`) and apply.
2. forge installs the new init system and its plumbing, and keeps the old one installed so that `reboot` and `poweroff`
   keep working until you have switched.
3. **Reboot.** The machine starts with the new init system.
4. Run `sudo forge apply` **once more**. It removes the old init system's packages.

The generation taken before the switch still boots the old init system from the Limine menu, if you change your mind.
Switching back is the same procedure with `init = "openrc"`.

## Notes

- Services you list under `services` are enabled in whichever init system is active (OpenRC service names, or systemd
  unit names).
- The services forge adds (firewall, swap, maintenance, sshd) exist for both.
- Under systemd a login on the serial console is only started where the machine has that serial port.
- systemd as the init system has been verified in a virtual machine and not on real hardware by the author; report
  anything odd.

## Network under systemd

Both init systems bring the network up with the same small engine (`netwatch`: DHCP through `dhcpcd` on wired and Wi-Fi interfaces,
`wpa_supplicant` on Wi-Fi), so a machine that has network under one has it under the other, and the `wifi` command works the
same. Under systemd it runs as the `forge-network` service; `systemd-networkd` and `systemd-resolved` are masked, and
`/etc/resolv.conf` is a plain file that DHCP writes. (With the Vesper desktop installed NetworkManager takes over instead.)

**Switching safely:** get the newest forge while you still have network, then switch.

```bash
sudo forge upgrade        # on your current system, with network
sudo forge apply          # with init = "systemd" in system.toml: prepares the systemd side
sudo reboot
```
