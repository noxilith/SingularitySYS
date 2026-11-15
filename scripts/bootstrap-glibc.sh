#!/usr/bin/env bash
# Stage 0 and 1 for glibc: the first packages (kernel headers, a static BusyBox and make), then glibc, binutils and
# gcc for x86_64-unknown-linux-gnu, all built with the HOST's compiler (nothing in the store can compile for glibc
# yet) into a fresh local repository. A later stage rebuilds them with themselves.
#
#   scripts/bootstrap-glibc.sh
#
# Needs on the host: gcc, g++, make, ld, tar, xz, sed, awk, bison, flex, m4, perl, python3, gperf, readelf.
# The glibc build is a few minutes; gcc takes 30 to 60 minutes on 8 cores. Every run writes fresh directories
# and refuses to reuse existing ones.
set -euo pipefail

cd "$(dirname "$0")/.."
FORGE=${FORGE:-target/debug/forge}
CACHE=${CACHE:-sources/cache}
JOBS=${JOBS:-6}
STAMP=${STAMP:-$(date +%Y%m%d-%H%M%S)}
OUT=${OUT:-packages/glibc-$STAMP}
REPO=${REPO:-repo/glibc-$STAMP}
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/forge/build}

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -x "$FORGE" ] || fail "build forge first (cargo build), or set FORGE=/path/to/forge"
. scripts/lib-keys.sh; ensure_dev_key || fail "could not set up the development signing key"
for d in "$OUT" "$REPO"; do
    [ -n "${ONLY:-}" ] || [ ! -e "$d" ] || fail "$d already exists; choose another STAMP (or OUT/REPO) instead of reusing it"
done
sign_existing_repo "$REPO"
for tool in gcc g++ make ld tar xz sed awk readelf bison flex m4 perl python3 gperf; do
    command -v "$tool" >/dev/null || fail "this needs '$tool' on the host"
done
mkdir -p "$OUT/logs"
want() { [ -z "${ONLY:-}" ] || case " $ONLY " in *" $1 "*) true ;; *) false ;; esac; }

build() {
    local recipe=$1 log=$OUT/logs/$1.log
    local repos=()
    if [ -f "$REPO/index.toml" ]; then repos+=(--repo "$REPO"); fi
    printf -- '---- %-24s ' "$recipe"
    if ! "$FORGE" build "${TRUST[@]}" "recipes/$recipe" --libc glibc --tools host --jobs "$JOBS" \
        "${repos[@]}" --out "$OUT" --source-cache "$CACHE" \
        --work-dir "$WORK" --publish "$REPO" "${SIGN[@]}" >"$log" 2>&1; then
        echo "FAILED"
        grep -v 'no version information available' "$log" | tail -n 40
        fail "building $recipe failed; the full log is $log"
    fi
    grep -m1 '^built ' "$log" | sed 's/^built //'
}

echo "==== stage 0 for glibc"
for r in linux-headers-stage0 busybox-stage0 make-stage0; do if want "$r"; then build "$r"; fi; done
echo "==== stage 1 (glibc)"
for r in glibc-stage1 binutils-glibc-stage1 gcc-glibc-stage1; do if want "$r"; then build "$r"; fi; done

echo "==== checking the results"
verify=$OUT/verify
for name in glibc-stage1 binutils-glibc-stage1 gcc-glibc-stage1; do
    archive=$(ls "$OUT/$name"-*.tar.zst)
    "$FORGE" store --store "$verify" install "$archive" --unverified >/dev/null || fail "cannot install $archive"
done
g=$(ls -d "$verify"/glibc-stage1-*); b=$(ls -d "$verify"/binutils-glibc-stage1-*); c=$(ls -d "$verify"/gcc-glibc-stage1-*)
T=x86_64-unknown-linux-gnu
for f in "$b/usr/bin/$T-ld" "$b/usr/bin/$T-as" "$c/usr/bin/$T-gcc" "$c/usr/bin/$T-g++" \
         "$c/usr/libexec/gcc/$T/14.2.0/cc1" "$c/usr/libexec/gcc/$T/14.2.0/cc1plus"; do
    [ -e "$f" ] || fail "$f is missing"
    [ "$(readelf -lW "$f" | grep -c INTERP || true)" = 0 ] || fail "$(basename "$f") is dynamic; it would not start in the sandbox"
done
[ -x "$g/usr/lib/ld-linux-x86-64.so.2" ] || fail "glibc has no dynamic loader"
echo "ok: glibc, binutils and gcc are built; the compilers are fully static"
echo "stage 1 repository: $REPO"
echo "packages:           $OUT"
echo "PASS"
