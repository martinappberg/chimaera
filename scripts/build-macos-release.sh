#!/usr/bin/env bash
# tauri-action publishes only after this build command returns successfully.
# Keep verification here so it covers the exact bundle the action uploads.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root/crates/chimaera-app"

# This machine's credentials live outside the checkout. CI supplies its own.
if [ -z "${CI:-}" ] && [ -n "${HOME:-}" ]; then
  signing_env="$HOME/.config/chimaera/signing/env.sh"
  if [ -f "$signing_env" ]; then
    # shellcheck source=/dev/null
    source "$signing_env"
  fi
fi

: "${APPLE_SIGNING_IDENTITY:?Set the Developer ID Application signing identity}"
: "${APPLE_TEAM_ID:?Set the Apple Developer team ID}"
case "$APPLE_SIGNING_IDENTITY" in
  "Developer ID Application: "*) ;;
  *) echo 'A Developer ID Application identity is required for a release.' >&2; exit 1 ;;
esac

# Tauri otherwise allows a signed build without notarization credentials.
if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then
  : # APPLE_TEAM_ID above completes Apple-ID authentication.
else
  : "${APPLE_API_ISSUER:?Set the App Store Connect API issuer ID}"
  : "${APPLE_API_KEY:?Set the App Store Connect API key ID}"
  : "${APPLE_API_KEY_PATH:?Set the path to the App Store Connect private key}"
  test -s "$APPLE_API_KEY_PATH"
fi

if [ "$#" -eq 0 ]; then set -- build; fi
npx --no-install tauri "$@"

# The release workflow builds the host architecture in the default target dir.
app="$root/crates/chimaera-app/target/release/bundle/macos/chimaera.app"
codesign --verify --deep --strict "$app"
details=$(codesign -dvvv "$app" 2>&1)
if ! printf '%s\n' "$details" | grep -Fxq "Authority=$APPLE_SIGNING_IDENTITY"; then
  echo 'The app was not signed with the requested Developer ID certificate.' >&2
  exit 1
fi
if ! printf '%s\n' "$details" | grep -Fxq "TeamIdentifier=$APPLE_TEAM_ID"; then
  echo 'The app was signed by a different Apple Developer team.' >&2
  exit 1
fi
xcrun stapler validate "$app"
spctl --assess --type execute --verbose=2 "$app"

# Wasmtime must execute inside this hardened-runtime signature, not merely
# inside an unsigned cargo-test binary. Test assets are prepared before build.
node "$root/scripts/smoke-macos-plugins.mjs" "$app"

# Tauri notarizes the app before packaging; Gatekeeper also assesses the DMG.
# Reject missing/ambiguous output rather than publish an unchecked container.
shopt -s nullglob
dmgs=("$root/crates/chimaera-app/target/release/bundle/dmg/"*.dmg)
if [ "${#dmgs[@]}" -ne 1 ]; then
  echo 'Expected exactly one release DMG to notarize.' >&2
  exit 1
fi
bash "$root/scripts/notarize-macos-dmg.sh" "${dmgs[0]}"
