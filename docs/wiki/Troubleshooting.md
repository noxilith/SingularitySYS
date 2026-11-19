# Troubleshooting

Start with `sudo forge doctor`. Run it with `sudo`: `/boot` and some files in `/etc` can only be read by root. It checks the
init system, boot entries, base packages, users, services, clock, microcode, Secure Boot, and says which command fixes each
problem.

There is **no system logger** (decided against). Use `dmesg` for kernel messages; under systemd `journalctl -b` works.

## Network

- **`forge` says it could not download the index.** There is no network. Wired: `ip addr`, then check the cable and that
  the router gives out addresses. Wi-Fi: `wifi status`, `wifi scan`, then `wifi connect SSID PASSPHRASE`.
- **Wi-Fi shows nothing.** The adapter's firmware may be missing or the radio blocked; `wifi status` and `dmesg | grep -i
  firmware` say which.

## After installing

- **`nft list ruleset` or `swapon --show` print nothing.** The firewall and swap services are created by
  `sudo forge apply`; run it once after the first boot. The firewall also needs a kernel with nf_tables, so if you
  installed from an old ISO run `sudo forge kernel update` and reboot.
- **A failed `serial-getty@ttyS0.service` under systemd.** Fixed in the current forge: `sudo forge upgrade && sudo forge
  apply`. (Your machine has no serial port; the login on it is now only started where the port exists.)
- **`forge doctor` prints warnings about `/boot` or `nftables.conf` as a normal user.** Run it with `sudo`.

## Boot problems

- **Something broke after an update.** Reboot and pick the previous entry under **Generations** in the Limine menu. See
  [Generations and rollback](Generations-and-rollback).
- **The screen goes black after adding graphics firmware.** Boot an earlier generation, then check that the firmware package
  matches the card (see [Kernel and graphics](Kernel-and-graphics)).
- **`reboot` or `poweroff` does nothing right after switching init systems.** Current forge keeps the old init system's
  tools installed until you have rebooted; with an older forge, `sudo forge upgrade` first. A hard reset always works.
- **Repairing from the live ISO.** Boot the ISO, then mount the system and look around:

  ```bash
  mount -o subvol=@ /dev/sdX2 /mnt
  mount /dev/sdX1 /mnt/boot
  ```

## Packages

- **`forge apply` refuses to proceed with a message about a file.** forge does not overwrite a file in `/usr` that it did not
  put there; the message names the file. Move it away, or run with `--adopt` if you want forge to take it over.
- **The declaration is invalid.** `sudo forge edit` checks the file when you close the editor, tells you what is wrong and
  offers to edit it again; if you say no, the file is left unchanged.
- **Disk space.** `sudo forge store gc` frees packages nothing uses; `sudo forge gen prune --keep 3` removes old
  generations.

## Still stuck

Open an issue at <https://github.com/noxilith/SingularitySYS/issues> with the output of `sudo forge doctor`, what you ran,
and what you expected.

## Send a report with `sysreport`

`sudo sysreport` gathers what is needed to diagnose a problem (versions, kernel messages, boot and service state, the desktop's
logs, disks and graphics hardware) into one text report, shows how big it is, and asks before uploading it to a paste service
(dpaste, kept for 7 days) and printing the link. Give that link to whoever is helping you.

```bash
sudo sysreport             # collect, ask, upload, print the link
sudo sysreport --save      # only write the report to a file, nothing leaves the machine
sudo sysreport --service paste.rs
```

Left out on purpose: password hashes, Wi-Fi passphrases, SSH keys, your files, saved network connections; MAC addresses are
masked. Anyone with the link can read it, so share it only with the person helping. If there is no network the report is saved
under `/tmp` so it can be copied over on a USB stick. It is part of the base system, so `sudo forge upgrade` brings it in.
