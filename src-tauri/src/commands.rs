//! Every `#[tauri::command]`. The only module that knows both the AppHandle and the engine.
//!
//! Sync commands run on the main thread, so they only send messages or do quick reads. The
//! few that must wait (settings save, hotkey capture) are `async` and wait inside
//! `spawn_blocking`, never on a tokio worker or the main thread. Errors reach the UI as
//! `Notice`s (`{ level, error, message }`).

use crate::engine::Msg;
use crate::model::{
    AppError, AppState, Diagnostics, HotkeyStatus, ImportReport, MonitorInfo, Notice, Rect,
    RegionSpec, Settings, TrayWindowInfo,
};
use crate::state::{lock, Core};
use crate::{hotkey, platform, region, settings};
use std::fs;
use std::sync::mpsc::sync_channel;
use std::time::Duration;
use tauri::{AppHandle, State};

pub const REPO_URL: &str = "https://github.com/Plinpod/borderfit";
const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(2);
const LOG_TAIL_LINES: usize = 20;
/// The AHK INI is a few hundred bytes; refuse anything absurd.
const MAX_INI_BYTES: u64 = 64 * 1024;

type CmdResult<T> = Result<T, Notice>;

fn fail(error: AppError) -> Notice {
    Notice::from_error(error)
}

/// Runs `f` on the main thread (where the hotkey manager lives) and waits for its result
/// without blocking an async worker.
async fn on_main_thread<T, F>(app: &AppHandle, f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| AppError::system(format!("main thread unavailable: {e}")))?;
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(MAIN_THREAD_TIMEOUT))
        .await
        .map_err(|_| AppError::Timeout)?
        .map_err(|_| AppError::Timeout)
}

#[tauri::command]
pub fn get_app_state(core: State<'_, Core>) -> AppState {
    // Also the day-1 check that the UI loaded and IPC crosses the CSP.
    log::info!("ui: get_app_state");
    AppState {
        settings: core.settings(),
        status: lock(&core.last_status).clone(),
        monitors: platform::window_manager().monitors(),
        hotkey: lock(&core.hotkey).clone(),
        is_elevated: core.env.is_elevated,
        os_build: core.env.os_build.clone(),
        log_dir: core.paths.log_dir.display().to_string(),
        version: core.env.version.clone(),
        notices: std::mem::take(&mut *lock(&core.startup_notices)),
    }
}

/// Validates and persists settings, then tells the engine (which re-fits if the region
/// changed), rebinds the hotkey if it differs, and updates the Run value if needed.
#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    core: State<'_, Core>,
    settings: Settings,
) -> CmdResult<Settings> {
    let mut next = settings::validate(settings).map_err(fail)?;
    let previous = core.settings();
    // Only Rust sets this flag; a UI copy saved before the `settings` event arrived must not
    // clear it again.
    next.first_run_done |= previous.first_run_done;

    if next.hotkey != previous.hotkey {
        let chord = next.hotkey.clone();
        let result = on_main_thread(&app, move || hotkey::rebind(&chord)).await.map_err(fail)?;
        match result {
            Ok(binding) => {
                core.set_hotkey_status(&app, HotkeyStatus::active(&next.hotkey, binding));
            }
            Err((error, restored)) => {
                // Keep the old chord and say why the new one failed.
                next.hotkey = previous.hotkey.clone();
                core.set_hotkey_status(
                    &app,
                    HotkeyStatus::failed(&previous.hotkey, error, restored),
                );
            }
        }
    }

    if next.launch_at_startup != previous.launch_at_startup {
        if core.env.is_elevated {
            next.launch_at_startup = previous.launch_at_startup;
        } else if let Err(error) = platform::set_autostart(next.launch_at_startup) {
            log::error!("autostart: {error}");
            next.launch_at_startup = previous.launch_at_startup;
        }
    }

    let dir = core.paths.config_dir.clone();
    let to_save = next.clone();
    tauri::async_runtime::spawn_blocking(move || settings::save(&dir, &to_save))
        .await
        .map_err(|_| fail(AppError::Timeout))?
        .map_err(|e| fail(AppError::system(format!("Settings could not be saved: {e}"))))?;
    *lock(&core.settings) = next.clone();
    core.send(Msg::Settings(Box::new(next.clone())));
    Ok(next)
}

/// Absolute rect a region resolves to on the current monitors.
#[tauri::command]
pub fn preview_region(spec: RegionSpec) -> CmdResult<Rect> {
    let monitors = platform::window_manager().monitors();
    region::resolve_on(&monitors, &spec).map(|r| r.rect).map_err(fail)
}

#[tauri::command]
pub fn list_monitors() -> Vec<MonitorInfo> {
    platform::window_manager().monitors()
}

#[tauri::command]
pub fn restore_window(core: State<'_, Core>) {
    core.send(Msg::Restore { done: None });
}

#[tauri::command]
pub fn validate_hotkey(chord: String) -> CmdResult<String> {
    hotkey::normalize(&chord).map_err(fail)
}

