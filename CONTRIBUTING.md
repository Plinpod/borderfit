# Contributing to BorderFit

## Prerequisites

- Node 22+ and pnpm 10 (`corepack enable`), Rust stable (`rust-toolchain.toml` adds the MSVC target).
- **On Windows:** nothing else; `pnpm tauri dev` works as usual.
- **On Linux (logic tests only):** `cargo test` compiles the tauri crate, so install its system
  libraries first (the same list CI uses): `sudo apt-get install -y libwebkit2gtk-4.1-dev
  build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`.
- **On WSL (cross-build):** `cargo install --locked cargo-xwin`, `clang`, `lld`, `nsis`, and **llvm-rc 18**:
  Ubuntu 22.04's llvm-rc 14 links an exe that fails at launch (tauri#10164). Either
  `wget https://apt.llvm.org/llvm.sh && sudo bash llvm.sh 18`, or without sudo extract the `llvm-18` and
  `libllvm18` debs from apt.llvm.org into `~/.local/opt/llvm18` and add a `bin/llvm-rc` wrapper that sets
  `LD_LIBRARY_PATH`. `scripts/xwin-env.sh` picks up either location.

## Dev loop

| Goal | Command |
|---|---|
| UI only, in a browser (mock backend) | `pnpm dev`, open http://localhost:1420 |
| Logic tests (Linux or Windows) | `pnpm test` |
| Windows tests from WSL (runs the test exe on Windows via interop) | `pnpm test:win` |
| Windows tests that move real windows and hide the taskbar (`#[ignore]`d by default) | `scripts/xwin.sh test --test win32 -- --ignored` |
| Chrome fullscreen-click repro (real Chrome + mouse; see `docs/browser-fullscreen.md`) | `scripts/xwin.sh test --test chrome_repro -- --ignored --nocapture` |
| Clippy for the Windows code paths | `pnpm clippy:win` |
| Live app from WSL | `source scripts/xwin-env.sh && pnpm tauri dev --runner cargo-xwin --target x86_64-pc-windows-msvc` |
| Installer + portable exe, copied to `C:\Users\<you>\borderfit-dev` | `pnpm xbuild` |
| Everything the Linux CI jobs check (lint, build, tests, fmt, clippy) | `pnpm check` |
| What the Windows CI job adds | `pnpm clippy:win` and `pnpm test:win` |

If a manual test leaves the taskbar hidden or a window borderless: restart Explorer, then run
BorderFit once (startup recovery restores what the marker describes).

## Architecture in 40 lines

```
src/                      Vue 3 UI: one screen, no router, no store library
  state.ts                one reactive AppState, fed by get_app_state + the status/notice/hotkey/settings events
  api.ts, types.ts        typed invoke wrappers; types.ts mirrors src-tauri/src/model.rs by hand
  geometry.ts             pure helpers (size chips, SVG view box); placement math lives in Rust
  lib/ipc.mock.ts         the backend for `pnpm dev` in a browser
src-tauri/src/
  main.rs                 SetProcessDpiAwarenessContext(PMv2) first, then lib::run()
  lib.rs                  builder: single-instance → log → commands → setup → window/run events
  state.rs                Core (managed state), the prepare_for_exit() funnel, the engine's event sink
  commands.rs             every #[tauri::command]; the only module that knows AppHandle and Engine
  engine.rs               the engine thread: Idle/Fitted, fit and restore sequences, 150 ms tick
  taskbar.rs              explorer-owned tray discovery each call, reconcile(desired)
  recovery.rs             restore marker, shared restore sequence, emergency path, startup adopt
  region.rs               pure placement math (presets, alignment, 25% overlap validation)
  settings.rs             settings.json load/migrate/atomic save, AHK INI import
  hotkey.rs               chord normalization + global-hotkey registration on the main thread
  keyhook.rs              WH_KEYBOARD_LL fallback for chords Windows refuses (F12), own thread
  hooks.rs                WinEvent hook thread (destroy/hide/cloak/move of the tracked window)
  tray.rs                 tray icon, tooltip and menu; updates always posted to the main thread
  win32/mod.rs            trait WindowManager: the only door to Windows, plain data in signatures
  win32/windows.rs        the Win32 implementation + shell helpers (elevation, autostart, subclass)
  win32/fake.rs           in-memory FakeWindowManager with a call log for tests
```

Invariants ([docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)): the restore marker is fsync'd before the first change to a foreign window
or the taskbar and deleted only after a full restore; the taskbar is hidden only while fitted, the
game is visible and "Hide taskbar" is on (re-derived every tick, never stored); restore is
idempotent and tolerates a missing window; the engine thread is the only writer of foreign windows.

## Rules

- Every `unsafe` block has a `// SAFETY:` comment; `unsafe_op_in_unsafe_fn` is denied.
- The engine thread never calls Tauri window or tray methods and never holds `Core` locks across
  calls; it emits events and posts closures to the main thread. The hotkey handler only sends on
  a channel.
- Logic goes behind `WindowManager` with a test over `FakeWindowManager`.
- **Techniques yes, GPL code no** (see [docs/PRIOR-ART.md](docs/PRIOR-ART.md)).
- No new dependencies without an issue. Style: `cargo fmt`, prettier (4 spaces, no semicolons).

## Releasing

1. `main` is clean and CI is green; `pnpm check`, `pnpm clippy:win` and `pnpm test:win` pass.
2. Manual matrix on a real desktop: three games (one Unity, one DX12); maximized/minimized/snapped
   targets; hotkey rebind and a conflict; taskbar hide/restore; close to tray; Exit restores; kill
   BorderFit from Task Manager while fitted → next launch adopts the window; log off while fitted →
   taskbar visible at next login; elevated Notepad → needs-admin card → relaunch as admin; autostart
   with `--minimized`; a 150% secondary display; Explorer restart while fitted; Win key with the
   taskbar hidden.
3. `scripts/release.sh X.Y.Z` (bumps `src-tauri/Cargo.toml`, the single version source; commits; tags; pushes).
4. Wait for `release.yml`; check the draft has the setup exe, the portable exe, `BorderFit_x64-setup.exe`
   and `SHA256SUMS.txt`.
5. Install, run and uninstall in a clean VM (Windows Sandbox or a throwaway VM, networking on for the
   WebView2 bootstrapper). Then publish the draft.
