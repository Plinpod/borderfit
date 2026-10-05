#!/usr/bin/env bash
# Regenerates the app/installer icons and the two tray icons from the SVG masters in assets/.
set -euo pipefail
cd "$(dirname "$0")/.."

pnpm tauri icon assets/icon.svg -o src-tauri/icons
# Drop the mobile/Store variants the CLI always writes; BorderFit is Windows desktop only.
rm -rf src-tauri/icons/android src-tauri/icons/ios src-tauri/icons/Square*Logo.png src-tauri/icons/StoreLogo.png

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
for state in idle fitted; do
    pnpm tauri icon "assets/tray-$state.svg" -o "$tmp/$state" -p 32
    cp "$tmp/$state/32x32.png" "src-tauri/icons/tray-$state.png"
done
