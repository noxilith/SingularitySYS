# Kernel and graphics

## The kernel

The kernel is not part of the declaration. `forge apply` and `forge upgrade` never replace kernel files; kernels are
installed explicitly, so a kernel change is always something you chose.

### Update to the kernel the project publishes

```bash
sudo forge kernel update
sudo reboot
```

This downloads the newest `linux` package from your repository (signature and hash checked), takes a generation, installs
the kernel and its modules, and rewrites the boot menu. The new kernel becomes the default and the old one stays in the
menu under Generations. It is the same kernel the live ISO runs: every driver the kernel has, as a module.

### Build your own

```bash
forge kernel list                      # what kernel.org offers
forge kernel build --auto              # a kernel trimmed to the modules this machine has loaded
sudo forge kernel install
```

`--full` builds every driver as a module (about 300 MB of modules), `--menuconfig` lets you choose, and `--config FILE`
uses a configuration as it is. `--series stable` (the default) or `--series longterm` chooses the series, `--version 7.3.3` an exact release. Plug in
the devices you use before an `--auto` build so their modules are kept (or name them with `--also-keep`). The tools the
build needs are fetched from the package repository and removed afterwards.

## Graphics cards

Graphics firmware is large and only needed for matching hardware, so it is a separate package for each vendor. Add the one
for your card to `packages` in `/etc/forge/system.toml` (or `sudo forge install NAME`) and reboot:

| Package | Card | Driver |
|---|---|---|
| `linux-firmware-amdgpu` | AMD Radeon (including recent RDNA cards) | `amdgpu`, `radeon` |
| `linux-firmware-intel-gpu` | Intel graphics | `i915`, `xe` |
| `linux-firmware-nvidia` | NVIDIA, with the open driver | `nouveau` |

The driver modules exist in the kernel but are loaded **only when the matching firmware package is installed**. A graphics
driver that takes the screen over and then finds no firmware leaves the display black; this rule keeps a machine without
the package on the plain framebuffer, with a working screen. `i915` is built into the kernel.

Check that it worked:

```bash
lsmod | grep amdgpu
sudo dmesg | grep -i amdgpu | head
ls /sys/class/drm
```

You should see the driver loaded and one `card...` entry per connected output.

This is the kernel side only. A desktop also needs a graphics library (Mesa), a display server or compositor, input and
audio, which are not packaged yet.
