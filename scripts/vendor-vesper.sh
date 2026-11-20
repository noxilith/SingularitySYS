#!/usr/bin/env bash
# Make the source tarball of VesperDE (https://github.com/noxilith/VesperDE) that recipes/vesper builds from: one
# pinned commit with every crate its two Cargo.lock files name copied in (`cargo vendor`) and cargo told to use them,
# because a forge build has no network. Run on a machine with network and cargo; the result is deterministic for a
# given commit and cargo, and goes (with -u) to the `sources` release of this repository, where the recipe fetches it.
#
#   scripts/vendor-vesper.sh        # builds $OUT/vesper-<rev>-vendored.tar.gz and prints its SHA-256
#   scripts/vendor-vesper.sh -u     # also uploads it to the `sources` release (created when missing)
set -euo pipefail
cd "$(dirname "$0")/.."; ROOT=$PWD

fail() { echo "FAIL: $*" >&2; exit 1; }

VESPER_REV=${VESPER_REV:-9c123d5f95bb16f668c6eb0bccc1b2a1a20091bb}
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/forge/vesper-vendor}
OUT=${OUT:-$WORK}
NAME=vesper-$VESPER_REV-vendored.tar.gz

mkdir -p "$WORK"; WORK=$(cd "$WORK" && pwd); OUT=$(cd "$OUT" && pwd)
src=$WORK/vesper-$VESPER_REV
if [ ! -d "$src" ]; then
    curl -fsSL -o "$WORK/src.tar.gz" "https://github.com/noxilith/VesperDE/archive/$VESPER_REV.tar.gz"
    tar -C "$WORK" -xzf "$WORK/src.tar.gz"
    mv "$WORK/VesperDE-$VESPER_REV" "$src"
    rm -f "$WORK/src.tar.gz"
fi
cd "$src"
if [ ! -d vendor ]; then
    cargo vendor --locked --manifest-path Cargo.toml -s file-manager/Cargo.toml vendor > "$WORK/vendor-config.toml"
    # cargo prints the source replacement it wants; the recipe's tarball carries it in the checkout's cargo config.
    printf '\n' >> .cargo/config.toml
    cat "$WORK/vendor-config.toml" >> .cargo/config.toml
fi
# Deterministic: sorted names, no owners, fixed times.
cd "$WORK"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -cf - "vesper-$VESPER_REV" | gzip -n -9 > "$OUT/$NAME"
sha=$(sha256sum "$OUT/$NAME" | cut -d' ' -f1)
echo "$NAME  $(du -h "$OUT/$NAME" | cut -f1)"
echo "sha256 = \"$sha\""

if [ "${1:-}" = "-u" ]; then
    cd "$ROOT"
    gh release view sources >/dev/null 2>&1 || gh release create sources --title "Source tarballs" \
        --notes "Source tarballs the recipes build from (vendored crates, since a forge build has no network)." --prerelease
    gh release upload sources "$OUT/$NAME" --clobber
    echo "uploaded: https://github.com/noxilith/SingularitySYS/releases/download/sources/$NAME"
fi
