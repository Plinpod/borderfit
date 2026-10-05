//! BorderFit: fit any game window borderless to a region of a monitor, hide the taskbar while it
//! is fitted, and restore everything exactly. See docs/ARCHITECTURE.md.

pub mod engine;
pub mod hooks;
pub mod hotkey;
pub mod keyhook;
pub mod model;
pub mod platform;
pub mod recovery;
pub mod region;
pub mod settings;
pub mod taskbar;
pub mod win32;

mod commands;
mod state;
#[cfg(test)]
mod test_support;
mod tray;

use engine::{Engine, Msg};
use hooks::HookThread;
use model::{HotkeyStatus, Notice, Settings};
use recovery::{FileMarkerStore, StartupRecovery, EMERGENCY};
use state::{lock, Core, Environment, Paths, TauriSink, SHUTDOWN_TIMEOUT};
use std::sync::mpsc;
use std::time::Duration;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

/// Base name of the log file in `app_log_dir()`.
pub const LOG_FILE_NAME: &str = "borderfit";
const MARKER_FILE_NAME: &str = "restore-state.json";
/// Log rotation size (KeepOne keeps one previous file).
const LOG_MAX_BYTES: u128 = 1_000_000;

pub fn run() {
    let app = tauri::Builder::default()
        // Must be first: a second launch focuses this instance and exits before anything runs.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            log::info!("second launch ({argv:?}); showing the window");
            tray::show_main(app);
        }))
        .plugin(log_plugin())
        .invoke_handler(tauri::generate_handler![
            commands::get_app_state,
            commands::save_settings,
            commands::preview_region,
            commands::list_monitors,
            commands::restore_window,
            commands::validate_hotkey,
            commands::begin_hotkey_capture,
            commands::end_hotkey_capture,
            commands::import_ahk_ini,
            commands::relaunch_elevated,
            commands::open_link,
            commands::get_diagnostics,
        ])
        .setup(|app| {
            setup(app.handle())?;
            Ok(())
        })
        .on_window_event(on_window_event)
        .build(tauri::generate_context!())
        .expect("error while building BorderFit");
    app.run(on_run_event);
}

fn log_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let level =
        if cfg!(debug_assertions) { log::LevelFilter::Debug } else { log::LevelFilter::Info };
    let mut builder = tauri_plugin_log::Builder::new()
        .clear_targets()
        .target(Target::new(TargetKind::LogDir { file_name: Some(LOG_FILE_NAME.into()) }))
        .max_file_size(LOG_MAX_BYTES)
        .rotation_strategy(RotationStrategy::KeepOne)
        .level(level)
        // Dependency chatter stays out of the user's log.
        .level_for("tao", log::LevelFilter::Warn)
        .level_for("wry", log::LevelFilter::Warn);
    if cfg!(debug_assertions) {
        builder = builder.target(Target::new(TargetKind::Stdout));
    }
    builder.build()
}

fn resolve_paths(app: &AppHandle) -> Result<Paths, tauri::Error> {
    let resolver = app.path();
    Ok(Paths {
        config_dir: resolver.app_config_dir()?,
        log_dir: resolver.app_log_dir()?,
        marker: resolver.app_local_data_dir()?.join(MARKER_FILE_NAME),
    })
}

fn gather_environment(app: &AppHandle) -> Environment {
    Environment {
        version: app.package_info().version.to_string(),
        is_elevated: platform::is_elevated(),
        os_build: platform::os_build(),
        dpi_context: platform::dpi_context(),
    }
}

/// Loads settings; a first run gets defaults shaped by the primary monitor.
fn load_settings(
    paths: &Paths,
    monitors: &[model::MonitorInfo],
    notices: &mut Vec<Notice>,
) -> Settings {
    let loaded = settings::load(&paths.config_dir);
    notices.extend(loaded.notice);
    if !loaded.fresh {
        return loaded.settings;
    }
    let defaults = settings::first_run_defaults(monitors);
    if let Err(e) = settings::save(&paths.config_dir, &defaults) {
        log::error!("settings: first save failed: {e}");
    }
    defaults
}

fn recovery_notice(outcome: &StartupRecovery, hotkey: &str) -> Option<Notice> {
    match outcome {
        StartupRecovery::Clean => None,
        StartupRecovery::TaskbarRestored { failed } if failed.is_empty() => {
            Some(Notice::info("Taskbar restored after an unclean exit."))
        }
        StartupRecovery::TaskbarRestored { failed } => Some(Notice::from_error(
            model::AppError::TaskbarRestoreFailed { hwnds: failed.clone() },
        )),
        StartupRecovery::Adopted(adopted) => Some(Notice::info(format!(
            "BorderFit exited unexpectedly while \"{}\" was fitted. It is still tracked; press {hotkey} or click Restore.",
            adopted.tracked.title
        ))),
    }
}

