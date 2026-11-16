# Packages

Packages are signed binary archives in the project's GitHub release. forge checks the signature of the index and the hash
of every archive, and refuses a repository that goes backwards in time.

## Everyday commands

```bash
forge search               # everything the repository offers
forge search htop          # packages whose name contains "htop"
forge list                 # what you installed, and the whole system profile
sudo forge install htop tmux
sudo forge remove tmux
sudo forge upgrade         # newest version of everything, the base system included
```

`install` and `remove` edit the `packages` list in `/etc/forge/system.toml` and apply it, so the file always matches the
machine.

## What is available

The base system is implied (glibc, bash and dash, GNU coreutils and friends, util-linux, shadow, sudo, kmod, iproute2,
dhcpcd, wpa_supplicant, btrfs-progs, Limine, nftables, curl, the micro editor, ...). Packages you can add include:

| Package | What it is |
|---|---|
| `git`, `tmux`, `htop`, `nano` | The usual tools. |
| `openssh` | SSH client and server. See [Networking](Networking). |
| `qemu` | Headless x86-64 emulator with KVM, for testing images. |
| `python`, `gcc`, `make`, `bison`, `flex`, `perl`, `meson`, `ninja`, `bc` | Languages and build tools. |
| `systemd`, `dbus` | Installed for you when `init = "systemd"`. |
| `linux-firmware-amdgpu`, `linux-firmware-intel-gpu`, `linux-firmware-nvidia` | Graphics firmware. See [Kernel and graphics](Kernel-and-graphics). |

`forge search` is the authoritative list.

## How installation works

Packages live in an immutable store (`/forge/store`); the system's files are real copies placed into `/usr` from it. A
package never touches `/etc` configuration you have edited, and forge never replaces the kernel files or the init system's
plumbing behind your back. `sudo forge store gc` frees the space of packages nothing uses any more (the
[maintenance](Maintenance) service does it weekly).

## Your own packages

Packages are built from recipes in a sandbox. See [Building from source](Building-from-source).
