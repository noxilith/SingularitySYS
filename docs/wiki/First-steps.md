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

## 4. Update

```bash
sudo forge upgrade
```

This moves every package (the base system included) to the newest version in the repository. The kernel is separate: see
[Kernel and graphics](Kernel-and-graphics).

## 5. Make it yours

Everything about the machine lives in one file. Edit it with:

```bash
sudo forge edit
```

(or press **Ctrl+H** in a shell). When you close the editor the file is checked and applied. See
[The declaration](The-declaration) for every setting, and [Packages](Packages) to install software.
