#!/bin/sh
# SingularitySYS kernel build steps, run inside a configured kernel source tree. Shared by the `linux` recipe
# and `forge kernel` (which embeds this file), like full-config.sh.
#
#   sh kernel-build.sh compile     bzImage and every module (a module that fails does not lose the rest)
#   sh kernel-build.sh install     stage kernel, config and modules under $DESTDIR (boot/ and usr/lib/modules/)
#
# JOBS: parallel jobs (default 1). DESTDIR: where to stage (install). KCFG_CC: the compiler.
set -e
cc=${KCFG_CC:-gcc -std=gnu11}
export KBUILD_BUILD_TIMESTAMP='1980-01-01'
export KBUILD_BUILD_USER=forge KBUILD_BUILD_HOST=singularity
kmake() { make ARCH=x86_64 CC="$cc" "$@"; }

case "${1:?usage: kernel-build.sh compile|install}" in
compile)
    KC='-Wno-error -Wno-unterminated-string-initialization'
    # Boot messages that alarm without anything to act on: the kernel already turns RDSEED off by itself on the
    # AMD chips with the old-microcode (Zen 5) or Cyan Skillfish bug, and says so with pr_emerg, which `quiet`
    # cannot hide. The behaviour stays; only the message becomes a debug message.
    if [ -f arch/x86/kernel/cpu/amd.c ]; then
        sed -i 's/pr_emerg_once("RDSEED32 is broken/pr_debug("RDSEED32 is broken/; s/pr_emerg("RDSEED is not reliable/pr_debug("RDSEED is not reliable/' arch/x86/kernel/cpu/amd.c
    fi
    kmake KCFLAGS="$KC" -j"${JOBS:-1}" bzImage
    # Thousands of modules: one that fails to build must not lose the rest.
    kmake -k KCFLAGS="$KC" -j"${JOBS:-1}" modules || echo "note: some modules failed to build and are left out" >&2
    built=$(find . -name '*.ko' | wc -l)
    echo "modules built: $built"
    [ "$built" -gt 1 ] || { echo "no modules were built" >&2; exit 1; }
    ;;
install)
    : "${DESTDIR:?DESTDIR is not set}"
    rel=$(kmake -s kernelrelease)
    mkdir -p "$DESTDIR/boot"
    cp arch/x86/boot/bzImage "$DESTDIR/boot/vmlinuz-$rel"
    cp .config "$DESTDIR/boot/config-$rel"
    # modules.order lists modules that failed to build too; install what exists, then index it ourselves.
    kmake -k INSTALL_MOD_PATH="$DESTDIR/usr" INSTALL_MOD_STRIP=1 modules_install || true
    depmod -a -b "$DESTDIR/usr" "$rel"
    test -s "$DESTDIR/usr/lib/modules/$rel/modules.dep"
    # /usr/lib/modules is the real directory; the build and source links point into the build tree.
    rm -f "$DESTDIR"/usr/lib/modules/*/build "$DESTDIR"/usr/lib/modules/*/source
    # modprobe looks in /lib/modules/<release>: /lib is a symlink to /usr/lib on the system, so it finds them.
    test -f "$DESTDIR/boot/vmlinuz-$rel"
    ;;
*) echo "unknown step $1" >&2; exit 2 ;;
esac
