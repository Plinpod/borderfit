#!/usr/bin/env bash
# Bumps the version (Cargo.toml is the single source), commits, tags and pushes.
# Usage: scripts/release.sh 0.1.0-alpha.1
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:?usage: scripts/release.sh X.Y.Z[-pre]}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
    echo "not a semver version: $version" >&2
    exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
    echo "working tree is not clean" >&2
    exit 1
fi
if [ "$(git rev-parse --abbrev-ref HEAD)" != "main" ]; then
    echo "releases are cut from main" >&2
    exit 1
fi

# Only the first `version = ` line (the package's); awk + mv instead of GNU-only `sed -i 0,/re/`.
awk -v v="$version" '!done && /^version = "/ { sub(/".*"/, "\"" v "\""); done = 1 } { print }' \
    src-tauri/Cargo.toml >src-tauri/Cargo.toml.tmp
mv src-tauri/Cargo.toml.tmp src-tauri/Cargo.toml
cargo update -w --manifest-path src-tauri/Cargo.toml
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "chore(release): v$version"
git tag "v$version"
git push origin main "v$version"
