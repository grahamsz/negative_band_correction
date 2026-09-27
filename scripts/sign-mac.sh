#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
platform="${1:?Pass mac/x64 or mac/arm64}"
case "$platform" in mac/x64|mac/arm64) ;; *) exit 2 ;; esac
: "${APPLE_CERTIFICATE_P12:?Missing certificate secret}"
: "${APPLE_CERTIFICATE_PASSWORD:?Missing certificate password}"
: "${APPLE_SIGNING_IDENTITY:?Missing signing identity}"
: "${APPLE_ID:?Missing Apple ID}"
: "${APPLE_TEAM_ID:?Missing Apple team ID}"
: "${APPLE_APP_PASSWORD:?Missing app-specific password}"
cd "$repo_root"
addon="$(node -p 'require("./plugin/manifest.json").addon.name')"
binary="$repo_root/plugin/$platform/$addon"
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/banding-sign.XXXXXX")"
keychain="$work/signing.keychain-db"
keychain_password="$(uuidgen)"
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -f "$work/certificate.p12" "$work/addon.zip" "$work/signing.keychain-db"
  rmdir "$work" 2>/dev/null || true
}
trap cleanup EXIT
export CERT_FILE="$work/certificate.p12"
python3 -c 'import os,base64; open(os.environ["CERT_FILE"],"wb").write(base64.b64decode(os.environ["APPLE_CERTIFICATE_P12"]))'
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$work/certificate.p12" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null
codesign --force --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" --timestamp --options runtime "$binary"
codesign --verify --strict --verbose=2 "$binary"
ditto -c -k --keepParent "$binary" "$work/addon.zip"
xcrun notarytool submit "$work/addon.zip" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait --output-format json > "$work/notary-result.json"
python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print("Notarization:",r.get("status")); sys.exit(0 if r.get("status")=="Accepted" else 1)' "$work/notary-result.json"
# Bare Mach-O addons cannot carry a stapled ticket. Apple records the accepted
# signature for online verification; distribute the exact signed binary.
rm -f "$work/notary-result.json"
