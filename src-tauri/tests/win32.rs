//! Windows integration tests against the real Win32 layer.
//!
//! Safe tests always run (`pnpm test:win`, windows-latest CI). Tests that move real windows,
//! hide the taskbar or touch HKCU are `#[ignore]`d; run them with `-- --ignored` on a desktop you
//! are not using at that moment.
#![cfg(windows)]

use borderfit_lib::engine::{Engine, EventSink, FitSource, HookControl, Msg, NoHooks};
use borderfit_lib::hooks::HookThread;
use borderfit_lib::keyhook::{self, ChordSpec};
use borderfit_lib::model::{
    AppError, FitState, FitStatus, HAlign, Notice, Preset, Rect, RegionSpec, RestoreMarker,
    Settings,
};
use borderfit_lib::recovery::{FileMarkerStore, MarkerStore};
use borderfit_lib::win32::windows::{self as win, Win32WindowManager};
use borderfit_lib::win32::{WindowManager, WS_CAPTION, WS_THICKFRAME};
use borderfit_lib::{hotkey, taskbar};
use std::process::{Child, Command};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The keyboard hook is process-wide, so tests that start it take turns.
static KEYBOARD_HOOK: Mutex<()> = Mutex::new(());

/// A fresh folder in the system temp directory, deleted again when dropped.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("borderfit-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn marker_store(&self) -> FileMarkerStore {
        FileMarkerStore::new(self.0.join("restore-state.json"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Polls `check` until it returns true or `timeout` passes.
fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    check()
}

// ---------------------------------------------------------------------------------------------
// Always-on tests
// ---------------------------------------------------------------------------------------------

#[test]
fn monitors_have_exactly_one_primary_and_display_names() {
    let monitors = Win32WindowManager::new().monitors();
    assert!(!monitors.is_empty());
    assert_eq!(monitors.iter().filter(|m| m.primary).count(), 1, "{monitors:?}");
    for m in &monitors {
        // A disconnected session reports one virtual "WinDisc" display instead.
        assert!(m.device.starts_with(r"\\.\DISPLAY") || m.device == "WinDisc", "{m:?}");
        assert!(m.rect.w > 0 && m.rect.h > 0);
        assert!(m.dpi >= 96);
    }
}

#[test]
fn tray_discovery_returns_only_explorer_owned_trays() {
    for tray in Win32WindowManager::new().tray_windows() {
        assert!(tray.exe.eq_ignore_ascii_case("explorer.exe"), "{tray:?}");
        assert!(tray.class == "Shell_TrayWnd" || tray.class == "Shell_SecondaryTrayWnd");
    }
}

#[test]
fn elevation_and_integrity_level_agree() {
    let wm = Win32WindowManager::new();
    let level = wm.integrity_level(wm.own_pid()).expect("own integrity level");
    assert_eq!(win::is_elevated(), level >= 0x3000, "level {level:#x}");
}

#[test]
fn os_build_is_readable() {
    let build = win::os_build();
    assert!(build.chars().any(|c| c.is_ascii_digit()), "{build}");
}

#[test]
fn autostart_command_quotes_the_path() {
    let path = std::path::Path::new(r"C:\Users\Some One\AppData\Local\BorderFit\BorderFit.exe");
    assert_eq!(
        win::autostart_command(path),
        r#""C:\Users\Some One\AppData\Local\BorderFit\BorderFit.exe" --minimized"#
    );
}

#[test]
fn marker_is_written_read_and_deleted_on_disk() {
    let dir = TempDir::new("marker");
    let store = dir.marker_store();
    assert!(store.read().is_none());
    let marker = RestoreMarker { schema_version: 1, pid: 42, ..RestoreMarker::default() };
    store.write(&marker).unwrap();
    assert_eq!(store.read(), Some(marker));
    store.delete();
    assert!(!store.present());
}

#[test]
fn process_handles_track_our_own_process() {
    let wm = Win32WindowManager::new();
    let handle = wm.open_process(wm.own_pid()).unwrap();
    assert!(!wm.process_exited(&handle));
    assert!(wm.process_alive(wm.own_pid(), handle.created));
    assert!(!wm.process_alive(wm.own_pid(), handle.created + 1));
    wm.close_process(&handle);
}

#[test]
fn a_chord_held_elsewhere_falls_back_to_the_keyboard_hook() {
    let _turn = KEYBOARD_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let chord = "Ctrl+Alt+Shift+F23";
    // Each thread gets its own hidden manager window, like two apps would.
    let holder = std::thread::spawn(move || {
        hotkey::create_manager().unwrap();
        hotkey::register(chord).unwrap();
        let other = std::thread::spawn(move || {
            hotkey::create_manager().unwrap();
            let binding = hotkey::register(chord);
            hotkey::unregister();
            binding
        })
        .join()
        .unwrap();
        hotkey::unregister();
        other
    })
    .join()
    .unwrap();
    assert_eq!(holder, Ok(hotkey::Binding::Hook));
}

/// Injects Ctrl+Alt+Shift+F23 (harmless if it leaks) and expects the hook to report a press.
#[test]
fn the_keyboard_hook_hears_its_chord() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_F23, VK_MENU, VK_SHIFT,
    };
    let _turn = KEYBOARD_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tx, rx) = std::sync::mpsc::channel();
    keyhook::set_sender(tx);
    keyhook::start(ChordSpec::parse("Ctrl+Alt+Shift+F23").unwrap()).unwrap();

    let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
    };
    let up = KEYEVENTF_KEYUP;
    let down = KEYBD_EVENT_FLAGS(0);
    let inputs = [
        key(VK_CONTROL, down),
        key(VK_MENU, down),
        key(VK_SHIFT, down),
        key(VK_F23, down),
        key(VK_F23, up),
        key(VK_SHIFT, up),
        key(VK_MENU, up),
        key(VK_CONTROL, up),
    ];
    // SAFETY: the slice and the size describe valid INPUT structs.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    let heard = rx.recv_timeout(Duration::from_secs(2));
    keyhook::stop();
    if sent == 0 {
        eprintln!("SendInput blocked (no interactive desktop); skipping");
        return;
    }
    assert!(matches!(heard, Ok(Msg::Toggle { src: FitSource::Hotkey })));
}

