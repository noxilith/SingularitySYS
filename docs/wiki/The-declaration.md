# The declaration

`/etc/forge/system.toml` describes the machine. Edit it with `sudo forge edit` (or Ctrl+H): the file is checked when the
editor closes, you see the plan, and `forge apply` makes the system match. `sudo forge apply --dry-run` shows the plan
without doing it. Every apply takes a [generation](Generations-and-rollback) first.

The base system (shell, networking, forge itself, and so on) is always installed and is **not** listed. The file lists
only what you chose.

## A complete example

```toml
hostname = "nova"
libc     = "glibc"
timezone = "Europe/Paris"
locale   = "en_US.UTF-8"
keymap   = "us"
init     = "openrc"          # or "systemd"
shell    = "bash"            # or fish, zsh, ...: installed for you
repos    = ["https://github.com/noxilith/SingularitySYS/releases/download/repo-glibc"]

packages = """
  git,
  htop,      # a comment
  openssh,
"""

services       = []
kernel_modules = []
swap           = "zram"

[firewall]
enable = true
allow  = ["ssh", "443/tcp"]

[[firewall.forward]]
port = 8080
to   = "192.168.1.50:80"

[maintenance]
enable           = true
keep_generations = 10

[[users]]
name   = "ada"
groups = ["wheel", "audio", "video"]
```

## Settings

| Setting | Meaning |
|---|---|
| `hostname` | The machine's name. |
| `libc` | `glibc` (the only one the distribution supports). |
| `timezone` | A name from `/usr/share/zoneinfo`, such as `Europe/Paris`. Default `UTC`. |
| `locale` | Default `en_US.UTF-8`. |
| `keymap` | Console keyboard layout. Default `us`. |
| `init` | `openrc` or `systemd`. See [Init systems](Init-systems). |
| `shell` | Login shell for root and for users who name none. `bash` is part of the base; anything else is installed for you. |
| `repos` | Where packages come from: a URL, or a local directory (searched in order). |
| `packages` | What you chose. A list, or names separated by commas and new lines in one `"""` string. Lines may carry `#` comments. |
| `services` | Services to start at boot (OpenRC service names, or systemd units). |
| `kernel_modules` | Kernel modules to load at boot. |
| `swap` | `zram` (compressed swap in memory, half the RAM up to 4 GiB, the default) or `none`. |
| `[firewall]` | See [Networking](Networking). On by default; inbound traffic is dropped unless allowed. |
| `[maintenance]` | See [Maintenance](Maintenance). |
| `[[users]]` | One block per user: `name`, `groups`, and optionally `shell`. Users and groups are created, and removed from groups, to match. |

Unknown keys are an error, so a typo is caught when you save rather than silently ignored.

## Commands that edit the file for you

`sudo forge install NAME` and `sudo forge remove NAME` change the `packages` list in place (comments and layout are kept)
and apply it.

## Language and keyboard

- **`locale`**: any of 78 UTF-8 locales is installed, including `en_US`, `en_GB`, `de_DE`, `fr_FR`, `es_ES`, `it_IT`,
  `pt_BR`, `nl_NL`, `pl_PL`, `ru_RU`, `uk_UA`, `tr_TR`, `el_GR`, `ja_JP`, `ko_KR`, `zh_CN`, `zh_TW`, `ar_SA`, `he_IL`,
  `hi_IN`, `th_TH`, `vi_VN` and more. `locale -a` lists them all. Write it as `de_DE.UTF-8`.
- **`keymap`**: the keyboard layout of the text console: a name from `/usr/share/keymaps` such as `us`, `uk`, `de`, `fr`,
  `es`, `it`, `br-abnt2`, `ru`, `dvorak`. (`find /usr/share/keymaps -name '*.map.gz'` lists them.) A graphical desktop
  chooses its own layout.
- `sudo forge doctor` checks that the locale and layout you wrote exist.
