#!/usr/bin/env bash
# Build recipes for glibc, dynamically linked, with store tools only, into a repository.
#
#   TOOLCHAIN=stage1 scripts/build-glibc-world.sh zlib m4 gawk ...     # tools from the stage-1 bootstrap
#   TOOLCHAIN=final  scripts/build-glibc-world.sh zlib m4 gawk ...     # tools from the final toolchain
#
# Recipes name their tools by their final names (busybox, make, binutils, gcc, glibc, linux-headers). While the
# toolchain is still the bootstrap one (stage 1) those names are mapped to the bootstrap packages in a temporary
# copy of the recipe, so the recipe files never carry bootstrap names.
#
# Environment:
#   BOOT_REPO   the stage-1 repository (default: the newest repo/glibc-*)
#   WORLD_REPO  where results are published (default repo/glibc-world); repeated runs add to it
#   WORLD_OUT   archives and logs (default packages/glibc-world)
set -euo pipefail

cd "$(dirname "$0")/.."
FORGE=${FORGE:-target/debug/forge}
CACHE=${CACHE:-sources/cache}
JOBS=${JOBS:-6}
TOOLCHAIN=${TOOLCHAIN:-stage1}
WORLD_REPO=${WORLD_REPO:-repo/glibc-world}
WORLD_OUT=${WORLD_OUT:-packages/glibc-world}
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/forge/build}
BOOT_REPO=${BOOT_REPO:-$(ls -d repo/glibc-[a-z]* 2>/dev/null | tail -n 1)}
STORE=${STORE:-$WORLD_OUT/store}

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -x "$FORGE" ] || fail "build forge first (cargo build), or set FORGE=/path/to/forge"
[ $# -gt 0 ] || fail "name the recipes to build, in dependency order"
. scripts/lib-keys.sh; ensure_dev_key || fail "could not set up the development signing key"
sign_existing_repo "$WORLD_REPO"
mkdir -p "$WORLD_OUT/logs" "$WORLD_OUT/recipes"

repos=()
if [ "$TOOLCHAIN" = stage1 ]; then
    [ -f "$BOOT_REPO/index.toml" ] || fail "no stage-1 repository (set BOOT_REPO)"
    sign_existing_repo "$BOOT_REPO"
    repos+=(--repo "$BOOT_REPO")
fi

for name in "$@"; do
    log=$WORLD_OUT/logs/$name.log
    dir=recipes/$name
    if [ "$TOOLCHAIN" = stage1 ]; then
        dir=$WORLD_OUT/recipes/$name
        rm -f "$dir/recipe.toml" 2>/dev/null || true
        mkdir -p "$dir"
        cp -R "recipes/$name/." "$dir/"
        perl -0pi scripts/lib-stage1-names.pl "$dir/recipe.toml"
    fi
    r=("${repos[@]}")
    if [ -f "$WORLD_REPO/index.toml" ]; then r+=(--repo "$WORLD_REPO"); fi
    printf -- '---- %-16s ' "$name"
    if ! "$FORGE" build "${TRUST[@]}" "$dir" --libc glibc --tools store --jobs "$JOBS" \
        --store "$STORE" "${r[@]}" --out "$WORLD_OUT" --source-cache "$CACHE" \
        --work-dir "$WORK" --publish "$WORLD_REPO" "${SIGN[@]}" >"$log" 2>&1; then
        echo "FAILED"
        grep -v 'no version information available' "$log" | tail -n 40
        fail "building $name failed; the full log is $log"
    fi
    grep -m1 '^built ' "$log" | sed 's/^built //'
    grep -h '^run dependencies\|^warning: libraries' "$log" | sed 's/^/        /' || true
done
echo "PASS: $WORLD_REPO"
