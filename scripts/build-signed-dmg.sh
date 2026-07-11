#!/usr/bin/env bash
#
# fork(voice-control): reproducible local release build for the personal fork.
#
# Why this exists (both are macOS-local pain points, see AGENTS.md "Fork Build"):
#   1. Code signing: an ad-hoc signature has an unstable identity, so macOS TCC
#      forgets Microphone/Accessibility grants on every rebuild. Signing with a
#      stable self-signed cert makes the app's Designated Requirement constant,
#      so permissions stick across builds.
#   2. DMG packaging: Tauri's built-in `bundle_dmg.sh` (AppleScript-driven Finder
#      layout) fails intermittently on this machine. We build only the .app with
#      Tauri and package the DMG deterministically via hdiutil.
#
# Usage:  scripts/build-signed-dmg.sh
# Override the signing identity with:  HANDY_SIGN_IDENTITY="<cert name>" scripts/build-signed-dmg.sh
# Keep Cargo intermediates after success: HANDY_KEEP_BUILD_ARTIFACTS=1 scripts/build-signed-dmg.sh
# List available identities with:      security find-identity -v
#
set -euo pipefail

IDENTITY="${HANDY_SIGN_IDENTITY:-Voice Control / Handy Dev}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# cmake in the transcribe-cpp C++ tree needs this policy shim on modern cmake.
export CMAKE_POLICY_VERSION_MINIMUM=3.5
# Tauri passes this to codesign for the .app.
export APPLE_SIGNING_IDENTITY="$IDENTITY"

echo "==> Building Handy.app (signed as: $IDENTITY)"
# createUpdaterArtifacts=false: the fork ships local DMGs, not updater bundles;
# leaving it on aborts the build without TAURI_SIGNING_PRIVATE_KEY.
bun run tauri build --bundles app --config '{"bundle":{"createUpdaterArtifacts":false}}'

APP="src-tauri/target/release/bundle/macos/Handy.app"
if [[ ! -d "$APP" ]]; then
  echo "ERROR: expected app bundle not found at $APP" >&2
  exit 1
fi

# Defensive re-sign: guarantee a deep, stable signature even if Tauri's pass
# missed nested binaries. Must pass --options runtime AND the entitlements file:
# hardened runtime without the microphone entitlements silently blocks mic access
# (the permission prompt never appears). Entitlements path matches tauri.conf.json.
ENTITLEMENTS="src-tauri/Entitlements.plist"
echo "==> Re-signing bundle (deep, with entitlements)"
codesign --force --deep --options runtime --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"
codesign --verify --strict "$APP" && echo "    signature valid"

VERSION="$(defaults read "$REPO_ROOT/$APP/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || echo "dev")"
ARCH="$(uname -m)"
BUILD_OUT_DIR="src-tauri/target/release/bundle"
ARTIFACT_DIR="$REPO_ROOT/Handy artifacts"
DMG_NAME="Handy_${VERSION}_voice-control_${ARCH}.dmg"
BUILD_DMG="$BUILD_OUT_DIR/$DMG_NAME"
FINAL_DMG="$ARTIFACT_DIR/$DMG_NAME"

# Package the signed .app into a compressed DMG via hdiutil (reliable path).
STAGE="$BUILD_OUT_DIR/.dmg_stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
echo "==> Packaging DMG"
hdiutil create -volname "Handy" -srcfolder "$STAGE" -ov -format UDZO "$BUILD_DMG" >/dev/null
rm -rf "$STAGE"

mkdir -p "$ARTIFACT_DIR"
mv -f "$BUILD_DMG" "$FINAL_DMG"

# Keep only the three newest fork DMGs. File names are generated above and
# contain no newlines; quoting still protects the artifact directory's space.
artifact_count=0
while IFS= read -r artifact; do
  artifact_count=$((artifact_count + 1))
  if (( artifact_count > 3 )); then
    rm -f "$artifact"
  fi
done < <(find "$ARTIFACT_DIR" -maxdepth 1 -type f -name 'Handy_*_voice-control_*.dmg' -print | xargs -I '{}' stat -f '%m %N' '{}' | sort -rn | cut -d ' ' -f 2-)

if [[ "${HANDY_KEEP_BUILD_ARTIFACTS:-0}" != "1" ]]; then
  echo "==> Removing Cargo build intermediates"
  cargo clean --manifest-path "$REPO_ROOT/src-tauri/Cargo.toml"
else
  echo "==> Keeping Cargo build intermediates (HANDY_KEEP_BUILD_ARTIFACTS=1)"
fi

echo ""
echo "==> Done: $FINAL_DMG"
echo "    First launch of a new build: right-click -> Open once (Gatekeeper, unnotarized)."
echo "    Permissions persist across rebuilds as long as the signing cert is unchanged."
