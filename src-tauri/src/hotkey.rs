//! The global hotkey: chord normalization (pure, tested everywhere) and registration through
//! `global-hotkey` 0.8.0 (`RegisterHotKey`). When Windows refuses a chord (F12 is reserved for
//! the debugger; another app may hold one), it is heard through `keyhook` instead.
//!
//! Threading rules: the `GlobalHotKeyManager` is `!Send` and lives in a `thread_local!` on the
//! main thread, reached only through `run_on_main_thread`. The event handler is installed before
//! the manager exists and only sends on a channel: no locks, no manager calls.

use crate::model::AppError;

/// Canonical key names a chord may use, transcribed from global-hotkey 0.8.0's `parse_key`
/// (media keys, NumLock, NumpadEnter/Equal left out on purpose). Pinned by a Windows test that
/// parses every entry with `HotKey::from_str`.
pub const ALLOWED_KEYS: &[&str] = &[
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "F13",
    "F14",
    "F15",
    "F16",
    "F17",
    "F18",
    "F19",
    "F20",
    "F21",
    "F22",
    "F23",
    "F24",
    "Pause",
    "ScrollLock",
    "PrintScreen",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Backspace",
    "Tab",
    "Enter",
    "Space",
    "Escape",
    "CapsLock",
    "Numpad0",
    "Numpad1",
    "Numpad2",
    "Numpad3",
    "Numpad4",
    "Numpad5",
    "Numpad6",
    "Numpad7",
    "Numpad8",
    "Numpad9",
    "NumpadAdd",
    "NumpadSubtract",
    "NumpadMultiply",
    "NumpadDivide",
    "NumpadDecimal",
    "Backquote",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Semicolon",
    "Quote",
    "Comma",
    "Period",
    "Slash",
];

/// Keys allowed without Ctrl, Alt or Win: they don't type anything.
const BARE_KEYS: &[&str] = &[
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "F13",
    "F14",
    "F15",
    "F16",
    "F17",
    "F18",
    "F19",
    "F20",
    "F21",
    "F22",
    "F23",
    "F24",
    "Pause",
    "ScrollLock",
];

/// Chords Windows or every user relies on; registering them would break the system.
const RESERVED: &[&str] = &[
    "Alt+F4",
    "Alt+Tab",
    "Alt+Shift+Tab",
    "Alt+Escape",
    "Ctrl+Escape",
    "Ctrl+Shift+Escape",
    "Ctrl+Alt+Delete",
    "Super+L",
];

/// Modifiers in canonical output order.
const MODIFIERS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];

fn modifier_name(token: &str) -> Option<&'static str> {
    let name = match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "controlleft" | "controlright" => "Ctrl",
        "alt" | "option" | "menu" | "altleft" | "altright" => "Alt",
        "shift" | "shiftleft" | "shiftright" => "Shift",
        "super" | "win" | "windows" | "meta" | "cmd" | "command" | "os" | "metaleft"
        | "metaright" | "osleft" | "osright" => "Super",
        _ => return None,
    };
    Some(name)
}

/// Canonical key name for a token: a canonical name in any case, a `KeyboardEvent.code`
/// (`KeyB`, `Digit1`) or a common alias (`Esc`, `PgUp`, `Up`).
fn key_name(token: &str) -> Option<&'static str> {
    let lower = token.to_ascii_lowercase();
    let stripped = lower
        .strip_prefix("key")
        .filter(|rest| rest.len() == 1)
        .or_else(|| lower.strip_prefix("digit").filter(|rest| rest.len() == 1))
        .unwrap_or(&lower);
    let alias = match stripped {
        "esc" => "escape",
        "del" => "delete",
        "ins" => "insert",
        "pgup" => "pageup",
        "pgdn" => "pagedown",
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        "return" => "enter",
        "bs" => "backspace",
        "break" | "pausebreak" => "pause",
        "prtsc" | "printscr" | "print" => "printscreen",
        "spacebar" => "space",
        "numpaddot" => "numpaddecimal",
        other => other,
    };
    ALLOWED_KEYS.iter().copied().find(|k| k.eq_ignore_ascii_case(alias))
}

