#!/usr/bin/env bash
# Package sysreport (distro/sysreport/sysreport): the diagnostic report tool, a single shell script. Published, signed, into
# $REPO. Raise REVISION when republishing the same script (it becomes _rN).
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "FAIL: $*" >&2; exit 1; }

FORGE=${FORGE:-target/debug/forge}
REPO=${REPO:-repo/glibc-world2}
OUT=${OUT:-packages/glibc-world2}
VERSION=1.0; [ "${REVISION:-0}" = 0 ] || VERSION=1.0_r$REVISION

[ -x "$FORGE" ] || fail "build forge first (cargo build), or set FORGE=/path/to/forge"
. scripts/lib-keys.sh
ensure_dev_key || fail "could not set up the development signing key"
bash -n distro/sysreport/sysreport || fail "the script has a syntax error"

tree=$(mktemp -d "${TMPDIR:-/tmp}/sysreport-pkg-XXXXXX")
trap 'chmod -R u+w "$tree" 2>/dev/null; rm -rf "$tree"' EXIT
mkdir -p "$tree/usr/bin"
install -m 755 distro/sysreport/sysreport "$tree/usr/bin/sysreport"

mkdir -p "$OUT"
archive=$OUT/sysreport-$VERSION.tar.zst
"$FORGE" store pack "$tree" --name sysreport --version "$VERSION" --libc glibc --out "$archive"
"$FORGE" store publish "$archive" --repo "$REPO" "${SIGN[@]}"
echo "PASS: sysreport $VERSION is in $REPO"
