#!/usr/bin/env bash
# Package the micro editor: a statically linked build (pure Go, no C), published, signed, into $REPO.
# IMPURE, like package-forge.sh: the build host's network and a Go toolchain are used. The toolchain is the official
# Go release (go.dev), unpacked into a scratch directory, and both it and micro's source are pinned by SHA-256.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "FAIL: $*" >&2; exit 1; }

FORGE=${FORGE:-target/debug/forge}
REPO=${REPO:-repo/glibc-world2}
OUT=${OUT:-packages/glibc-world2}
WORK=${WORK:-packages/micro-build}

MICRO_VERSION=2.0.15
MICRO_URL=https://github.com/zyedidia/micro/archive/refs/tags/v$MICRO_VERSION.tar.gz
MICRO_SHA=612c775321c268c8f9e1767505ff378bca9b9ab66f5c41b69ecb2464ecf15084
GO_VERSION=1.27.4
GO_URL=https://go.dev/dl/go$GO_VERSION.linux-amd64.tar.gz
GO_SHA=7226736c2339719cda28fbf3a54d71aa63bcefe376e98cef00034268dab24840

[ -x "$FORGE" ] || fail "build forge first (cargo build), or set FORGE=/path/to/forge"
. scripts/lib-keys.sh
ensure_dev_key || fail "could not set up the development signing key"

fetch() { # url sha256 file
    if [ ! -f "$3" ] || [ "$(sha256sum "$3" | cut -d' ' -f1)" != "$2" ]; then
        curl -fsSL -o "$3.part" "$1" && mv "$3.part" "$3"
    fi
    [ "$(sha256sum "$3" | cut -d' ' -f1)" = "$2" ] || fail "$3 does not match its pinned SHA-256"
}

mkdir -p "$WORK"
fetch "$GO_URL" "$GO_SHA" "$WORK/go.tar.gz"
fetch "$MICRO_URL" "$MICRO_SHA" "$WORK/micro.tar.gz"
[ -x "$WORK/go/bin/go" ] || tar -C "$WORK" -xzf "$WORK/go.tar.gz"
src=$WORK/micro-$MICRO_VERSION
[ -d "$src" ] || tar -C "$WORK" -xzf "$WORK/micro.tar.gz"

export GOROOT=$PWD/$WORK/go GOPATH=$PWD/$WORK/gopath GOCACHE=$PWD/$WORK/gocache CGO_ENABLED=0 GOFLAGS=-mod=mod
export PATH=$GOROOT/bin:$PATH
echo "==== building micro $MICRO_VERSION with $(go version | cut -d' ' -f3)"
mkdir -p "$WORK/out"
(cd "$src" && go build -trimpath -ldflags "-s -w \
    -X github.com/zyedidia/micro/v2/internal/util.Version=$MICRO_VERSION \
    -X github.com/zyedidia/micro/v2/internal/util.CommitHash=release \
    -X 'github.com/zyedidia/micro/v2/internal/util.CompileDate=1980-01-01'" \
    -o "$OLDPWD/$WORK/out/micro" ./cmd/micro)
bin=$WORK/out/micro
readelf -lW "$bin" | grep -q INTERP && fail "$bin is dynamic"

tree=$(mktemp -d "${TMPDIR:-/tmp}/micro-pkg-XXXXXX")
trap 'chmod -R u+w "$tree" 2>/dev/null; rm -rf "$tree"' EXIT
mkdir -p "$tree/usr/bin" "$tree/usr/share/licenses/micro"
install -m 755 "$bin" "$tree/usr/bin/micro"
cp "$src/LICENSE" "$tree/usr/share/licenses/micro/LICENSE"

mkdir -p "$OUT"
archive=$OUT/micro-$MICRO_VERSION.tar.zst
"$FORGE" store pack "$tree" --name micro --version "$MICRO_VERSION" --libc glibc --out "$archive"
"$FORGE" store publish "$archive" --repo "$REPO" "${SIGN[@]}"
echo "PASS: micro $MICRO_VERSION is in $REPO"
