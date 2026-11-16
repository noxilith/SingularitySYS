# SingularitySYS

SingularitySYS is an independent Linux distribution built around one program, **forge**. The whole machine is described by
one file, `/etc/forge/system.toml`; `forge apply` makes the system match it. The root filesystem is btrfs, and every
change takes a snapshot (a **generation**) you can boot from the Limine menu if something goes wrong. Packages are
signed binaries from a GitHub release; the base is glibc, with OpenRC as the default init system (systemd is a setting).

> **Early software.** It has been installed and used on the author's hardware and very little else. Do not put it on a
> machine you cannot reinstall.

## Start here

| If you want to... | Read |
|---|---|
| Install it on a disk | [Installing](Installing) |
| Do the first things after the first boot | [First steps](First-steps) |
| Understand and edit `system.toml` | [The declaration](The-declaration) |
| Install, remove, upgrade software | [Packages](Packages) |
| Go back to an earlier state | [Generations and rollback](Generations-and-rollback) |
| Update or build the kernel, use a graphics card | [Kernel and graphics](Kernel-and-graphics) |
| Wi-Fi, firewall, port forwarding, SSH | [Networking](Networking) |
| Swap, trimming, scrubbing, old generations | [Maintenance](Maintenance) |
| Switch between OpenRC and systemd | [Init systems](Init-systems) |
| Boot with Secure Boot on, with a key of your own | [Secure Boot](Secure-Boot) |
| Something is wrong | [Troubleshooting](Troubleshooting) |
| Build packages or the ISO yourself | [Building from source](Building-from-source) |

## What works, and what does not

Checked on real hardware by the author: the live ISO (boots into RAM, Wi-Fi, verified downloads), installing to disk and
booting the result, `forge apply` and package upgrades, the firewall, zram swap, and the AMD graphics driver with its
firmware package.

Checked only in a virtual machine (QEMU): systemd as the init system, Secure Boot, kernel updates with rollback.

Not included, on purpose or not yet: a desktop environment (graphics libraries such as Mesa, a compositor, audio,
Bluetooth, power management are future packages), disk encryption (decided against), a system logger (decided against).

SingularitySYS is free software under the GNU General Public License, version 3 or later.

The full technical record, including what was not verified, is in
[DESIGN.md](https://github.com/noxilith/SingularitySYS/blob/main/DESIGN.md).
