#!/usr/bin/env bash
# Make a vendored source tarball of a Rust program for a recipe (a forge build has no network): download the release
# tarball, `cargo vendor --locked`, point cargo at it, and pack deterministically. With -u, upload it to the `sources`
# release of this repository. Run where there is network and cargo.
#
#   scripts/vendor-crate.sh [-u] NAME VERSION URL      # e.g. greetd 0.10.3 https://git.sr.ht/~kennylevinsen/greetd/archive/0.10.3.tar.gz
set -euo pipefail
cd "$(dirname "$0")/.."; ROOT=$PWD
up=0; [ "${1:-}" = "-u" ] && { up=1; shift; }
NAME=$1 VERSION=$2 URL=$3
WORK=${WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/forge/vendor-$NAME}
rm -rf "$WORK"; mkdir -p "$WORK"; cd "$WORK"
curl -fsSL -o src.tar.gz "$URL"
mkdir src && tar -xzf src.tar.gz -C src --strip-components=1
top=$NAME-$VERSION; mv src "$top"
(cd "$top" && mkdir -p .cargo && cargo vendor --locked vendor > .cargo/config.toml)
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -cf - "$top" | gzip -n -9 > "$WORK/$top-vendored.tar.gz"
echo "$top-vendored.tar.gz  $(du -h "$WORK/$top-vendored.tar.gz" | cut -f1)"
echo "sha256 = \"$(sha256sum "$WORK/$top-vendored.tar.gz" | cut -d' ' -f1)\""
if [ "$up" = 1 ]; then cd "$ROOT"; gh release upload sources "$WORK/$top-vendored.tar.gz" --clobber; fi