/// Registering a second chord releases the first, so Windows never keeps a stale chord reserved.
#[test]
fn registering_a_new_chord_releases_the_old_one() {
    let _turn = KEYBOARD_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (first, second) = ("Ctrl+Alt+Shift+F22", "Ctrl+Alt+Shift+F21");
    let other_app = std::thread::spawn(move || {
        hotkey::create_manager().unwrap();
        hotkey::register(first).unwrap();
        hotkey::register(second).unwrap();
        let other = std::thread::spawn(move || {
            hotkey::create_manager().unwrap();
            let binding = hotkey::register(first);
            hotkey::unregister();
            binding
        })
        .join()
        .unwrap();
        hotkey::unregister();
        other
    })
    .join()
    .unwrap();
    // Registered, not Hook: Windows let the other app have the released chord.
    assert_eq!(other_app, Ok(hotkey::Binding::Registered));
}

#[test]
fn every_allowed_key_registers() {
    let _turn = KEYBOARD_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::thread::spawn(|| {
        hotkey::create_manager().unwrap();
        for key in hotkey::ALLOWED_KEYS {
            let chord = format!("Ctrl+Alt+Shift+Super+{key}");
            match hotkey::register(&chord) {
                Ok(_) => {}
                Err(e) => panic!("{chord}: {e:?}"),
            }
            hotkey::unregister();
        }
    })
    .join()
    .unwrap();
}

// ---------------------------------------------------------------------------------------------
// Desktop tests (ignored by default; `-- --ignored`)
// ---------------------------------------------------------------------------------------------

/// A WinForms window in a separate PowerShell process: a foreign window like a game's.
struct HostWindow {
    child: Child,
    hwnd: isize,
}

impl HostWindow {
    fn start(title: &str, state: &str) -> Self {
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             $f = New-Object System.Windows.Forms.Form; $f.Text = '{title}'; \
             $f.StartPosition = 'Manual'; $f.Location = New-Object System.Drawing.Point(220, 180); \
             $f.Size = New-Object System.Drawing.Size(900, 600); $f.WindowState = '{state}'; \
             [System.Windows.Forms.Application]::Run($f)"
        );
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .spawn()
            .expect("powershell");
        let wm = Win32WindowManager::new();
        let mut hwnd = 0;
        let found = wait_until(Duration::from_secs(20), || {
            hwnd = wm
                .top_level_windows()
                .into_iter()
                .find(|&h| wm.info(h).is_ok_and(|i| i.title == title))
                .unwrap_or(0);
            hwnd != 0
        });
        assert!(found, "host window {title} did not appear");
        eprintln!("host window \"{title}\" is {hwnd:#x}");
        Self { child, hwnd }
    }
}

