#!/usr/bin/env bash
# Builds the provefab release for macOS: one universal binary (Apple Silicon + Intel),
# signed with Developer ID (hardened runtime), notarized, zipped with the example config.
# Output: dist/provefab-<version>-macos-universal.zip and its .sha256, to attach to the
# GitHub release of the matching tag (gh release create v<version> dist/*.zip dist/*.sha256).
#
# Needs, once per machine:
#   - a "Developer ID Application" certificate in the login keychain;
#   - a notarytool profile: xcrun notarytool store-credentials provefab-notary ...
#   - rustup target add aarch64-apple-darwin x86_64-apple-darwin
# Overrides: SIGN_ID (certificate name), NOTARY_PROFILE (default provefab-notary).
set -euo pipefail

cd "$(dirname "$0")/.."
NOTARY_PROFILE="${NOTARY_PROFILE:-provefab-notary}"
SIGN_ID="${SIGN_ID:-$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)}"
[ -n "$SIGN_ID" ] || { echo "no Developer ID Application certificate in the keychain" >&2; exit 1; }

VERSION="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="provefab"))')"
NAME="provefab-${VERSION}-macos-universal"
OUT="dist/${NAME}"
rm -rf "$OUT" "dist/${NAME}.zip" "dist/${NAME}.zip.sha256"
mkdir -p "$OUT"

for t in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked -p provefab --target "$t"
done
lipo -create -output "$OUT/provefab" \
  target/aarch64-apple-darwin/release/provefab \
  target/x86_64-apple-darwin/release/provefab

codesign --force --options runtime --timestamp --sign "$SIGN_ID" "$OUT/provefab"
codesign --verify --strict --verbose=2 "$OUT/provefab"

cp provefab.example.toml LICENSE.md "$OUT/"
cat > "$OUT/INSTALL.txt" <<EOF
Provefab ${VERSION} for macOS (Apple Silicon and Intel)

1. Put the binary on your PATH:
     sudo install -m 755 provefab /usr/local/bin/provefab
2. Follow the quick start from step 2:
     https://github.com/provefab/provefab#quick-start
   provefab.example.toml is the configuration to copy in step 4.
EOF

# A bare binary cannot be stapled: Gatekeeper checks the ticket online at first launch.
(cd dist && ditto -c -k --keepParent "$NAME" "${NAME}.zip")
xcrun notarytool submit "dist/${NAME}.zip" --keychain-profile "$NOTARY_PROFILE" --wait
(cd dist && shasum -a 256 "${NAME}.zip" > "${NAME}.zip.sha256")

echo "ready: dist/${NAME}.zip"
cat "dist/${NAME}.zip.sha256"
