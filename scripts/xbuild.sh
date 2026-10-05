#!/usr/bin/env bash
# Cross-builds the NSIS installer and the portable exe from WSL, then copies both to the
# Windows side (default C:\Users\<you>\borderfit-dev) so they can be launched from Explorer.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=xwin-env.sh
source "$here/xwin-env.sh"
cd "$here/.."

pnpm tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis "$@"

windows_user() {
    (cd /mnt/c && /mnt/c/Windows/System32/cmd.exe /c 'echo %USERNAME%' 2>/dev/null | tr -d '\r')
}
out="${BORDERFIT_DEV_DIR:-/mnt/c/Users/$(windows_user)/borderfit-dev}"
mkdir -p "$out"

release="src-tauri/target/x86_64-pc-windows-msvc/release"
cp "$release/BorderFit.exe" "$out/BorderFit.exe"
cp "$release"/bundle/nsis/*-setup.exe "$out/"
echo "Copied BorderFit.exe and the installer to $out"