fn invalid(chord: &str, reason: &str) -> AppError {
    AppError::HotkeyInvalid { chord: chord.to_string(), reason: reason.to_string() }
}

/// Normalizes a chord to `Ctrl+Alt+Shift+Super+Key` form, or explains why it can't be used.
pub fn normalize(chord: &str) -> Result<String, AppError> {
    let mut modifiers = [false; 4];
    let mut key: Option<&'static str> = None;
    for token in chord.split('+').map(str::trim) {
        if token.is_empty() {
            return Err(invalid(chord, "That shortcut is not complete."));
        }
        if let Some(m) = modifier_name(token) {
            let index = MODIFIERS.iter().position(|x| *x == m).expect("known modifier");
            modifiers[index] = true;
            continue;
        }
        if key.is_some() {
            return Err(invalid(chord, "Use one key plus modifiers."));
        }
        key = Some(
            key_name(token).ok_or_else(|| invalid(chord, "That key can't be used as a hotkey."))?,
        );
    }
    let key = key.ok_or_else(|| invalid(chord, "Add a key to the modifiers."))?;

    let [ctrl, alt, _shift, win] = modifiers;
    if !(ctrl || alt || win || BARE_KEYS.contains(&key)) {
        return Err(invalid(chord, "Add Ctrl, Alt or Win so typing isn't affected."));
    }
    if ctrl && key == "Pause" {
        return Err(invalid(
            chord,
            "Ctrl+Pause is sent as Break and never fires; pick another key.",
        ));
    }

    let mut parts: Vec<&str> =
        MODIFIERS.iter().zip(modifiers).filter(|(_, on)| *on).map(|(m, _)| *m).collect();
    parts.push(key);
    let normalized = parts.join("+");
    if RESERVED.contains(&normalized.as_str()) {
        return Err(invalid(chord, "Windows uses that shortcut."));
    }
    Ok(normalized)
}

/// How the toggle hotkey is being heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// `RegisterHotKey` accepted it.
    Registered,
    /// Windows refused it; the low-level keyboard hook listens for it instead.
    Hook,
}

#[cfg(windows)]
pub use platform::{create_manager, install_handler, register, registered_chord, unregister};

#[cfg(windows)]
mod platform {
    use super::{AppError, Binding};
    use crate::engine::{FitSource, Msg};
    use crate::keyhook::{self, ChordSpec};
    use global_hotkey::hotkey::HotKey;
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use std::cell::RefCell;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc::Sender;

    const ERROR_HOTKEY_ALREADY_REGISTERED: i32 = 1409;

    /// Id of the registered toggle hotkey (0 = none). Read by the event handler.
    static TOGGLE_ID: AtomicU32 = AtomicU32::new(0);

    thread_local! {
        static MANAGER: RefCell<Option<GlobalHotKeyManager>> = const { RefCell::new(None) };
        /// The active chord; the `HotKey` is `None` while it is heard through the keyboard hook.
        static REGISTERED: RefCell<Option<(Option<HotKey>, String)>> = const { RefCell::new(None) };
    }

