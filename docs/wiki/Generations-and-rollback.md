# Generations and rollback

The root filesystem is btrfs. Before every change forge makes, it takes a read-only snapshot of the system: a
**generation**. Each generation has its own entry in the Limine boot menu, under **Generations**, and boots the kernel
and system exactly as they were.

## Going back

If an update leaves the machine broken or unpleasant:

1. Reboot and pick the generation you want from the Limine menu (the newest is the state just before the last change).
2. It boots read-only. If it is what you want to keep, make it permanent:

```bash
sudo forge gen rollback N      # N is the generation number
# forge mounts the btrfs top-level volume itself for the length of the command (no --top needed)
```

A rollback finishes after a reboot; then run `sudo forge gen rewrite-limine`, and after one more reboot
`sudo forge gen cleanup-next`. forge prints these steps when it needs them.

## Commands

```bash
forge gen list                 # the generations; * marks the one you booted
forge gen current              # the booted generation and subvolume
sudo forge gen create "before I try X"
sudo forge gen remove 3        # one generation (or `all`); the current and booted ones are kept
sudo forge gen prune --keep 10 # remove the oldest, keep the newest 10
```

The [maintenance](Maintenance) service prunes old generations weekly (`keep_generations`, default 10).

## Kernels and generations

Every kernel build has its own name (for example `7.3.3-r7`): its boot file and its modules sit beside the older kernel's.
A generation made before a kernel update boots the **old kernel with the old modules**. Kernels installed before this
scheme keep the plain name (`7.3.3`) and are replaced in place by an update, so only newer ones can be rolled back
completely.

## What is not in a generation

`/home` is its own subvolume and is not rolled back. The boot partition holds kernels and the boot loader; they are kept
side by side rather than snapshotted.
