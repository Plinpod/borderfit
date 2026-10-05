#!/usr/bin/env bash
# Runs `cargo xwin <subcommand>` against the Windows target from WSL.
# Examples: scripts/xwin.sh test --tests    scripts/xwin.sh clippy --all-targets -- -D warnings
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=xwin-env.sh
source "$here/xwin-env.sh"
cd "$here/../src-tauri"

subcommand="$1"
shift
exec cargo xwin "$subcommand" --target x86_64-pc-windows-msvc "$@"
