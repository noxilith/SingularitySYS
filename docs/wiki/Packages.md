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

The base system is implied (glibc, bash and dash, GNU coreutils and friends, util-linux, shadow, sudo, kmod, iproute2, kbd,
dhcpcd, wpa_supplicant, btrfs-progs, Limine, nftables, curl, the micro editor, the `man` command and the manual pages, ...).
Packages you can add include:

| Package | What it is |
|---|---|
| `git`, `tmux`, `htop`, `nano`, `vim` | The usual tools. `vim` is the full editor with its syntax files and help. |
| `strace`, `lsof`, `file`, `tree`, `patch` | Everyday inspection and patching tools. |
| `zip`, `libarchive` | `zip` makes .zip files; `libarchive` provides `unzip`, `bsdtar`, `bsdcpio` and the library behind them. |
| `openssh` | SSH client and server. See [Networking](Networking). |
| `fetch` | The spinning-logo system information tool (<https://github.com/noxilith/Fetch>). Run `fetch`; `fetch --frames 1` draws one frame and exits. |
| `vesper` | The Vesper desktop environment (<https://github.com/noxilith/VesperDE>): Wayland compositor with scrollable tiling, bar, launcher, notifications, lock screen, settings and file manager. Installing it also installs what it uses: Xwayland (X11 apps), the screen-sharing portals (`xdg-desktop-portal`, `-wlr`, `slurp`), PipeWire and WirePlumber (sound, `wpctl`), NetworkManager (Wi-Fi), BlueZ (Bluetooth), brightnessctl, power-profiles-daemon and UPower, polkit (password prompts), udisks2 (mounting drives), greetd (login screen), tesseract with English data (copy text from the screen), wf-recorder and ffmpeg (screen recording), libnotify (`notify-send`), `foot` (the default terminal), fontconfig and the fonts (JetBrains Mono Nerd Font, DejaVu, Noto Color Emoji), Mesa (Intel, AMD and software rendering), libinput and PAM. Start it from a text console with `vesper-session`. **Early and untested on real hardware.** Set-up: [Vesper](Vesper). |
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

## Updates are your decision

Nothing here updates by itself, and nothing notifies you about updates. `forge install`, `forge remove`, `forge apply`
and `forge edit` never change a package you already have; only `sudo forge upgrade` does, when you run it. See
[First steps](First-steps).

## Removing a package, and its settings

`sudo forge remove NAME` removes the package and whatever only it needed, but **keeps its settings**: files it put under `/etc`
(which you may have edited) and anything it saved in `/var`. That way installing it again picks up where you left off.

`sudo forge remove --full NAME` also deletes those settings: the files the package installed under `/etc`, and its directories
`/etc/NAME`, `/var/lib/NAME`, `/var/cache/NAME` and `/var/log/NAME`, for the package and for everything that was removed with it.
The preview lists each one before you confirm. It never deletes forge's own files, anything a package that stays provides, or
your home directory (`~/.config/...` is yours: delete it by hand if you want it gone).
