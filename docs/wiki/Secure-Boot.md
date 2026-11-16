# Secure Boot

Secure Boot makes the firmware refuse to start boot code that is not signed by a key it trusts. SingularitySYS supports it
with a **key of your own**: forge makes the key, signs the boot loader with it, and pins everything the loader may read.
No third party signs anything, and no Microsoft signature is involved.

> **Status.** This was built and tested against QEMU's Secure Boot firmware (boot with the signed loader, a changed kernel
> is refused, an unsigned loader is refused, updates re-sign correctly). It has not been tried on a real motherboard's
> firmware by the author, whose menus differ from vendor to vendor. If something in the firmware step does not match what
> you see, the [Troubleshooting](#troubleshooting) section and the issue tracker are the place.

## What it protects, and what it does not

It protects the **boot chain of a powered-off machine**: the boot loader, its menu, the kernel, the microcode image and the
wallpaper. Someone who swaps the kernel or edits the boot menu on your disk gets a machine that refuses to start.

It does **not** cover kernel modules or the root filesystem, and the key lives on your disk: anyone who has root on the
running system, or who steals `/etc/forge/secureboot/sb.key`, can sign boot files too. It is tamper protection for a
machine that is switched off, not a defence against a compromised running system.

## Requirements

- A **UEFI** installation (the boot partition is an EFI System Partition). BIOS machines cannot use Secure Boot.
- The current forge: `sudo forge upgrade`.
- A FAT32-formatted USB stick, for carrying the certificate to the firmware menu.
- Keep the live ISO on a stick as well, in case you need to recover.

## Set it up (Option A: add your key, keep the existing ones)

This is the recommended way. It **adds** your certificate to the firmware's list of trusted signatures and leaves the
factory keys (usually Microsoft's and the board maker's) alone. That matters because a graphics card's own start-up code
is often signed by Microsoft; removing those keys can blank the screen before Linux starts, and it stops other
operating systems from booting.

### 1. Prepare the boot files

```bash
sudo forge upgrade
sudo forge secureboot setup
```

`setup` makes the key and certificate in `/etc/forge/secureboot`, rewrites the boot menu so every file it loads carries
its BLAKE2b hash, and signs the boot loader. It also writes the files for the firmware to `/boot/EFI/forge-secureboot`:

| File | Use |
|---|---|
| `singularity.cer` | The certificate (DER). What most firmware menus want. |
| `db.esl` | The same certificate as a signature list. |
| `db.auth`, `KEK.auth`, `PK.auth` | Authenticated variables, for firmwares that enroll those. |

Back up `/etc/forge/secureboot/sb.key` somewhere safe. It is secret: never share it.

### 2. Reboot once and check that the machine still starts

Leave Secure Boot **off** for this. The signed loader works either way, so this proves the new menu and loader are good
before anything is enforced.

```bash
sudo forge secureboot status
sudo forge doctor
```

### 3. Copy the certificate to a USB stick

```bash
sudo mount /dev/sdX1 /mnt          # your FAT32 stick
sudo cp /boot/EFI/forge-secureboot/singularity.cer /mnt/
sudo umount /mnt
```

### 4. Add it in the firmware menu

Reboot into the firmware settings (the key is usually Del or F2 at power-on). Menus differ, but the path is the same shape
everywhere:

1. Open the **Secure Boot** page. If key management is hidden, set **Secure Boot Mode** (or **OS Type**) to **Custom** or
   **Other OS**.
2. Open **Key Management** and find the **Authorized Signatures** list, also called **`db`**.
3. Choose **Append** or **Enroll** (not Replace or Delete), and select `singularity.cer` on the USB stick.
4. Switch **Secure Boot** to **Enabled**.
5. Save and exit.

### 5. Check that it is on

```bash
sudo forge secureboot status
sudo forge doctor
```

`Secure Boot active now: yes`, and a doctor line saying the loader is signed with your key and the menu pins every boot
file, mean it worked. `dmesg | grep -i "secure boot"` shows the kernel's own confirmation.

## Option B: replace the keys with your own (setup mode)

Clearing all keys puts the firmware into **setup mode**, in which it accepts a new key from the running system:

1. In the firmware, use **Delete all Secure Boot keys** (or **Clear keys**, **Reset to Setup Mode**).
2. Boot, then run `sudo forge secureboot enroll`. It writes the certificate as the database key, the key-exchange key and
   the platform key.
3. Reboot, make sure Secure Boot is enabled in the firmware, and run `sudo forge secureboot status`.

This removes every other key, Microsoft's included. Expect Windows dual-boot to stop working, and on some machines the
screen to stay black during the firmware's own start-up (graphics card boot code). If so, switch Secure Boot off in the
firmware, or reset its settings.

If `forge secureboot status` shows `unknown` for setup mode, the firmware's variables are not mounted: the current forge
mounts them itself; on an older one run `sudo mount -t efivarfs efivarfs /sys/firmware/efi/efivars`.

## Living with it

forge keeps it current by itself:

- Every kernel install, kernel update, generation or rollback rewrites the boot menu, pins the new files and signs a loader
  for the new menu. Both are replaced one right after the other.
- When packages change, `forge apply` signs the new Limine loader from the package; it never puts the package's unsigned
  copy over the signed one.
- `forge secureboot refresh` does it by hand.

## Turning it off

Switch Secure Boot off in the firmware first, then:

```bash
sudo forge secureboot disable
```

This restores an unsigned loader and a menu without hashes. The key stays in the firmware until you remove it there.

## Troubleshooting

- **The machine does not start after enabling it.** That is the check working: something on the boot partition does not
  match what was signed or pinned. Switch Secure Boot **off** in the firmware and try again; if it boots, run
  `sudo forge secureboot refresh` and `sudo forge doctor`, then switch it back on. If a firmware reset is easier, clearing
  the CMOS removes the setting.
- **It does not start even with Secure Boot off.** The signed loader also checks the hash of the boot menu, so a menu
  and loader that disagree (for example after a power cut between the two steps of an update) stop it. Boot the live ISO,
  mount the boot partition, and copy the plain `/usr/share/limine/BOOTX64.EFI` from the live system over
  `EFI/BOOT/BOOTX64.EFI` on it. That loader does not check the menu's hash. Boot the installed system with Secure Boot
  off and run `sudo forge secureboot refresh`.
- **The screen is black before Linux appears (Option B).** Probably the graphics card's boot code, signed by a key you
  removed. Use Option A, or switch Secure Boot off.
- **The firmware will not take `singularity.cer`.** Some boards want a different format or file name: try `db.esl` or
  `db.auth`, or tell us the board and the exact menu wording.
- **`forge doctor` says the loader is not signed.** Run `sudo forge secureboot refresh`.
