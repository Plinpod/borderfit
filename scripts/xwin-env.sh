# Shared environment for cross-building BorderFit from WSL with cargo-xwin. Source it; don't run it.
#
# Ubuntu 22.04's llvm-rc 14 compiles a resource file that makes the exe fail at launch
# (tauri#10164), so put llvm-rc 18 first on PATH: the apt.llvm.org install, else a user-space copy.
for dir in /usr/lib/llvm-18/bin "$HOME/.local/opt/llvm18/bin"; do
    if [ -x "$dir/llvm-rc" ]; then
        export PATH="$dir:$PATH"
        break
    fi
done
if ! llvm-rc /? 2>/dev/null | head -1 | grep -q 'LLVM Resource Converter'; then
    echo "xwin-env: llvm-rc not found; install llvm-18 (see CONTRIBUTING.md)" >&2
    return 1
fi

# Run Windows binaries (tests, `tauri dev`) through WSL interop instead of wine.
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER=/usr/bin/env