impl Drop for HostWindow {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Default)]
struct Collect {
    statuses: Arc<Mutex<Vec<FitStatus>>>,
    failures: Arc<Mutex<Vec<AppError>>>,
}

impl EventSink for Collect {
    fn status(&self, status: &FitStatus) {
        self.statuses.lock().unwrap().push(status.clone());
    }
    fn notice(&self, _notice: &Notice) {}
    fn fit_failed(&self, error: &AppError, _src: FitSource) {
        self.failures.lock().unwrap().push(error.clone());
    }
}

fn engine_for_test(dir: &TempDir, hide_taskbar: bool) -> (Engine, Collect) {
    engine_with_hooks(dir, hide_taskbar, Box::new(NoHooks))
}

fn engine_with_hooks(
    dir: &TempDir,
    hide_taskbar: bool,
    hooks: Box<dyn HookControl>,
) -> (Engine, Collect) {
    let mut settings = Settings::default();
    settings.profile.taskbar.hide_taskbar = hide_taskbar;
    settings.profile.region = RegionSpec {
        preset: Preset::Fixed { w: 800, h: 600 },
        halign: HAlign::Left,
        ..RegionSpec::default()
    };
    let sink = Collect::default();
    let engine = Engine::new(
        Box::new(Win32WindowManager::new()),
        Box::new(sink.clone()),
        Box::new(dir.marker_store()),
        hooks,
        Box::leak(Box::new(AtomicBool::new(false))),
        settings,
    );
    (engine, sink)
}

fn expected_target(wm: &Win32WindowManager) -> Rect {
    let primary = wm.monitors().into_iter().find(|m| m.primary).unwrap();
    Rect::new(primary.rect.x, primary.rect.y + (primary.rect.h - 600) / 2, 800, 600)
}

fn fit_restore_round_trip(state: &str) {
    let title = format!("BorderFit test {state} {}", std::process::id());
    let host = HostWindow::start(&title, state);
    let wm = Win32WindowManager::new();
    wait_until(Duration::from_secs(2), || wm.is_visible(host.hwnd));
    let before_rect = wm.rect(host.hwnd).unwrap();
    let before_placement = wm.placement(host.hwnd).unwrap();
    let (before_style, before_ex) = wm.styles(host.hwnd).unwrap();
    let target = expected_target(&wm);

    let dir = TempDir::new(&format!("fit-{state}"));
    let (mut engine, sink) = engine_for_test(&dir, false);
    engine.handle(Msg::Fit { hwnd: Some(host.hwnd), src: FitSource::Ui });
    assert!(sink.failures.lock().unwrap().is_empty(), "{:?}", sink.failures.lock().unwrap());
    assert!(engine.is_fitted());
    assert!(
        wait_until(Duration::from_secs(2), || wm.rect(host.hwnd).ok() == Some(target)),
        "fitted rect {:?}, want {target:?}",
        wm.rect(host.hwnd)
    );
    let (style, _) = wm.styles(host.hwnd).unwrap();
    assert_eq!(style & (WS_CAPTION | WS_THICKFRAME), 0, "borders cleared: {style:#x}");

    engine.handle(Msg::Restore { done: None });
    assert_eq!(sink.statuses.lock().unwrap().last().map(|s| s.state), Some(FitState::Idle));
    let restored_placement = || wm.placement(host.hwnd).map(|p| p.show_cmd).ok();
    assert!(
        wait_until(Duration::from_secs(2), || {
            wm.styles(host.hwnd).ok() == Some((before_style, before_ex))
                && restored_placement() == Some(before_placement.show_cmd)
        }),
        "styles {:?} vs {:?}",
        wm.styles(host.hwnd),
        (before_style, before_ex)
    );
    if state == "Normal" {
        assert!(
            wait_until(Duration::from_secs(2), || wm.rect(host.hwnd).ok() == Some(before_rect)),
            "restored rect {:?}, want {before_rect:?}",
            wm.rect(host.hwnd)
        );
    } else {
        assert_eq!(wm.placement(host.hwnd).unwrap().normal, before_placement.normal);
    }
}

#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_fit_restore_normal_window() {
    fit_restore_round_trip("Normal");
}

#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_fit_restore_maximized_window() {
    fit_restore_round_trip("Maximized");
}

