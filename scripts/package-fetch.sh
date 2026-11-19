#!/usr/bin/env bash
# Package fetch (https://github.com/noxilith/Fetch): the spinning-logo system information tool. A statically linked build
# with the host's Rust toolchain, published, signed, into $REPO. IMPURE, like package-forge.sh and package-micro.sh: the
# toolchain and static glibc come from the build host (and cargo fetches the crates Cargo.lock pins). The source is the
# GitHub archive of one commit, pinned by SHA-256. Raise REVISION when rebuilding the same source (it becomes _rN).
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "FAIL: $*" >&2; exit 1; }

FORGE=${FORGE:-target/debug/forge}
REPO=${REPO:-repo/glibc-world2}
OUT=${OUT:-packages/glibc-world2}
# Outside the repository: inside it cargo would treat the source as a member of forge's own workspace.
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/forge/fetch-build}

FETCH_VERSION=2.3.0
FETCH_REVISION=${REVISION:-2}
FETCH_REV=71c1f9e8b3aeb1b3e99fe5505382de1e3a9f825c
FETCH_URL=https://github.com/noxilith/Fetch/archive/$FETCH_REV.tar.gz
FETCH_SHA=b1bec0d1a65563414fba2ab4aecb756d324b712fa590f4172ed26e9dc467f3c0

VERSION=$FETCH_VERSION; [ "$FETCH_REVISION" = 0 ] || VERSION=${FETCH_VERSION}_r$FETCH_REVISION

[ -x "$FORGE" ] || fail "build forge first (cargo build), or set FORGE=/path/to/forge"
. scripts/lib-keys.sh
ensure_dev_key || fail "could not set up the development signing key"

mkdir -p "$WORK"; WORK=$(cd "$WORK" && pwd)
tarball=$WORK/fetch-$FETCH_REV.tar.gz
if [ ! -f "$tarball" ] || [ "$(sha256sum "$tarball" | cut -d' ' -f1)" != "$FETCH_SHA" ]; then
    curl -fsSL -o "$tarball.part" "$FETCH_URL" && mv "$tarball.part" "$tarball"
fi
[ "$(sha256sum "$tarball" | cut -d' ' -f1)" = "$FETCH_SHA" ] || fail "$tarball does not match its pinned SHA-256"
src=$WORK/Fetch-$FETCH_REV
[ -d "$src" ] || tar -C "$WORK" -xzf "$tarball"
grep -q "^version = \"$FETCH_VERSION\"" "$src/Cargo.toml" || fail "the pinned source is not version $FETCH_VERSION"

echo "==== building a static fetch $VERSION"
# This host has gcc but no cc or x86_64-linux-gnu-gcc (the repository's .cargo/config.toml names it, but not outside it).
(cd "$src" && CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER:-gcc} RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --locked \
    --target x86_64-unknown-linux-gnu --target-dir "$WORK/target" 2>&1 | tail -n 3)
bin=$WORK/target/x86_64-unknown-linux-gnu/release/fetch
readelf -lW "$bin" | grep -q INTERP && fail "$bin is dynamic"

tree=$(mktemp -d "${TMPDIR:-/tmp}/fetch-pkg-XXXXXX")
trap 'chmod -R u+w "$tree" 2>/dev/null; rm -rf "$tree"' EXIT
mkdir -p "$tree/usr/bin" "$tree/usr/share/licenses/fetch"
install -m 755 "$bin" "$tree/usr/bin/fetch"
cp "$src/LICENSE" "$tree/usr/share/licenses/fetch/LICENSE"

mkdir -p "$OUT"
archive=$OUT/fetch-$VERSION.tar.zst
"$FORGE" store pack "$tree" --name fetch --version "$VERSION" --libc glibc --out "$archive"
"$FORGE" store publish "$archive" --repo "$REPO" "${SIGN[@]}"
echo "PASS: fetch $VERSION is in $REPO"