/// Startup, in order: facts and logging, settings, instance check, crash recovery, engine and
/// hook threads, hotkey, tray, subclass, autostart refresh, window visibility.
fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let env = gather_environment(app);
    let paths = resolve_paths(app)?;
    log::info!(
        "BorderFit {} on Windows {}; DPI {}; elevated {}",
        env.version,
        env.os_build,
        env.dpi_context,
        env.is_elevated
    );

    let wm = platform::window_manager();
    let monitors = wm.monitors();
    log::info!(
        "monitors: {:?}",
        monitors.iter().map(|m| (&m.device, m.rect, m.dpi)).collect::<Vec<_>>()
    );
    let mut notices = Vec::new();
    let settings = load_settings(&paths, &monitors, &mut notices);

    // An elevated BorderFit is invisible to the single-instance plugin from here.
    let other_instance = platform::other_instance_running();
    if other_instance {
        // Usually an elevated instance, but also the original one after a declined UAC prompt
        // (its single-instance lock is already released by then), so don't claim which.
        log::warn!("another BorderFit holds the instance event");
        notices.push(Notice::info("Another BorderFit is already running; close it first."));
    } else {
        platform::create_instance_event();
    }

    let marker_store = FileMarkerStore::new(paths.marker.clone());
    let recovery = if other_instance {
        StartupRecovery::Clean
    } else {
        recovery::run_at_startup(&*wm, &marker_store)
    };
    notices.extend(recovery_notice(&recovery, &settings.hotkey));

    let (tx, rx) = mpsc::channel::<Msg>();
    app.manage(Core::new(settings.clone(), tx.clone(), paths, env, notices));
    let core = app.state::<Core>();

    let hooks = HookThread::spawn(tx.clone())?;
    let mut engine = Engine::new(
        wm,
        Box::new(TauriSink::new(app.clone())),
        Box::new(marker_store),
        Box::new(hooks.control()),
        &EMERGENCY,
        settings.clone(),
    );
    if let StartupRecovery::Adopted(adopted) = recovery {
        engine.adopt(adopted);
    }
    core.set_threads(engine.spawn(rx)?, hooks);

    let hotkey_ok = setup_hotkey(app, &core, tx, &settings.hotkey, other_instance);
    tray::build(app)?;
    install_subclass(app);
    refresh_autostart(&core, &settings);
    install_panic_hook(app.clone());

    let minimized = std::env::args().any(|arg| arg == "--minimized");
    if !minimized || !hotkey_ok || other_instance {
        tray::show_main(app);
    }
    Ok(())
}

/// Installs the handler, creates the manager and registers the chord. Returns false when the
/// hotkey could not be registered (the window is then shown so the conflict is visible).
fn setup_hotkey(
    app: &AppHandle,
    core: &Core,
    tx: mpsc::Sender<Msg>,
    chord: &str,
    other_instance: bool,
) -> bool {
    hotkey::install_handler(tx);
    let result = hotkey::create_manager().and_then(|()| {
        if other_instance {
            return Ok(None);
        }
        hotkey::register(chord).map(Some)
    });
    let status = match result {
        Ok(Some(binding)) => HotkeyStatus::active(chord, binding),
        Ok(None) => HotkeyStatus { chord: chord.to_string(), ..Default::default() },
        Err(error) => {
            log::warn!("hotkey: {chord} not registered: {error}");
            HotkeyStatus { chord: chord.to_string(), error: Some(error), ..Default::default() }
        }
    };
    let ok = status.error.is_none();
    core.set_hotkey_status(app, status);
    ok
}

/// Hears Explorer restarts and session end on the (hidden, unowned) main window.
fn install_subclass(app: &AppHandle) {
    #[cfg(windows)]
    {
        let Some(window) = app.get_webview_window("main") else { return };
        let Ok(hwnd) = window.hwnd() else { return };
        let handle = app.clone();
        platform::install_subclass(hwnd.0 as isize, move |message| {
            let Some(core) = handle.try_state::<Core>() else { return };
            match message {
                platform::ShellMessage::TaskbarCreated => core.send(Msg::TaskbarCreated),
                platform::ShellMessage::QueryEndSession => {
                    log::info!("session end requested; restoring");
                    core.restore_now(Duration::from_secs(2));
                }
                platform::ShellMessage::EndSession => {
                    log::info!("session ending");
                    core.prepare_for_exit(&handle, Duration::from_secs(2));
                }
            }
        });
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Re-writes the Run value so it follows the exe after an update or a move.
fn refresh_autostart(core: &Core, settings: &Settings) {
    if settings.launch_at_startup && !core.env.is_elevated {
        if let Err(e) = platform::set_autostart(true) {
            log::warn!("autostart: {e}");
        }
    }
}

/// Logs panics; a panic on the main thread takes the app down, so restore first.
fn install_panic_hook(app: AppHandle) {
    let main_thread = std::thread::current().id();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        if std::thread::current().id() == main_thread {
            if let Some(core) = app.try_state::<Core>() {
                core.prepare_for_exit(&app, Duration::from_secs(2));
            }
        }
        default_hook(info);
    }));
}

fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let close_to_tray =
            window.try_state::<Core>().is_none_or(|core| lock(&core.settings).close_to_tray);
        if close_to_tray {
            let _ = window.hide();
        } else {
            state::quit(window.app_handle());
        }
    }
}

fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        // Only a destroyed last window (or a crash path) asks to exit with no code. Tray Exit
        // and quit_app pass Some(0) and must go through, so only `None` is prevented.
        RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
        RunEvent::Exit => {
            if let Some(core) = app.try_state::<Core>() {
                core.prepare_for_exit(app, SHUTDOWN_TIMEOUT);
            }
        }
        _ => {}
    }
}
