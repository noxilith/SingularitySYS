# First steps

After the first boot, log in as the user you made.

## 1. Get online

Wired networks work by themselves (DHCP). For Wi-Fi:

```bash
wifi scan
wifi connect "My Network" "the passphrase"
wifi status
```

`wifi` asks for `sudo` itself. The network is remembered.

## 2. Run `forge apply` once

A fresh install does not have everything the declaration asks for yet (the firewall, compressed swap, the maintenance
service). One apply creates it:

```bash
sudo forge apply
```

It shows the plan and asks before it does anything, and takes a generation (a snapshot) first.

## 3. Check the system

```bash
sudo forge doctor
```

Run it with `sudo`: `/boot` and some files in `/etc` can only be read by root, and without it doctor cannot check them.
It lists what is wrong and the command that fixes it. `everything looks right` means exactly that.

## 4. Update, when you decide

**Nothing on this system updates by itself, and nothing nags you to.** No background updater, no notifications. You choose
when:

```bash
sudo forge upgrade
```

This moves every package (the base system included) to the newest version in the repository, after showing you what will
change and taking a generation you can roll back to. `forge install`, `forge remove`, `forge apply` and `forge edit`
**never** change a package you already have: installing `htop` adds `htop` and leaves everything else exactly as it is. The
kernel is separate: see [Kernel and graphics](Kernel-and-graphics). To see whether something newer exists, `forge search NAME`
shows the installed version next to the repository's.

If installing something needs a different build of a library than the one you have, forge says so and asks you to run
`sudo forge upgrade` first, instead of upgrading behind your back.

## 5. Make it yours

Everything about the machine lives in one file. Edit it with:

```bash
sudo forge edit
```

(or press **Ctrl+H** in a shell). When you close the editor the file is checked and applied. See
[The declaration](The-declaration) for every setting, and [Packages](Packages) to install software.

## 6. Read the manuals

Every program has its manual page, and the system calls and library functions are documented too:

```bash
man ls              # a program
man 2 open          # section 2: system calls
man 5 fstab         # section 5: file formats
apropos network     # search the manual page titles
```
