#!/usr/bin/env bash
# Build the live ISO from the glibc package repository.
#
#   scripts/build-iso.sh
#
# REPO (default repo/glibc-world2) is the repository scripts/build-glibc-world.sh fills: it holds every package
# the image is made of, including `linux` (the kernel, built from recipes/linux) and `forge`
# (scripts/package-forge.sh). The image is assembled by `forge iso`: the packages are copied into one root
# exactly as `forge apply` copies them into a system (merged /usr, setuid on the allowlist), the image carries
# its package store and a record of the package set so that an installed system can be upgraded with forge,
# and the result is a squashfs with a small initramfs, booted by Limine. Needs on the host: xorriso, mksquashfs,
# openssl and limine (the `limine` tool and /usr/share/limine files).
#
# Writes singularity-live.iso (override with ISO=...). Try it with scripts/test-iso-qemu.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "FAIL: $*" >&2; exit 1; }

FORGE=${FORGE:-target/debug/forge}
REPO=${REPO:-repo/glibc-world2}
OUT=${OUT:-packages/iso}
ISO=${ISO:-singularity-live.iso}

[ -x "$FORGE" ] || fail "build forge first (cargo build), or set FORGE=/path/to/forge"
for t in xorriso mksquashfs openssl limine; do
    command -v "$t" >/dev/null || fail "$t is needed on the host"
done
. scripts/lib-keys.sh
ensure_dev_key || fail "could not set up the development signing key"
sign_existing_repo "$REPO"
[ -f "$REPO/index.toml" ] || fail "no repository at $REPO: run scripts/build-glibc-world.sh first"
for p in linux forge; do
    grep -q "^name = \"$p\"" "$REPO/index.toml" || fail "$REPO has no \`$p\` package (kernel: build recipes/linux; forge: scripts/package-forge.sh)"
done

mkdir -p "$OUT"
echo "==== assembling the ISO"
[ -e "$ISO" ] && fail "$ISO exists; move it away first"
ISO_WORK=${ISO_WORK:-$OUT/iso-work}
[ -e "$ISO_WORK" ] && fail "$ISO_WORK exists from an earlier run; use another ISO_WORK"
WALLPAPER=()
# A picture for the boot menu, if there is one at distro/boot/wallpaper.{jpg,png} (not in git: pictures are not ours to ship).
for w in distro/boot/wallpaper.jpg distro/boot/wallpaper.png; do [ -f "$w" ] && { WALLPAPER=(--wallpaper "$w"); break; }; done
"$FORGE" iso "${TRUST[@]}" "${WALLPAPER[@]}" --repo "$REPO" --trusted-keys distro/keys --out "$ISO" --work-dir "$ISO_WORK" 2>&1 \
    | grep -v 'no version information available'
xorriso -indev "$ISO" -find / -maxdepth 3 2>/dev/null | grep -E "vmlinuz|initramfs|rootfs.squashfs|limine|BOOTX64" || true
echo "PASS: $ISO"