/// Unregisters the live chord (or stops the keyboard hook) so the webview can see the keys
/// being recorded.
#[tauri::command]
pub async fn begin_hotkey_capture(
    app: AppHandle,
    core: State<'_, Core>,
) -> CmdResult<HotkeyStatus> {
    on_main_thread(&app, hotkey::unregister).await.map_err(fail)?;
    let status = HotkeyStatus { chord: core.settings().hotkey, ..Default::default() };
    core.set_hotkey_status(&app, status.clone());
    Ok(status)
}

/// Ends recording: `None` re-registers the old chord; a chord is registered and persisted,
/// or on failure the old chord is registered again and the error is reported. A chord Windows
/// refuses (e.g. F12) still succeeds here, through the keyboard hook.
#[tauri::command]
pub async fn end_hotkey_capture(
    app: AppHandle,
    core: State<'_, Core>,
    chord: Option<String>,
) -> CmdResult<HotkeyStatus> {
    let old = core.settings().hotkey;
    let wanted = match chord {
        Some(chord) => Some(hotkey::normalize(&chord).map_err(fail)?),
        None => None,
    };
    let attempt = wanted.clone().unwrap_or_else(|| old.clone());
    let fallback = old.clone();
    let result =
        on_main_thread(&app, move || hotkey::register_or_restore(&attempt, Some(&fallback)))
            .await
            .map_err(fail)?;

    let status = match (result, wanted) {
        (Ok(binding), Some(new)) => {
            persist_hotkey(&app, &core, &new);
            HotkeyStatus::active(&new, binding)
        }
        (Ok(binding), None) => HotkeyStatus::active(&old, binding),
        (Err((error, restored)), _) => HotkeyStatus::failed(&old, error, restored),
    };
    core.set_hotkey_status(&app, status.clone());
    Ok(status)
}

fn persist_hotkey(app: &AppHandle, core: &Core, chord: &str) {
    core.update_settings(app, |settings| {
        settings.hotkey = chord.to_string();
        // Choosing F12 in the recorder is deliberate: its inline warning was shown.
        if chord == "F12" {
            settings.legacy_f12_ack = true;
        }
    });
}

/// Reads the AHK script's INI and returns the settings it would produce (not saved).
#[tauri::command]
pub fn import_ahk_ini(core: State<'_, Core>, path: String) -> CmdResult<ImportReport> {
    let too_big = fs::metadata(&path).map(|m| m.len() > MAX_INI_BYTES).unwrap_or(false);
    if too_big {
        return Err(fail(AppError::settings(
            "That file is too large to be borderless_config.ini.",
        )));
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| fail(AppError::system(format!("Could not read {path}: {e}"))))?;
    let monitors = platform::window_manager().monitors();
    settings::import_ahk_ini(&text, &core.settings(), &monitors).map_err(fail)
}

/// Restores, releases the single-instance lock, starts an elevated copy and exits. The work
/// runs on its own thread; a declined UAC prompt arrives as a notice.
#[tauri::command]
pub fn relaunch_elevated(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(core) = tauri::Manager::try_state::<Core>(&app) else { return };
        if !core.restore_now(crate::state::SHUTDOWN_TIMEOUT) {
            log::warn!("relaunch: engine did not confirm the restore");
        }
        core.release_single_instance(&app);
        platform::close_instance_event();
        match platform::relaunch_elevated() {
            Ok(()) => crate::state::quit(&app),
            Err(error) => {
                log::warn!("relaunch declined or failed: {error}");
                platform::create_instance_event();
                let notice = Notice::from_error(error);
                let _ = tauri::Emitter::emit(&app, crate::state::EVENT_NOTICE, notice);
            }
        }
    });
}

#[tauri::command]
pub fn open_link(core: State<'_, Core>, target: String) {
    match target.as_str() {
        "repo" => platform::shell_open(REPO_URL),
        "issues" => platform::shell_open(&issue_url(&core)),
        "logs" => platform::shell_open(&core.paths.log_dir.display().to_string()),
        other => log::warn!("open_link: unknown target {other}"),
    }
}

/// New-issue URL with the bug form's version and Windows fields prefilled.
fn issue_url(core: &Core) -> String {
    let encode =
        |s: &str| s.replace('%', "%25").replace(' ', "%20").replace('(', "%28").replace(')', "%29");
    format!(
        "{REPO_URL}/issues/new?template=bug_report.yml&version={}&windows={}",
        encode(&core.env.version),
        encode(&core.env.os_build)
    )
}

#[tauri::command]
pub fn get_diagnostics(core: State<'_, Core>) -> Diagnostics {
    let wm = platform::window_manager();
    Diagnostics {
        version: core.env.version.clone(),
        dpi_context: core.env.dpi_context.clone(),
        elevated: core.env.is_elevated,
        os_build: core.env.os_build.clone(),
        tray_windows: wm
            .tray_windows()
            .into_iter()
            .map(|t| TrayWindowInfo {
                hwnd: t.hwnd,
                class: t.class,
                exe: t.exe,
                visible: t.visible,
            })
            .collect(),
        marker_present: core.paths.marker.exists(),
        hotkey: lock(&core.hotkey).clone(),
        monitors: wm.monitors(),
        log_tail: log_tail(&core),
    }
}

fn log_tail(core: &Core) -> Vec<String> {
    let Ok(text) = fs::read_to_string(core.paths.log_file()) else { return Vec::new() };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    lines[start..].iter().map(|l| (*l).to_string()).collect()
}
