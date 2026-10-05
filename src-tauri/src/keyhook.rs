//! Fallback for hotkeys Windows refuses to `RegisterHotKey` (F12 and Shift+F12 are reserved for
//! the debugger; other chords may be held by another app): a low-level keyboard hook
//! (`WH_KEYBOARD_LL`) that watches for the one chord and swallows it, like AutoHotkey does.
//!
//! The hook runs in BorderFit's own process (nothing is injected into the game) on a dedicated
//! thread that pumps messages, so a busy UI thread can never make Windows drop the hook. It is
//! only installed while such a chord is the hotkey. Chord-to-key mapping is pure and tested
//! everywhere; the hook itself is Windows-only.

/// A normalized chord as the hook matches it: exact modifiers plus one virtual key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordSpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u8,
}

// The packing is read only by the Windows hook (and the tests).
#[cfg_attr(not(windows), allow(dead_code))]
impl ChordSpec {
    /// Parses a chord already normalized by `hotkey::normalize` (`Ctrl+Alt+Shift+Super+Key`).
    pub fn parse(chord: &str) -> Option<Self> {
        let mut spec = Self { ctrl: false, alt: false, shift: false, win: false, vk: 0 };
        for token in chord.split('+') {
            match token {
                "Ctrl" => spec.ctrl = true,
                "Alt" => spec.alt = true,
                "Shift" => spec.shift = true,
                "Super" => spec.win = true,
                key => spec.vk = virtual_key(key)?,
            }
        }
        (spec.vk != 0).then_some(spec)
    }

    /// Modifier bits in the order ctrl, alt, shift, win (bit 0..3).
    fn modifier_bits(self) -> u32 {
        u32::from(self.ctrl)
            | u32::from(self.alt) << 1
            | u32::from(self.shift) << 2
            | u32::from(self.win) << 3
    }

    /// Packs into one non-zero word so the hook callback can read it from an atomic.
    fn pack(self) -> u32 {
        u32::from(self.vk) | self.modifier_bits() << 8
    }
}

/// Windows virtual-key code for a canonical key name from `hotkey::ALLOWED_KEYS`.
pub fn virtual_key(key: &str) -> Option<u8> {
    let bytes = key.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
        return Some(bytes[0]);
    }
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then(|| 0x6F + n);
    }
    if let Some(n) = key.strip_prefix("Numpad").and_then(|n| n.parse::<u8>().ok()) {
        return (n <= 9).then(|| 0x60 + n);
    }
    let vk = match key {
        "Pause" => 0x13,
        "ScrollLock" => 0x91,
        "PrintScreen" => 0x2C,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowUp" => 0x26,
        "ArrowDown" => 0x28,
        "ArrowLeft" => 0x25,
        "ArrowRight" => 0x27,
        "Backspace" => 0x08,
        "Tab" => 0x09,
        "Enter" => 0x0D,
        "Space" => 0x20,
        "Escape" => 0x1B,
        "CapsLock" => 0x14,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadMultiply" => 0x6A,
        "NumpadDivide" => 0x6F,
        "NumpadDecimal" => 0x6E,
        "Backquote" => 0xC0,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Backslash" => 0xDC,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        _ => return None,
    };
    Some(vk)
}

#[cfg(windows)]
pub use platform::{set_sender, start, stop};

