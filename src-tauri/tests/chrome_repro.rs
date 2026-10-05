//! Opt-in reproduction of "clicking a fitted fullscreen Chrome window misbehaves": real Chrome,
//! the real engine and hook thread, real mouse input. Prints every foreground change, move of the
//! Chrome window and page mouse event with millisecond timestamps.
//!
//! Findings (2026-10-02): every mouse press makes Windows re-order Chrome's window; Chromium's
//! `HWNDMessageHandler::OnWindowPosChanging` rewrites any position change of a fullscreen window
//! to the monitor rect, so the press is laid out at monitor size before BorderFit can put the
//! window back. `BF_SETTLE_MS` sets the wait between fit and clicks.
//!
//! Run from WSL on a desktop nobody is using (it takes over the mouse):
//! `scripts/xwin.sh test --test chrome_repro -- --ignored --nocapture`
#![cfg(windows)]

use borderfit_lib::engine::{Engine, EventSink, FitSource, Msg};
use borderfit_lib::hooks::HookThread;
use borderfit_lib::model::{AppError, FitStatus, Notice, Preset, Rect, RegionSpec, Settings};
use borderfit_lib::recovery::FileMarkerStore;
use borderfit_lib::win32::windows::Win32WindowManager;
use borderfit_lib::win32::WindowManager;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
    MOUSE_EVENT_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, SetCursorPos, MSG, WINEVENT_OUTOFCONTEXT,
};

const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
const EVENT_OBJECT_NAMECHANGE: u32 = 0x800C;

/// The page: a bottom-right button (like YouTube's) that exits fullscreen; any click enters it.
/// Mouse events land in the title so the test can watch them from outside.
const PAGE: &str = r#"<!doctype html><html><head><title>BFREPRO ready</title><style>
html,body{margin:0;height:100%;background:#203040;color:#eee;font:20px sans-serif}
#b{position:fixed;right:20px;bottom:20px;width:120px;height:60px}</style></head>
<body><button id="b">FS</button><script>
let n=0;function note(s){n++;document.title='BFREPRO '+n+' '+s;}
['mousedown','mouseup','click'].forEach(e=>addEventListener(e,ev=>note(e+':'+(ev.target.id||ev.target.tagName)+' x'+ev.clientX),true));
addEventListener('resize',()=>note('resize:'+innerWidth+'x'+innerHeight));
document.addEventListener('fullscreenchange',()=>note('fullscreen:'+!!document.fullscreenElement));
addEventListener('click',e=>{if(!document.fullscreenElement)document.documentElement.requestFullscreen();else if(e.target.id==='b')document.exitFullscreen();});
</script></body></html>"#;

static T0: OnceLock<Instant> = OnceLock::new();
static CHROME_HWND: AtomicIsize = AtomicIsize::new(0);

fn ms() -> u128 {
    T0.get_or_init(Instant::now).elapsed().as_millis()
}

struct Quiet;
impl EventSink for Quiet {
    fn status(&self, _status: &FitStatus) {}
    fn notice(&self, _notice: &Notice) {}
    fn fit_failed(&self, error: &AppError, _src: FitSource) {
        eprintln!("{:>6} fit failed: {error:?}", ms());
    }
}

/// Prints one observed event. Runs on the observer thread.
unsafe extern "system" fn observe(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != 0 || id_child != 0 {
        return;
    }
    let raw = hwnd.0 as isize;
    let wm = Win32WindowManager::new();
    let chrome = CHROME_HWND.load(Ordering::SeqCst);
    match event {
        EVENT_SYSTEM_FOREGROUND => {
            let info =
                wm.info(raw).map(|i| format!("{} pid {}", i.class, i.pid)).unwrap_or_default();
            eprintln!("{:>6} FOREGROUND {raw:#x} {info}", ms());
        }
        EVENT_OBJECT_LOCATIONCHANGE if raw == chrome => {
            let rect = wm.rect(raw).map(|r| r.to_string()).unwrap_or_default();
            let style = wm.styles(raw).map(|(s, _)| s).unwrap_or(0);
            eprintln!("{:>6} MOVE {rect} style {style:#x}", ms());
        }
        EVENT_OBJECT_NAMECHANGE if raw == chrome => {
            let title = wm.info(raw).map(|i| i.title).unwrap_or_default();
            eprintln!("{:>6} PAGE {}", ms(), title.trim_end_matches(" - Google Chrome"));
        }
        _ => {}
    }
}

/// Global foreground hook plus Chrome-scoped move/name hooks, on a thread that pumps messages.
fn start_observer(pid: u32) {
    std::thread::spawn(move || {
        let flags = WINEVENT_OUTOFCONTEXT;
        // SAFETY: static callback; hooks live as long as this thread.
        unsafe {
            let _ = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(observe),
                0,
                0,
                flags,
            );
            let _ = SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_NAMECHANGE,
                None,
                Some(observe),
                pid,
                0,
                flags,
            );
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        }
    });
}

fn mouse(flags: MOUSE_EVENT_FLAGS) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: flags, ..Default::default() } },
    };
    // SAFETY: one valid INPUT.
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

/// Moves the cursor to (x, y) and clicks, holding the button for `hold`.
fn click(x: i32, y: i32, hold: Duration, label: &str) {
    // SAFETY: plain cursor move.
    unsafe {
        let _ = SetCursorPos(x, y);
    }
    std::thread::sleep(Duration::from_millis(300));
    eprintln!("{:>6} ---- {label}: down at {x},{y}", ms());
    mouse(MOUSEEVENTF_LEFTDOWN);
    std::thread::sleep(hold);
    eprintln!("{:>6} ---- {label}: up", ms());
    mouse(MOUSEEVENTF_LEFTUP);
}

