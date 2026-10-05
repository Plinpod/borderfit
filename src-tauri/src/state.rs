//! `Core`: the Tauri-managed state, the shutdown funnel, and the engine's Tauri event sink.

use crate::engine::{EventSink, FitSource, Msg};
use crate::hooks::HookThread;
use crate::model::{
    AppError, FitState, FitStatus, HotkeyStatus, NoEligibleReason, Notice, Settings,
};
use crate::recovery::{self, FileMarkerStore, EMERGENCY};
use crate::{platform, settings, tray};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Sender};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const EVENT_STATUS: &str = "status";
pub const EVENT_NOTICE: &str = "notice";
pub const EVENT_HOTKEY: &str = "hotkey";
pub const EVENT_SETTINGS: &str = "settings";

/// How long exit waits for the engine to restore before the emergency path takes over.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Locks a mutex, recovering the data if a panicking thread poisoned it.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct Paths {
    /// `%APPDATA%\app.borderfit`: settings.json.
    pub config_dir: PathBuf,
    /// `%LOCALAPPDATA%\app.borderfit\logs`.
    pub log_dir: PathBuf,
    /// `%LOCALAPPDATA%\app.borderfit\restore-state.json`.
    pub marker: PathBuf,
}

impl Paths {
    pub fn log_file(&self) -> PathBuf {
        self.log_dir.join(format!("{}.log", crate::LOG_FILE_NAME))
    }
}

/// Facts about this process gathered once at startup.
pub struct Environment {
    pub version: String,
    pub is_elevated: bool,
    pub os_build: String,
    pub dpi_context: String,
}

pub struct Core {
    /// The persisted settings (the engine holds a snapshot refreshed by `Msg::Settings`).
    pub settings: Mutex<Settings>,
    pub engine_tx: Sender<Msg>,
    /// Last status the engine reported, for `get_app_state`.
    pub last_status: Mutex<FitStatus>,
    pub hotkey: Mutex<HotkeyStatus>,
    /// Notices from startup (crash recovery, broken settings), handed to the UI once.
    pub startup_notices: Mutex<Vec<Notice>>,
    pub paths: Paths,
    pub env: Environment,
    threads: Mutex<Threads>,
    exit_started: AtomicBool,
    single_instance_released: AtomicBool,
}

#[derive(Default)]
struct Threads {
    engine: Option<JoinHandle<()>>,
    hooks: Option<HookThread>,
}

impl Core {
    pub fn new(
        settings: Settings,
        engine_tx: Sender<Msg>,
        paths: Paths,
        env: Environment,
        startup_notices: Vec<Notice>,
    ) -> Self {
        let hotkey = HotkeyStatus { chord: settings.hotkey.clone(), ..HotkeyStatus::default() };
        Self {
            settings: Mutex::new(settings),
            engine_tx,
            last_status: Mutex::new(FitStatus::default()),
            hotkey: Mutex::new(hotkey),
            startup_notices: Mutex::new(startup_notices),
            paths,
            env,
            threads: Mutex::new(Threads::default()),
            exit_started: AtomicBool::new(false),
            single_instance_released: AtomicBool::new(false),
        }
    }

    pub fn set_threads(&self, engine: JoinHandle<()>, hooks: HookThread) {
        let mut threads = lock(&self.threads);
        threads.engine = Some(engine);
        threads.hooks = Some(hooks);
    }

