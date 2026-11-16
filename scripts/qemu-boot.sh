#!/bin/sh
# usage: scripts/qemu-boot.sh ISO [extra qemu args]  -- headless boot, serial console on stdout
Q=${QEMU_DIR:-$(dirname "$0")/../packages/qemu-run}
iso=$1; shift
LD_LIBRARY_PATH=$Q/usr/lib exec $Q/usr/bin/qemu-system-x86_64 -L $Q/usr/share/qemu -enable-kvm -cpu host -m 2G -smp 2 \
  -display none -serial ${SERIAL:-mon:stdio} -cdrom "$iso" -boot d "$@"
