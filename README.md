# BorderFit

Fit any game window borderless into a region of your monitor, hide the taskbar while it is fitted, and put everything back exactly with the same key.

Built for ultrawides: on a 49-inch 5120×1440 panel, press **Ctrl+Alt+B** in a windowed game and it becomes a borderless 2560×1440 window centered on the screen, with nothing else in view. Press it again and the game gets its borders, size and position back.

BorderFit is the successor to the [BorderlessGaming AutoHotkey script](https://github.com/Techozu/BorderlessGaming): same idea, rebuilt as a small Windows app with a monitor picker, crash-safe taskbar handling and diagnostics.

**No injection, no memory reads.** BorderFit only calls public Windows window APIs (`SetWindowLongPtr`, `SetWindowPos`, `ShowWindow`) and registers its hotkey with `RegisterHotKey`, so it does not touch a game's process. When Windows refuses a key (it reserves F12 for debuggers), BorderFit listens for that one key with a keyboard hook in its own process, as AutoHotkey does.

## Install

Download `BorderFit_x64-setup.exe` from [Releases](https://github.com/Plinpod/borderfit/releases/latest) (per-user install, no admin rights), or the portable `BorderFit-<version>-portable.exe`.

- Using the portable exe? Turn off **Launch at startup** before deleting it; only the installer's uninstaller removes the startup entry for you.
- Windows 10 or 11, x64. The installer fetches Microsoft's WebView2 runtime if it is missing (Windows 11 already has it).
- The installer is **not code-signed yet**, so SmartScreen may say "Windows protected your PC": click **More info → Run anyway**. Each release lists SHA-256 checksums in `SHA256SUMS.txt` so you can verify the download (`Get-FileHash .\BorderFit_x64-setup.exe`), and the exes carry a GitHub build attestation that proves they were built from this repository by its release workflow (`gh attestation verify .\BorderFit_x64-setup.exe --repo Plinpod/borderfit`). Code signing is on the roadmap.

## Use it

1. Pick where games go: a display, a size (Full, 2560×1440 · 16:9, 3440×1440, …) and Left / Center / Right. Or type exact X/Y/W/H.
2. Switch your game to **windowed** mode, focus it and press **Ctrl+Alt+B**.
3. Press **Ctrl+Alt+B** again (or click **Restore**, or use the tray menu) to put it back. Closing the game also brings the taskbar back.

**Browser videos** work too: fullscreen the video (YouTube etc.), then press the hotkey. Leaving fullscreen (Esc or the player's button) lets the window go by itself.

Close the window and BorderFit keeps running in the tray. Turn on **Launch at startup** to have it ready after every boot.

## How it differs from the AHK script

| | BorderlessGaming (AHK) | BorderFit |
|---|---|---|
| Region | Absolute X/Y/W/H | Monitor picker + aspect-correct size presets, or exact X/Y/W/H relative to the chosen display |
| Restore | ORs the border bits back (can add a caption the window never had) | Writes the saved style words, placement and rect back verbatim |
| Taskbar | Hidden with `SW_HIDE`; stays hidden if the script dies | Re-derived every 150 ms, re-hidden after Explorer restarts, and restored after a crash or logoff from a marker file |
| Hotkey | F12 (Steam's screenshot key, reserved by Windows for debuggers) | Ctrl+Alt+B by default; any F-key, Pause or ScrollLock alone, other keys with Ctrl/Alt/Win |
| Settings | INI + Win32 dialog | One-screen app; drop your `borderless_config.ini` on the window to import it |

Two behaviours changed on purpose:

- **Switching games takes two presses.** While a window is fitted, the hotkey restores it no matter which window is in front (so a stray press in Discord can never fit Discord). Press again with the next game focused to fit it.
- **The first-run region** is 2560×1440 centered when your primary display is wider than 2:1 and big enough, otherwise the full display (the script defaulted to half the virtual desktop width).

## Troubleshooting

- **The hotkey does nothing in one game.** If Win+D also does nothing there, the game blocks app hotkeys; use the tray icon's **Fit active window** instead.
- **"It runs as administrator".** Windows does not let a normal app move an elevated window. Click **Restart BorderFit as admin** in the status card.
- **The size is not accepted.** Some games force their own size. Set the game to windowed at the target size in its own settings, then press the hotkey twice.
- **Clicks on a fitted browser video miss (Chrome, Edge).** While a fullscreen browser window is fitted, Chrome briefly snaps it back to the whole monitor on every mouse press, so clicks on the player's buttons (for example its fullscreen button) can miss. Use the keyboard instead: `f` or Esc to leave fullscreen, space to pause. Details: [docs/browser-fullscreen.md](docs/browser-fullscreen.md).
- **Exclusive fullscreen.** BorderFit cannot help games that only offer exclusive fullscreen; switch them to windowed or borderless first.
- **Escape hatch.** If the taskbar ever stays hidden, restart Explorer from Task Manager (Details → explorer.exe → End task, then File → Run new task → `explorer`), then start BorderFit once: it shows a hidden taskbar at startup.
- Logs: `%LOCALAPPDATA%\app.borderfit\logs\borderfit.log` (Diagnostics → Open logs folder). Settings: `%APPDATA%\app.borderfit\settings.json`.

### Advanced settings (JSON only)

In `settings.json`, `profile.taskbar.hide_secondary_taskbars: true` also hides the taskbars on your other displays.

## Not goals

- Games that only run in exclusive fullscreen.
- Injection, patching or reading game memory.
- 32-bit or ARM64 Windows; tracking more than one window at a time.
- A separate "hide the Start button" option ([#4](https://github.com/Techozu/BorderlessGaming/issues/4)): BorderFit never intercepts the Win key, so Start stays reachable from the keyboard while the taskbar is hidden, and Windows 11's Start button has no window of its own.

## Roadmap

1. **v1.1** Cursor lock to the fitted window (opt-in).
2. **v1.2** Auto-update, a code-signed installer, and `winget install Plinpod.BorderFit`.
3. **v1.3** Per-game profiles that apply automatically when a game starts.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for the dev loop (WSL cross-build or Windows), the architecture and the rules. How it works and why: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## License

[MIT](LICENSE) © 2026 Ironweb. Built with [Tauri](https://tauri.app); WebView2 is Microsoft's runtime.