    pub fn send(&self, msg: Msg) {
        if self.engine_tx.send(msg).is_err() {
            log::error!("engine is not running");
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    pub fn set_hotkey_status(&self, app: &AppHandle, status: HotkeyStatus) {
        *lock(&self.hotkey) = status.clone();
        let _ = app.emit(EVENT_HOTKEY, &status);
        tray::refresh(app);
    }

    /// Changes settings on the Rust side (not through a UI save): persists them, hands them to
    /// the engine and emits them to the UI, whose copy would otherwise go stale and be saved back
    /// over this change. Does nothing when `edit` changes nothing. Writes the file, so never call
    /// it from the engine thread.
    pub fn update_settings(&self, app: &AppHandle, edit: impl FnOnce(&mut Settings)) {
        let snapshot = {
            let mut settings = lock(&self.settings);
            let before = settings.clone();
            edit(&mut settings);
            if *settings == before {
                return;
            }
            settings.clone()
        };
        if let Err(e) = settings::save(&self.paths.config_dir, &snapshot) {
            log::error!("settings: save failed: {e}");
        }
        let _ = app.emit(EVENT_SETTINGS, &snapshot);
        self.send(Msg::Settings(Box::new(snapshot)));
    }

    /// Asks the engine to restore now and waits up to `timeout`; false on timeout.
    pub fn restore_now(&self, timeout: Duration) -> bool {
        let (tx, rx) = sync_channel(1);
        self.engine_tx.send(Msg::Restore { done: Some(tx) }).is_ok()
            && rx.recv_timeout(timeout).is_ok()
    }

    /// Releases the single-instance mutex and window exactly once (the plugin's `destroy`
    /// closes handles, so a second call would close someone else's).
    pub fn release_single_instance(&self, app: &AppHandle) {
        if !self.single_instance_released.swap(true, Ordering::SeqCst) {
            tauri_plugin_single_instance::destroy(app);
        }
    }

    /// The single, idempotent shutdown funnel (also the future updater's pre-restart hook):
    /// the engine restores and stops, or the emergency path restores from the marker; then the
    /// hook thread stops and the single-instance lock is released.
    pub fn prepare_for_exit(&self, app: &AppHandle, timeout: Duration) {
        if self.exit_started.swap(true, Ordering::SeqCst) {
            return;
        }
        log::info!("exit: restoring and stopping");
        let (tx, rx) = sync_channel(1);
        let acknowledged = self.engine_tx.send(Msg::Shutdown { done: tx }).is_ok()
            && rx.recv_timeout(timeout).is_ok();
        let mut threads = lock(&self.threads);
        if acknowledged {
            if let Some(engine) = threads.engine.take() {
                let _ = engine.join();
            }
        } else {
            log::error!("exit: engine did not answer in {timeout:?}; emergency restore");
            let wm = platform::window_manager();
            let store = FileMarkerStore::new(self.paths.marker.clone());
            recovery::emergency_restore_from_marker(&*wm, &store, &EMERGENCY);
        }
        if let Some(mut hooks) = threads.hooks.take() {
            hooks.stop();
        }
        drop(threads);
        self.release_single_instance(app);
    }
}

/// Runs the exit funnel, then exits the app.
pub fn quit(app: &AppHandle) {
    if let Some(core) = app.try_state::<Core>() {
        core.prepare_for_exit(app, SHUTDOWN_TIMEOUT);
    }
    app.exit(0);
}

/// The engine's view of Tauri: emits events and posts tray updates to the main thread.
/// Never blocks and never calls window or tray methods directly.
pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriSink {
    fn status(&self, status: &FitStatus) {
        if let Some(core) = self.app.try_state::<Core>() {
            *lock(&core.last_status) = status.clone();
            if status.state == FitState::Fitted && !lock(&core.settings).first_run_done {
                mark_first_run_done(self.app.clone());
            }
        }
        let _ = self.app.emit(EVENT_STATUS, status);
        tray::refresh(&self.app);
    }

    fn notice(&self, notice: &Notice) {
        let _ = self.app.emit(EVENT_NOTICE, notice);
    }

    fn fit_failed(&self, error: &AppError, src: FitSource) {
        let _ = self.app.emit(EVENT_NOTICE, Notice::from_error(error.clone()));
        let own_window =
            matches!(error, AppError::NoEligibleWindow { reason: NoEligibleReason::OwnWindow });
        // In a game, BorderFit never steals focus: a beep plus a tray tooltip, and the error
        // waits in the status card.
        if src == FitSource::Hotkey && !own_window {
            platform::beep();
            tray::flash_failure(&self.app);
        }
    }
}

/// Persists `first_run_done` after the first successful fit, off the engine thread.
fn mark_first_run_done(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(core) = app.try_state::<Core>() else { return };
        core.update_settings(&app, |settings| settings.first_run_done = true);
    });
}