#[cfg(windows)]
mod platform {
    use super::ChordSpec;
    use crate::engine::{FitSource, Msg};
    use crate::model::AppError;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc::{sync_channel, Sender};
    use std::sync::{Mutex, OnceLock};
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
        SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
    };

    /// Unassigned virtual key sent after a swallowed Alt/Win chord, so releasing Alt doesn't
    /// open the focused app's menu bar and releasing Win doesn't open Start.
    const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

    /// The packed chord the callback watches for (0 = none).
    static WATCHED: AtomicU32 = AtomicU32::new(0);
    /// True between a swallowed key-down and its key-up (auto-repeats are swallowed too).
    static HELD: AtomicBool = AtomicBool::new(false);
    static SENDER: OnceLock<Sender<Msg>> = OnceLock::new();
    static THREAD: Mutex<Option<(u32, JoinHandle<()>)>> = Mutex::new(None);

    /// Where a press is reported. Called once, with the hotkey handler.
    pub fn set_sender(tx: Sender<Msg>) {
        let _ = SENDER.set(tx);
    }

    /// Replaces any running hook with one for `spec`; waits until it is installed.
    pub fn start(spec: ChordSpec) -> Result<(), AppError> {
        stop();
        WATCHED.store(spec.pack(), Ordering::SeqCst);
        HELD.store(false, Ordering::SeqCst);
        let (ready_tx, ready_rx) = sync_channel::<Result<u32, u32>>(1);
        let join = std::thread::Builder::new()
            .name("keyboard-hook".into())
            .spawn(move || hook_thread(&ready_tx))
            .map_err(|_| AppError::win32("keyboard hook thread", 0))?;
        match ready_rx.recv().unwrap_or(Err(0)) {
            Ok(tid) => {
                *THREAD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((tid, join));
                Ok(())
            }
            Err(code) => {
                WATCHED.store(0, Ordering::SeqCst);
                let _ = join.join();
                Err(AppError::win32("SetWindowsHookExW", code))
            }
        }
    }

    /// Removes the hook, if any, and waits for its thread.
    pub fn stop() {
        WATCHED.store(0, Ordering::SeqCst);
        let Some((tid, join)) =
            THREAD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
        else {
            return;
        };
        // SAFETY: posts a plain thread message; no pointers are passed.
        if let Err(e) = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) } {
            log::warn!("keyhook: PostThreadMessageW(WM_QUIT) failed: {e}");
            return;
        }
        let _ = join.join();
    }

    /// Installs the hook, reports the outcome, pumps messages until WM_QUIT, then unhooks.
    fn hook_thread(ready: &std::sync::mpsc::SyncSender<Result<u32, u32>>) {
        let mut msg = MSG::default();
        // SAFETY: creates this thread's message queue, installs the hook here and removes it
        // on the same thread after the standard message loop ends.
        unsafe {
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let module = GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).ok();
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) {
                Ok(hook) => hook,
                Err(e) => {
                    log::error!("keyhook: SetWindowsHookExW failed: {e}");
                    let _ = ready.send(Err(e.code().0 as u32));
                    return;
                }
            };
            let _ = ready.send(Ok(GetCurrentThreadId()));
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            if let Err(e) = UnhookWindowsHookEx(hook) {
                log::warn!("keyhook: UnhookWindowsHookEx failed: {e}");
            }
        }
    }

    /// Must return fast: Windows skips (and eventually removes) hooks that stall input.
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT for this event.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            if should_swallow(info.vkCode, down) {
                return LRESULT(1);
            }
        }
        // SAFETY: passes the event on unchanged.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// Decides one key event; reports the press (once per physical press) when it matches.
    fn should_swallow(vk: u32, down: bool) -> bool {
        let watched = WATCHED.load(Ordering::SeqCst);
        if watched == 0 || vk != watched & 0xFF {
            return false;
        }
        if !down {
            // Swallow the release only if the press was ours.
            return HELD.swap(false, Ordering::SeqCst);
        }
        if HELD.load(Ordering::SeqCst) {
            return true; // auto-repeat of a press already handled
        }
        let wanted = watched >> 8;
        if held_modifier_bits() != wanted {
            return false;
        }
        HELD.store(true, Ordering::SeqCst);
        if wanted & 0b1010 != 0 {
            send_mask_key();
        }
        if let Some(tx) = SENDER.get() {
            let _ = tx.send(Msg::Toggle { src: FitSource::Hotkey });
        }
        true
    }

    /// Ctrl, Alt, Shift, Win currently held, in `ChordSpec::modifier_bits` order.
    fn held_modifier_bits() -> u32 {
        // SAFETY: GetAsyncKeyState only reads key state.
        let held = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(i32::from(vk.0)) } < 0;
        u32::from(held(VK_CONTROL))
            | u32::from(held(VK_MENU)) << 1
            | u32::from(held(VK_SHIFT)) << 2
            | u32::from(held(VK_LWIN) || held(VK_RWIN)) << 3
    }

    fn send_mask_key() {
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: MASK_KEY, dwFlags: flags, ..Default::default() },
            },
        };
        let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
        // SAFETY: the slice and the size describe valid INPUT structs.
        unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::ALLOWED_KEYS;

    #[test]
    fn every_allowed_key_has_a_virtual_key() {
        for key in ALLOWED_KEYS {
            assert!(virtual_key(key).is_some(), "{key}");
        }
    }

    #[test]
    fn known_virtual_keys() {
        assert_eq!(virtual_key("F1"), Some(0x70));
        assert_eq!(virtual_key("F12"), Some(0x7B));
        assert_eq!(virtual_key("F24"), Some(0x87));
        assert_eq!(virtual_key("B"), Some(0x42));
        assert_eq!(virtual_key("7"), Some(0x37));
        assert_eq!(virtual_key("Numpad5"), Some(0x65));
        assert_eq!(virtual_key("F25"), None);
        assert_eq!(virtual_key("ContextMenu"), None);
    }

    #[test]
    fn parses_normalized_chords() {
        let f12 = ChordSpec::parse("F12").unwrap();
        assert_eq!(f12, ChordSpec { ctrl: false, alt: false, shift: false, win: false, vk: 0x7B });
        let chord = ChordSpec::parse("Ctrl+Alt+Shift+Super+B").unwrap();
        assert_eq!(chord.modifier_bits(), 0b1111);
        assert_eq!(chord.pack(), 0x42 | 0b1111 << 8);
        assert!(ChordSpec::parse("Ctrl+Alt").is_none());
        assert!(ChordSpec::parse("Ctrl+Bogus").is_none());
    }
}