    /// Installs the event handler. Must run before `create_manager` and before any
    /// registration (global-hotkey latches the first handler it sees).
    pub fn install_handler(tx: Sender<Msg>) {
        keyhook::set_sender(tx.clone());
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            // Released is synthesized on another thread; act on Pressed only.
            let id = TOGGLE_ID.load(Ordering::SeqCst);
            if event.state == HotKeyState::Pressed && id != 0 && event.id == id {
                let _ = tx.send(Msg::Toggle { src: FitSource::Hotkey });
            }
        }));
    }

    /// Creates the manager's hidden window on the current (main) thread.
    pub fn create_manager() -> Result<(), AppError> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| AppError::win32(&format!("GlobalHotKeyManager ({e})"), 0))?;
        MANAGER.with(|m| *m.borrow_mut() = Some(manager));
        Ok(())
    }

    /// The chord currently registered on this thread.
    pub fn registered_chord() -> Option<String> {
        REGISTERED.with(|r| r.borrow().as_ref().map(|(_, chord)| chord.clone()))
    }

    /// Registers `chord` (normalized) as the toggle hotkey, falling back to the keyboard hook
    /// when Windows says it is taken. Any other active chord is released first, so at most one
    /// is ever held. Main thread only.
    pub fn register(chord: &str) -> Result<Binding, AppError> {
        if let Some(binding) = current_binding(chord) {
            return Ok(binding);
        }
        let hotkey = HotKey::from_str(chord).map_err(|e| AppError::HotkeyInvalid {
            chord: chord.to_string(),
            reason: e.to_string(),
        })?;
        unregister();
        let result = MANAGER.with(|m| {
            let manager = m.borrow();
            let manager =
                manager.as_ref().ok_or(AppError::win32("RegisterHotKey (no manager)", 0))?;
            manager.register(hotkey).map_err(|e| classify(chord, e))
        });
        match result {
            Ok(()) => {
                TOGGLE_ID.store(hotkey.id(), Ordering::SeqCst);
                REGISTERED.with(|r| *r.borrow_mut() = Some((Some(hotkey), chord.to_string())));
                log::info!("hotkey: registered {chord}");
                Ok(Binding::Registered)
            }
            Err(refused @ AppError::HotkeyInUse { .. }) => hook(chord, refused),
            Err(other) => Err(other),
        }
    }

    /// Listens for a chord Windows refused through the keyboard hook; on failure the original
    /// refusal is returned so the UI explains the real problem.
    fn hook(chord: &str, refused: AppError) -> Result<Binding, AppError> {
        let Some(spec) = ChordSpec::parse(chord) else { return Err(refused) };
        if let Err(e) = keyhook::start(spec) {
            log::error!("hotkey: {chord} refused and the keyboard hook failed: {e}");
            return Err(refused);
        }
        REGISTERED.with(|r| *r.borrow_mut() = Some((None, chord.to_string())));
        log::info!("hotkey: Windows refused {chord}; listening through the keyboard hook");
        Ok(Binding::Hook)
    }

    /// How `chord` is heard if it is already the active one.
    fn current_binding(chord: &str) -> Option<Binding> {
        REGISTERED.with(|r| match r.borrow().as_ref() {
            Some((hotkey, active)) if active == chord => {
                Some(if hotkey.is_some() { Binding::Registered } else { Binding::Hook })
            }
            _ => None,
        })
    }

    /// Unregisters the toggle hotkey, if any. Main thread only.
    pub fn unregister() {
        let Some((hotkey, chord)) = REGISTERED.with(|r| r.borrow_mut().take()) else { return };
        TOGGLE_ID.store(0, Ordering::SeqCst);
        let Some(hotkey) = hotkey else {
            keyhook::stop();
            log::info!("hotkey: stopped listening for {chord}");
            return;
        };
        MANAGER.with(|m| {
            if let Some(manager) = m.borrow().as_ref() {
                if let Err(e) = manager.unregister(hotkey) {
                    log::warn!("hotkey: unregister {chord} failed: {e}");
                }
            }
        });
        log::info!("hotkey: unregistered {chord}");
    }

    fn classify(chord: &str, error: global_hotkey::Error) -> AppError {
        match error {
            global_hotkey::Error::AlreadyRegistered(_) => {
                AppError::HotkeyInUse { chord: chord.to_string() }
            }
            global_hotkey::Error::OsError(e)
                if e.raw_os_error() == Some(ERROR_HOTKEY_ALREADY_REGISTERED) =>
            {
                AppError::HotkeyInUse { chord: chord.to_string() }
            }
            global_hotkey::Error::OsError(e) => {
                AppError::win32("RegisterHotKey", e.raw_os_error().unwrap_or(0) as u32)
            }
            other => {
                AppError::HotkeyInvalid { chord: chord.to_string(), reason: other.to_string() }
            }
        }
    }
}

/// Non-Windows builds have no global hotkey; registration always "succeeds".
#[cfg(not(windows))]
mod stub {
    use super::{AppError, Binding};
    use crate::engine::Msg;
    use std::sync::mpsc::Sender;

