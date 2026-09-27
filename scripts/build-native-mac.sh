#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
sdk_root="${1:?Pass the extracted Adobe UXP Hybrid SDK directory}"
target="${2:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$target" in
  aarch64-apple-darwin) arch=arm64; bundle_arch=arm64 ;;
  x86_64-apple-darwin) arch=x86_64; bundle_arch=x64 ;;
  *) echo "Unsupported macOS target: $target" >&2; exit 1 ;;
esac
test -f "$sdk_root/src/api/UxpAddonShared.h"
cd "$repo_root"
addon="$(node -p 'require("./plugin/manifest.json").addon.name')"
export MACOSX_DEPLOYMENT_TARGET=12.0
cargo build --locked --release --lib --target "$target"
out="$repo_root/target/native/$bundle_arch"
mkdir -p "$out" "$repo_root/plugin/mac/$bundle_arch"
xcrun clang++ -dynamiclib -arch "$arch" -std=c++17 -O2 -fvisibility=hidden \
  -I"$sdk_root/src/utilities" -I"$sdk_root/src/api" native/addon.cpp \
  "target/$target/release/libbanding.a" -framework CoreFoundation -framework Security \
  -Wl,-dead_strip -o "$out/$addon"
# Ad-hoc signing permits local development. Distribution requires Developer ID
# signing and notarization; see docs/build-and-ci.md and sign-mac.sh.
codesign --force --sign - "$out/$addon"
xcrun clang++ -arch "$arch" -std=c++17 -O2 -I"$sdk_root/src/api" \
  native/smoke.cpp -o "$out/native-smoke"
"$out/native-smoke" "$out/$addon"
cp "$out/$addon" "$repo_root/plugin/mac/$bundle_arch/$addon"