fn wait_for(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn launch_chrome(dir: &std::path::Path) -> Child {
    let page = dir.join("fs-test.html");
    std::fs::write(&page, PAGE).unwrap();
    Command::new(CHROME)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .args(["--no-first-run", "--no-default-browser-check", "--new-window"])
        .args(["--window-position=100,100", "--window-size=1200,800"])
        .arg(format!("file:///{}", page.display().to_string().replace('\\', "/")))
        .spawn()
        .expect("chrome")
}

/// Kills the launched Chrome tree even when an assertion fails (a live Chrome holds the test's
/// output pipe open, so WSL would wait forever).
struct ChromeGuard(Child);

impl Drop for ChromeGuard {
    fn drop(&mut self) {
        let pid = self.0.id().to_string();
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid]).output();
    }
}

#[test]
#[ignore = "diagnostic: drives real Chrome and the mouse; run with --ignored --nocapture"]
fn chrome_click_repro() {
    let _ = ms();
    let dir = std::env::temp_dir().join(format!("bf-chrome-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wm = Win32WindowManager::new();
    let _chrome = ChromeGuard(launch_chrome(&dir));

    let mut chrome = 0;
    assert!(
        wait_for(Duration::from_secs(20), || {
            chrome = wm
                .top_level_windows()
                .into_iter()
                .find(|&h| wm.info(h).is_ok_and(|i| i.title.starts_with("BFREPRO")))
                .unwrap_or(0);
            chrome != 0
        }),
        "chrome window did not appear"
    );
    let pid = wm.info(chrome).unwrap().pid;
    CHROME_HWND.store(chrome, Ordering::SeqCst);
    start_observer(pid);
    std::thread::sleep(Duration::from_millis(1500));

    // Enter fullscreen with a click in the middle of the window.
    let window = wm.rect(chrome).unwrap();
    click(window.x + window.w / 2, window.y + window.h / 2, Duration::from_millis(80), "enter");
    let monitor = wm.monitors().into_iter().find(|m| m.primary).unwrap().rect;
    assert!(
        wait_for(Duration::from_secs(5), || wm.rect(chrome).ok() == Some(monitor)),
        "not fullscreen: {:?}",
        wm.rect(chrome)
    );
    std::thread::sleep(Duration::from_millis(1500));

    // Fit with the real engine and hook thread, centred 1280×720.
    let (tx, rx) = std::sync::mpsc::channel();
    let hooks = HookThread::spawn(tx.clone()).unwrap();
    let mut settings = Settings::default();
    settings.profile.taskbar.hide_taskbar = false;
    settings.profile.region =
        RegionSpec { preset: Preset::Fixed { w: 1280, h: 720 }, ..RegionSpec::default() };
    let engine = Engine::new(
        Box::new(Win32WindowManager::new()),
        Box::new(Quiet),
        Box::new(FileMarkerStore::new(dir.join("restore-state.json"))),
        Box::new(hooks.control()),
        Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false))),
        settings,
    );
    let engine_thread = engine.spawn(rx).unwrap();
    eprintln!("{:>6} ---- fit", ms());
    tx.send(Msg::Fit { hwnd: Some(chrome), src: FitSource::Ui }).unwrap();
    let target =
        Rect::new(monitor.x + (monitor.w - 1280) / 2, monitor.y + (monitor.h - 720) / 2, 1280, 720);
    assert!(wait_for(Duration::from_secs(3), || wm.rect(chrome).ok() == Some(target)));
    let settle: u64 =
        std::env::var("BF_SETTLE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    std::thread::sleep(Duration::from_millis(settle));

    // Who is stacked above Chrome (owned popups would force a z-order change on click)?
    for hwnd in wm.top_level_windows() {
        if hwnd == chrome {
            break;
        }
        if let Ok(info) = wm.info(hwnd) {
            if info.pid == pid {
                let shown = if info.visible { "visible" } else { "hidden" };
                eprintln!("{:>6} ABOVE {hwnd:#x} {} {shown} {:?}", ms(), info.class, wm.rect(hwnd));
            }
        }
    }

    // The exit button, bottom-right of the fitted viewport.
    let (bx, by) = (target.x + target.w - 20 - 60, target.y + target.h - 20 - 30);
    click(bx, by, Duration::from_millis(80), "normal click on exit button");
    std::thread::sleep(Duration::from_millis(2500));
    let style = wm.styles(chrome).map(|(s, _)| s).unwrap_or(0);
    eprintln!("{:>6} ---- after normal click: rect {:?} style {style:#x}", ms(), wm.rect(chrome));

    if wm.rect(chrome).ok() == Some(target) {
        click(bx, by, Duration::from_millis(700), "long click on exit button");
        std::thread::sleep(Duration::from_millis(2500));
        let style = wm.styles(chrome).map(|(s, _)| s).unwrap_or(0);
        eprintln!("{:>6} ---- after long click: rect {:?} style {style:#x}", ms(), wm.rect(chrome));
    }

    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let _ = tx.send(Msg::Shutdown { done: done_tx });
    let _ = done_rx.recv_timeout(Duration::from_secs(3));
    let _ = engine_thread.join();
}
