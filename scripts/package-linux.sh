#!/usr/bin/env bash
# Packs one Linux release archive from a binary already built for <target>:
# dist/provefab-<version>-linux-<arch>.tar.gz and its .sha256, to attach to the
# GitHub release of the matching tag (the CI tag job does it; see
# .github/workflows/ci.yml).
#
#   scripts/package-linux.sh x86_64-unknown-linux-musl x86_64
#   scripts/package-linux.sh aarch64-unknown-linux-musl aarch64
set -euo pipefail

cd "$(dirname "$0")/.."
TARGET="${1:?cargo target}"
ARCH="${2:?x86_64 or aarch64}"
case "$ARCH" in
  x86_64 | aarch64) ;;
  *) echo "arch must be x86_64 or aarch64, not $ARCH" >&2; exit 2 ;;
esac

VERSION="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="provefab"))')"
NAME="provefab-${VERSION}-linux-${ARCH}"
# CARGO_TARGET_DIR is honoured: a Linux build in Docker on a Mac keeps its
# artifacts apart from the Mac's own target directory.
BIN="${CARGO_TARGET_DIR:-target}/${TARGET}/release/provefab"
[ -x "$BIN" ] || { echo "build first: $BIN is missing" >&2; exit 1; }
file "$BIN" | grep -Eq 'statically linked|static-pie linked' || { echo "$BIN is not statically linked" >&2; exit 1; }

OUT="dist/${NAME}"
rm -rf "$OUT" "dist/${NAME}.tar.gz" "dist/${NAME}.tar.gz.sha256"
mkdir -p "$OUT"
install -m 755 "$BIN" "$OUT/provefab"
cp provefab.example.toml LICENSE.md "$OUT/"
cat > "$OUT/INSTALL.txt" <<EOF
Provefab ${VERSION} for Linux (${ARCH}), a static binary

1. Put the binary on your PATH, without sudo:
     mkdir -p ~/.local/bin && install -m 755 provefab ~/.local/bin/provefab
2. Follow the quick start from step 2:
     https://github.com/provefab/provefab#quick-start
   provefab.example.toml is the configuration to copy in step 4.
EOF

(cd dist && tar -czf "${NAME}.tar.gz" "$NAME" && sha256sum "${NAME}.tar.gz" > "${NAME}.tar.gz.sha256")
echo "ready: dist/${NAME}.tar.gz"
cat "dist/${NAME}.tar.gz.sha256"
