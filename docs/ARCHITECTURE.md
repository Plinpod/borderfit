# Architecture

How BorderFit works and why it is built this way. The module map is in
[CONTRIBUTING.md](../CONTRIBUTING.md#architecture-in-40-lines); this file covers the rules that
keep a game window and the taskbar safe, and the Windows behaviour those rules rely on.

## Invariants

1. **Marker first.** The restore marker (`%LOCALAPPDATA%\app.borderfit\restore-state.json`) is
   written and fsync'd before the first change to a foreign window or the taskbar, and deleted
   only after a full restore. Fitted in memory implies a marker on disk.
2. **Taskbar derived, never stored.** The taskbar is hidden exactly while a window is fitted, that
   window is visible and "Hide taskbar" is on. The desired state is re-derived every tick.
3. **Restore is idempotent and total.** Every path (user, process exit, shutdown, crash recovery,
   emergency) runs the same ordered sequence, which tolerates a missing window.
4. **One writer.** While it is alive, the engine thread is the only writer of foreign windows and
   taskbars. The emergency path runs only when the engine is dead or hung, guarded by the
   `EMERGENCY` flag; the engine clears the flag when it notices and drops its session without
   touching anything.
5. **The main thread never waits on the engine** except during shutdown. The engine only emits
   events and posts closures to the main thread; it never calls Tauri window or tray methods.

## Threads

| Thread | Owns | Never |
|---|---|---|
| Main (Tauri event loop) | webview, tray, the `GlobalHotKeyManager` (`thread_local!`, reached through `run_on_main_thread`), the subclass on the hidden main window (`TaskbarCreated`, session end) | blocks on the engine outside shutdown |
| Engine (`engine.rs`) | every write to foreign windows and taskbars, the marker, the 150 ms tick | calls Tauri window/tray methods, holds `Core` locks across calls |
| WinEvent hooks (`hooks.rs`) | pid-scoped destroy/hide/cloak/move hooks for the tracked window | does anything in the callback but send on the channel |
| Keyboard hook (`keyhook.rs`) | the `WH_KEYBOARD_LL` hook, only while the hotkey is a chord Windows refused | stalls (Windows drops slow hooks) |
| Tokio workers | `async` commands that wait (settings save, hotkey capture) | block outside `spawn_blocking` |

Everything reaches the engine as a `Msg` on one `std::sync::mpsc` channel.

## Fit

`Engine::fit` makes no change at all until the marker is written; a failure after that rolls
back through the restore sequence.

1. **Target.** The hotkey fits the foreground window only (its root). Tray and UI fits, where
   BorderFit itself is in front, take the foreground window if it is fittable, else the first
   fittable window in z-order. Never fittable: BorderFit, shell windows, hidden or cloaked
   windows, tool windows, the UWP host frame.
2. **Access.** A target with a higher integrity level, or one whose no-op `SetWindowPos` fails
   with `ERROR_ACCESS_DENIED`, is refused with "needs admin" (UIPI).
3. **Capture** style, ex-style, placement and `GetWindowRect`. A minimized, maximized or snapped
   window gets a provisional rect.
4. **Resolve** the region on the configured monitor (missing monitor: primary plus a warning).
   Open the process handle.
5. **Write the marker.**
6. A provisional window is restored first (`SW_RESTORE`, twice at most), re-read, and the marker
   is rewritten.
7. Track the pid on the hook thread; `SetForegroundWindow`, reconcile the taskbar,
   `SetForegroundWindow` again (the hotkey press grants foreground rights).
8. Clear `WS_CAPTION | WS_THICKFRAME` and the edge ex-styles, square the DWM corners, one
   `SetWindowPos` to the target.
9. **Verify** at +150, +500 and +2000 ms and on every move of the tracked window. A mismatch
   is re-applied, up to 4 times per burst. A window that held the target for 1 s starts a new
   burst (browsers reset on activation); one that keeps pushing back ends as "size not
   accepted" and stays fitted.

A window fitted while frameless (fullscreen) that regains a frame or `WS_MAXIMIZE` left fullscreen
by itself (Esc in a browser video). BorderFit releases it without writing the captured
fullscreen geometry back. See [browser-fullscreen.md](browser-fullscreen.md) for the browser
details and the one known limitation.

## Tick (every 150 ms while fitted)

1. The process handle is signalled: restore, skipping window calls. The handle wait is
   authoritative; a false `IsWindow` alone is not trusted (UIPI) and needs a
   `ERROR_INVALID_WINDOW_HANDLE` from `GetWindowRect` to count.
2. Visible = `IsWindowVisible` and not minimized.
3. Reconcile the taskbar against the derived state (secondary taskbars only when enabled), with
   trays rediscovered on every call.
4. Run a due verify step.

## Restore (`recovery::restore_sequence`)

Each step is logged and the next one runs regardless.

1. Untrack the pid. Put the topmost bit back through z-order (never through the ex-style word);
   reset the DWM corners.
2. **Taskbar first:** `SW_SHOWNA` on every hidden explorer tray, re-check, retry once. Done before
   the window so a failure half-way leaves the less harmful state.
3. Write the saved style words verbatim (`WS_EX_TOPMOST` masked out).
4. Geometry by the saved show state: normal goes back with `SetWindowPos(saved rect)`; maximized
   or provisional with `SetWindowPlacement` plus a frame refresh; minimized with
   `SetWindowPlacement` then `SW_SHOWMINNOACTIVE`. Screen coordinates only ever go to
   `SetWindowPos`, workspace coordinates only to `SetWindowPlacement`.
5. Close the process handle; delete the marker if every tray came back.

## Crash safety and shutdown

- **Startup** (`recovery::run_at_startup`): a marker whose window is still alive (same pid and
  process creation time) is **adopted**, so the game is not yanked back under the player; the
  next hotkey press restores it. A marker whose window is gone, or an unreadable marker, restores
  the taskbar. With no marker, a hidden explorer taskbar is shown anyway: "start BorderFit once"
  is the documented fix for a taskbar left hidden by anything.
- **Exit** goes through `Core::prepare_for_exit`, one idempotent funnel: ask the engine to restore
  and stop (3 s), else run the emergency restore from the marker; stop the hook thread; release
  the single-instance lock. Tray Exit, closing with close-to-tray off, the elevated relaunch,
  `RunEvent::Exit`, a main-thread panic and session end all use it. `Drop for Engine` restores
  as a last resort, and release builds keep `panic = "unwind"` so it runs.
- **Single instance:** `tauri-plugin-single-instance` plus a `Local\BorderFit` event with an
  explicit SDDL, because the plugin cannot see an elevated first instance. Both are per logon
  session, so another user on the same PC runs their own BorderFit.

## Hotkey

Chords are normalized to `Ctrl+Alt+Shift+Super+Key` (`hotkey::normalize`, the allow-list is
pinned to `global-hotkey` 0.8.0). Registration is `RegisterHotKey` through `global-hotkey` on the
main thread; the handler is installed before the manager exists and only sends `Msg::Toggle` on
`Pressed`. When Windows refuses a chord (F12 is reserved for debuggers, or another app holds it),
a `WH_KEYBOARD_LL` hook in BorderFit's own process listens for that one chord and swallows it.
Nothing is injected into games. The hotkey while fitted always restores, whatever is in front.

## Settings

`%APPDATA%\app.borderfit\settings.json`, every struct `#[serde(default)]` so new fields are
additive. Loading runs a migration ladder on `schema_version`; an unreadable file is moved aside
as `settings.json.broken-<timestamp>` and defaults are used. Saves are atomic (write `.tmp`, fsync,
rename; the marker uses the same helper). Regions are stored monitor-relative and resolved to
absolute coordinates at fit time, so a rearranged monitor layout doesn't break a saved fit.

Two writers exist. The UI saves the whole object (`save_settings`, debounced). Rust changes a
few fields on its own (the recorded hotkey, `first_run_done`, `legacy_f12_ack`) through
`Core::update_settings`, which persists them and emits a `settings` event so the UI's copy
follows.

## IPC

Commands live in `commands.rs`; `src/types.ts` mirrors `model.rs` by hand and the `json_shape`
test pins the wire format. `restore_window` is fire-and-forget: the result arrives as a `status`
event. Fits are started by the hotkey or the tray, never by the UI. Command errors reject with a
`Notice` (`{ level, error, message }`).

| Event | Payload |
|---|---|
| `status` | `FitStatus`: idle or fitted, window, target, taskbar state, verify attempts, warning |
| `notice` | `Notice`: a one-off message for the status card |
| `hotkey` | `HotkeyStatus`: chord, live or not, heard through the keyboard hook, error |
| `settings` | `Settings` after a change Rust made on its own |

## Windows behaviour BorderFit relies on

| Decision | Why | Source |
|---|---|---|
| Hide the taskbar with `ShowWindowAsync(SW_HIDE)` on explorer-owned `Shell_TrayWnd`, show with `SW_SHOWNA`, never touch `SPI_SETWORKAREA` | no supported API hides the taskbar; `SW_SHOW` would steal focus; Explorer restarts recreate a visible taskbar | [Taskbar](https://learn.microsoft.com/en-us/windows/win32/shell/taskbar), [ShowWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow) |
| Rediscover trays every call and keep only explorer.exe-owned windows of the right class | third-party bars reuse the class; Windows 11 21H2 `FindWindow` returned wrong-class handles | [Q&A 956408](https://learn.microsoft.com/en-us/answers/questions/956408/findwindow-findwindowex-broken-on-windows-11), [windhawk-mods#1704](https://github.com/ramensoftware/windhawk-mods/issues/1704) |
| Hear `TaskbarCreated` on an unowned top-level window, allowed through UIPI | Explorer restarts and primary DPI changes | [Window features](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features) |
| Clear only `WS_CAPTION \| WS_THICKFRAME` and the edge ex-styles; restore the saved words verbatim | never clear layered/app/tool-window bits; OR-ing a mask back adds a caption to windows that never had one | [Extended window styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles), [SetWindowLongPtrW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw) |
| Restore a minimized/maximized window before reading its rect; keep placement and rect apart | a minimized rect is −32000; `WINDOWPLACEMENT` is in workspace coordinates | [Old New Thing](https://devblogs.microsoft.com/oldnewthing/20041028-00/?p=37453), [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement) |
| Default hotkey `Ctrl+Alt+B`, F12 only through the keyboard hook | `RegisterHotKey` reserves F12 for debuggers | [RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey) |
| `SetForegroundWindow` on the game inside the fit | the hotkey press grants foreground rights | [Old New Thing](https://devblogs.microsoft.com/oldnewthing/20090226-00/?p=19013) |
| Process-handle wait is authoritative, `IsWindow` advisory; probe access before fitting | UIPI blocks handle validation and messages to higher-integrity windows | [IsWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-iswindow), [UIPI](https://learn.microsoft.com/en-us/previous-versions/dotnet/articles/bb625963(v=msdn.10)) |
| Per-Monitor-V2 DPI awareness as the first statement of `main` | tao sets it only inside `EventLoop::new`; earlier geometry is virtualized | [tao event loop](https://github.com/tauri-apps/tao/blob/tao-v0.35.3/src/platform_impl/windows/event_loop.rs) |
| Prevent exit only for `ExitRequested { code: None }` | otherwise tray Exit can't exit | [tauri-runtime-wry](https://github.com/tauri-apps/tauri/blob/tauri-v2.11.5/crates/tauri-runtime-wry/src/lib.rs) |

Games that register raw input with `RIDEV_NOHOTKEYS` suppress app hotkeys; the tray's "Fit active
window" is the fallback.

## UI

One screen, no router, no store library: `src/state.ts` is a single reactive object mirroring
Rust. The size chips (`geometry.ts` `presetsFor`) are Full, aspect rows at monitor height (16:9,
16:10, 4:3, and 21:9 only when 3440×1440 doesn't fit) and common fixed sizes, merged by size,
filtered to what fits the monitor, at most 7. Colours are CSS variables in `src/assets/main.css`
exposed to Tailwind, light by default and dark under `prefers-color-scheme`.
