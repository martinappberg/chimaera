#!/usr/bin/env bash
# Verify the signed distribution container, not only the app nested inside it.
set -euo pipefail

if [ "$#" -ne 1 ] || [ ! -f "$1" ] || [[ "$1" != *.dmg ]]; then
  echo 'Usage: notarize-macos-dmg.sh /path/to/signed.dmg' >&2
  exit 1
fi
dmg=$1

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

if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then
  auth=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
else
  : "${APPLE_API_ISSUER:?Set the App Store Connect API issuer ID}"
  : "${APPLE_API_KEY:?Set the App Store Connect key ID}"
  : "${APPLE_API_KEY_PATH:?Set the path to the App Store Connect private key}"
  test -s "$APPLE_API_KEY_PATH"
  auth=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
fi

codesign --verify --strict "$dmg"
details=$(codesign -dvvv "$dmg" 2>&1)
if ! printf '%s\n' "$details" | grep -Fxq "Authority=$APPLE_SIGNING_IDENTITY" ||
   ! printf '%s\n' "$details" | grep -Fxq "TeamIdentifier=$APPLE_TEAM_ID"; then
  echo 'The DMG was not signed with the expected Developer ID certificate and team.' >&2
  exit 1
fi

result=$(mktemp)
trap 'rm -f "$result"' EXIT
# A successful submission alone is insufficient: require Apple's final verdict.
if xcrun notarytool submit "$dmg" "${auth[@]}" --wait --timeout 20m --output-format json >"$result"; then
  :
else
  status=$?
  cat "$result" >&2
  exit "$status"
fi
cat "$result"
if [ "$(plutil -extract status raw -o - "$result")" != Accepted ]; then
  echo 'Apple did not accept the DMG for notarization.' >&2
  exit 1
fi
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
codesign --verify --strict "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
