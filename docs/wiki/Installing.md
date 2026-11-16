# Installing

Installing copies the live system onto two partitions you make. **It erases those partitions.** Back up first, and
install on a machine or disk you can afford to lose while the distribution is this young.

## What you need

- A USB stick (the ISO is about 600 MB).
- A computer that boots **UEFI** or **BIOS**. UEFI is needed for [Secure Boot](Secure-Boot).
- A network connection, wired or Wi-Fi, for the first `forge apply` after installing.

## 1. Get the ISO

The ISO is not published as a download yet; this page will link it when it is. Until then build it yourself:
see [Building from source](Building-from-source).

## 2. Write it to the USB stick

On another Linux machine (replace `/dev/sdX` with the **stick**, not a disk you care about; check with `lsblk`):

```bash
sudo dd if=singularity-live.iso of=/dev/sdX bs=4M status=progress conv=fsync
```

## 3. Boot it

Boot from the stick (usually F8, F11, F12 or Esc at power-on). The Limine menu appears; the first entry starts the system.
The live system copies itself into RAM and runs from there, so you can remove the stick afterwards. Log in as `root`,
password `root`.

## 4. Make two partitions

Use `cfdisk` on the disk you are installing to (for example `/dev/nvme0n1` or `/dev/sda`):

| Partition | Size | Type |
|---|---|---|
| Boot | about 512 MB | EFI System (for UEFI) or any small partition (for BIOS) |
| Root | the rest | Linux filesystem |

Write the table and note the two device names (for example `/dev/sda1` and `/dev/sda2`).

## 5. Install

```bash
forge strap /dev/sda2 /dev/sda1
```

The **root partition comes first, then the boot partition**. forge works out by itself whether the machine started in UEFI
or BIOS mode (`--uefi` and `--bios` force it). It will ask you to create your user and set passwords. It then:

1. formats the boot partition as FAT32 and the root as btrfs, with the subvolumes `@`, `@snapshots`, `@home`, `@store`;
2. copies the system over;
3. writes `/etc/fstab` and a starter `/etc/forge/system.toml`;
4. installs the Limine boot loader and writes its menu;
5. creates your user (in the `wheel` group, which may use `sudo`).

Useful options: `--hostname NAME`, `--yes` (do not ask for confirmation), `--no-users` (skip the account questions; root
keeps the password `root`), `--dry-run` (show the plan only).

## 6. Reboot

Remove the stick and reboot. Continue with [First steps](First-steps).