#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_closing_the_game_releases_it() {
    let title = format!("BorderFit test exit {}", std::process::id());
    let mut host = HostWindow::start(&title, "Normal");
    let dir = TempDir::new("exit");
    let (mut engine, sink) = engine_for_test(&dir, false);
    engine.handle(Msg::Fit { hwnd: Some(host.hwnd), src: FitSource::Ui });
    assert!(engine.is_fitted(), "{:?}", sink.failures.lock().unwrap());
    host.child.kill().unwrap();
    host.child.wait().unwrap();
    assert!(wait_until(Duration::from_secs(3), || {
        engine.tick();
        !engine.is_fitted()
    }));
}

/// A fitted window moved from outside (what Chromium does to a fullscreen browser when another
/// window is activated) is put back by the LOCATIONCHANGE hook alone, without any tick.
#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_a_moved_window_is_put_back_by_the_hook() {
    let title = format!("BorderFit test move {}", std::process::id());
    let host = HostWindow::start(&title, "Normal");
    let wm = Win32WindowManager::new();
    let target = expected_target(&wm);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut hooks = HookThread::spawn(tx).unwrap();
    let dir = TempDir::new("move");
    let (mut engine, sink) = engine_with_hooks(&dir, false, Box::new(hooks.control()));
    engine.handle(Msg::Fit { hwnd: Some(host.hwnd), src: FitSource::Ui });
    assert!(engine.is_fitted(), "{:?}", sink.failures.lock().unwrap());
    let pump_until = |engine: &mut Engine, check: &dyn Fn() -> bool| {
        wait_until(Duration::from_secs(3), || {
            while let Ok(msg) = rx.try_recv() {
                engine.handle(msg);
            }
            check()
        })
    };
    assert!(pump_until(&mut engine, &|| wm.rect(host.hwnd).ok() == Some(target)));

    let elsewhere = Rect::new(target.x + 40, target.y + 30, 700, 500);
    wm.set_pos(host.hwnd, elsewhere, borderfit_lib::win32::ZOrder::Keep, false).unwrap();
    assert!(
        pump_until(&mut engine, &|| wm.rect(host.hwnd).ok() == Some(target)),
        "still at {:?}, want {target:?}",
        wm.rect(host.hwnd)
    );

    engine.handle(Msg::Restore { done: None });
    hooks.stop();
}

/// Hides the primary taskbar for a moment and checks the work area never changes.
#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_taskbar_hide_and_restore_keep_the_work_area() {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let work_area = || {
        let mut rect = RECT::default();
        // SAFETY: SPI_GETWORKAREA writes one RECT.
        unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut rect as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .unwrap();
        (rect.left, rect.top, rect.right, rect.bottom)
    };
    let wm = Win32WindowManager::new();
    let primary_visible = || wm.tray_windows().iter().any(|t| t.is_primary() && t.visible);
    if !primary_visible() {
        eprintln!("skipped: no visible explorer taskbar");
        return;
    }
    let before = work_area();
    taskbar::reconcile(&wm, true, false);
    let hidden = wait_until(Duration::from_secs(1), || !primary_visible());
    let during = work_area();
    let failed = taskbar::restore_all(&wm);
    assert!(failed.is_empty(), "taskbar did not come back: {failed:?}");
    assert!(wait_until(Duration::from_secs(1), primary_visible));
    assert!(hidden, "SW_HIDE did not hide the taskbar");
    assert_eq!(before, during, "the work area must not change");
    assert_eq!(before, work_area());
}

#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_autostart_value_round_trips() {
    if win::autostart_value().is_some() {
        eprintln!("skipped: a real BorderFit autostart value exists");
        return;
    }
    win::set_autostart(true).unwrap();
    let value = win::autostart_value().unwrap();
    let exe = std::env::current_exe().unwrap();
    assert_eq!(value, win::autostart_command(&exe));
    win::set_autostart(false).unwrap();
    assert!(win::autostart_value().is_none());
    win::set_autostart(false).unwrap();
}

#[test]
#[ignore = "moves real windows, hides the taskbar or writes HKCU: run with --ignored on an idle desktop"]
fn desktop_instance_event_is_visible_until_closed() {
    if win::other_instance_running() {
        eprintln!("skipped: BorderFit is running");
        return;
    }
    win::create_instance_event();
    assert!(win::other_instance_running());
    win::close_instance_event();
    assert!(!win::other_instance_running());
}
