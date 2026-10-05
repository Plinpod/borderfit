# Fitting fullscreen browser videos (Chrome/Edge)

Status: works, with one known limitation (mouse presses on the player while fitted). Decision on
2026-10-02: document it and revisit later.

## What works

Fullscreen a video (YouTube etc.) in Chrome or Edge, focus it and press the hotkey: the fullscreen
window is fitted to the region like any game.

- **Another window activated or dragged over the video:** Chromium resizes its fullscreen window
  back to the whole monitor (see below). The pid-scoped `EVENT_OBJECT_LOCATIONCHANGE` hook
  (`hooks.rs`) reports the move and the engine puts the window back at once
  (`Engine::on_location_change` → `verify_if_due`, per-burst re-apply budget).
- **Leaving fullscreen** (Esc, `f`, the player's button): the window regains `WS_THICKFRAME`
  (Chrome's custom-drawn frame has no `WS_CAPTION` bit), `WS_CAPTION` or `WS_MAXIMIZE`, so
  `Engine::left_fullscreen` releases it without writing the captured fullscreen geometry back.
- **Keyboard player controls** (`f`, Esc, space, arrows) work normally.

## Known limitation: mouse presses on a fitted fullscreen Chrome window

A mouse press inside the fitted video makes Chrome jump to the full monitor for about 10–20 ms
before BorderFit puts it back. The press is handled at monitor size, so it can land on the wrong
element: clicking the player's fullscreen button (or other control-bar buttons) often does
nothing. A press that stays on the video area itself still reaches the video.

User-facing workaround (README → Troubleshooting): use the keyboard for player controls while a
video is fitted.

### Cause (Chromium source)

`ui/views/win/hwnd_message_handler.cc`, `HWNDMessageHandler::OnWindowPosChanging`: for a
top-level window that is fullscreen, on the same monitor as last time, not snapped
(`IsWindowArranged`) and not in the "background fullscreen" state, Chromium overwrites every
`WM_WINDOWPOSCHANGING` with the monitor rect, even a z-order-only change (it falls back to
`GetWindowRect` when `SWP_NOMOVE|SWP_NOSIZE` are set). Every mouse press makes Windows re-order
the clicked window (`OnMouseActivate` returns `MA_ACTIVATE`), so every press goes through this.

BorderFit's own `SetWindowPos` uses `SWP_NOSENDCHANGING`, which is why its moves stick at all.
Without that flag Chrome refuses the fit outright (verified).

Related: `PostProcessActivateMessage` / `OnBackgroundFullscreen` shrink the window to the monitor
minus 1 px when another window on the same monitor is activated and restore the monitor rect when
Chrome is activated again; while that "background fullscreen hack" flag is set,
`OnWindowPosChanging` does not enforce. Activation always clears it, so it cannot be held while
the user interacts with the video.

### Evidence

On a 5120×1440 ultrawide (Chrome, region 2560×1440 centred), `move:` log lines: each press
(`lbutton down`, `focus target`) is followed by `5120×1440 … style 0x160b0000`, then BorderFit's
re-apply 9–14 ms later; only the final long press exited fullscreen.

This PC (Chrome, 1920×1080, region 1280×720), `tests/chrome_repro.rs`:

```
 12227 ---- normal click on exit button: down at 1520,850
 12235 MOVE 1920×1080 at 0, 0 style 0x160b0000      <- Chrome on the press, no focus change
 12257 MOVE 1280×720 at 320, 180 style 0x160b0000   <- BorderFit puts it back
 12311 ---- normal click on exit button: up
 12449 PAGE … click:BODY x1200                      <- the click missed the button
```

### Tried and ruled out

| Idea | Result |
|---|---|
| Wait until Chrome's "Press Esc" bubble (an owned popup above the window) has hidden | Still resets on every press (8 s after the fit) |
| Make the fitted window topmost | Still resets |
| Send `WM_WINDOWPOSCHANGING` with our resize (drop `SWP_NOSENDCHANGING`) | Chrome rewrites it to the monitor rect; the fit never sticks |
| Keep Chrome at monitor size and clip it with `SetWindowRgn` | Not built: the player lays out for the whole monitor, so its control bar (fullscreen button at the far right) would be clipped away |

### Ideas for a later revisit

- **Firefox:** different window code; may not enforce fullscreen bounds. Untested (not installed
  on the dev PC). Its `full-screen-api.ignore-widgets` pref makes fullscreen stay inside the
  window, which BorderFit can then fit as a plain window.
- **"Fullscreen in a window" browser extensions** that turn the fullscreen request into a plain
  popup window: BorderFit fits plain windows without any fight. Untested.
- **Snap state:** `OnWindowPosChanging` skips windows where `IsWindowArranged` is true. There is
  no public API to set it; simulating Snap keystrokes is too invasive.
- Re-check with newer Chromium releases: the enforcement code may change.

### Reproduce

On a Windows desktop nobody is using (the test takes over the mouse), from WSL:

```
scripts/xwin.sh test --test chrome_repro -- --ignored --nocapture
```

It launches Chrome with a throwaway profile and a test page, enters fullscreen, fits it with the
real engine and hook thread, clicks the page's exit button normally and with a long press, and
prints foreground changes, window moves and page mouse events in milliseconds. On a user's
machine, the `move:` lines in `%LOCALAPPDATA%\app.borderfit\logs\borderfit.log` give the same
picture.