    pub fn install_handler(_tx: Sender<Msg>) {}
    pub fn create_manager() -> Result<(), AppError> {
        Ok(())
    }
    pub fn register(_chord: &str) -> Result<Binding, AppError> {
        Ok(Binding::Registered)
    }
    pub fn unregister() {}
    pub fn registered_chord() -> Option<String> {
        None
    }
}

#[cfg(not(windows))]
pub use stub::{create_manager, install_handler, register, registered_chord, unregister};

/// Registers `new`; if that fails, registers `previous` again (when given) so a chord stays
/// live. The error carries how `previous` is heard now, or `None` when it could not be restored.
/// Main thread only.
pub fn register_or_restore(
    new: &str,
    previous: Option<&str>,
) -> Result<Binding, (AppError, Option<Binding>)> {
    register(new).map_err(|error| {
        let restored = previous.and_then(|old| {
            register(old).map_err(|e| log::error!("hotkey: could not re-register {old}: {e}")).ok()
        });
        (error, restored)
    })
}

/// Swaps the live chord for `new`, keeping the old one if `new` fails. Identical chords are a
/// no-op. Main thread only.
pub fn rebind(new: &str) -> Result<Binding, (AppError, Option<Binding>)> {
    let old = registered_chord();
    register_or_restore(new, old.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(chord: &str) -> String {
        normalize(chord).unwrap_or_else(|e| panic!("{chord}: {e:?}"))
    }

    #[test]
    fn canonical_form() {
        assert_eq!(ok("Ctrl+Alt+B"), "Ctrl+Alt+B");
        assert_eq!(ok(" alt + control + keyb "), "Ctrl+Alt+B");
        assert_eq!(ok("Shift+Ctrl+Digit1"), "Ctrl+Shift+1");
        assert_eq!(ok("f12"), "F12");
        assert_eq!(ok("Pause"), "Pause");
        assert_eq!(ok("scrolllock"), "ScrollLock");
        assert_eq!(ok("Ctrl+Alt+PgUp"), "Ctrl+Alt+PageUp");
        assert_eq!(ok("Alt+Backquote"), "Alt+Backquote");
        // AutoHotkey names, as found in an imported borderless_config.ini.
        assert_eq!(ok("Ctrl+NumpadDot"), "Ctrl+NumpadDecimal");
        assert_eq!(ok("Alt+BS"), "Alt+Backspace");
    }

    #[test]
    fn meta_becomes_super() {
        assert_eq!(ok("Meta+Shift+F"), "Shift+Super+F");
        assert_eq!(ok("Win+Numpad5"), "Super+Numpad5");
    }

    #[test]
    fn typing_keys_need_a_real_modifier() {
        assert!(normalize("B").is_err());
        assert!(normalize("Shift+B").is_err());
        assert!(normalize("Space").is_err());
        assert_eq!(ok("Shift+F5"), "Shift+F5");
    }

    #[test]
    fn rejects_unusable_chords() {
        for bad in [
            "ContextMenu",
            "Ctrl+IntlBackslash",
            "Ctrl+Alt",
            "Ctrl+ShiftLeft",
            "Ctrl+Pause",
            "Ctrl+Alt+Pause",
            "Ctrl+A+B",
            "Ctrl++",
            "",
            "Alt+F4",
            "Win+L",
            "Ctrl+Alt+Del",
            "Ctrl+MediaPlayPause",
        ] {
            assert!(normalize(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn every_allowed_key_normalizes_to_itself() {
        for key in ALLOWED_KEYS {
            let chord = format!("Alt+Super+{key}");
            assert_eq!(ok(&chord), chord);
        }
    }

    /// The allow-list only contains names global-hotkey 0.8.0 parses (Windows-only dependency).
    #[cfg(windows)]
    #[test]
    fn every_allowed_key_parses_in_global_hotkey() {
        use global_hotkey::hotkey::HotKey;
        use std::str::FromStr;
        for key in ALLOWED_KEYS {
            let chord = format!("Ctrl+{key}");
            assert!(HotKey::from_str(&chord).is_ok(), "{chord}");
        }
        assert!(HotKey::from_str("Ctrl+Alt+Shift+Super+F24").is_ok());
        assert!(HotKey::from_str("Ctrl+ContextMenu").is_err());
    }
}
