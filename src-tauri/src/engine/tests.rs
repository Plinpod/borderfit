//! Engine behaviour over `FakeWindowManager`, asserting call-log order where it matters.

use super::*;
use crate::model::{
    DpiAwareness, HAlign, Preset, Rect, RegionSpec, ShowState, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED,
    SW_SHOWMINNOACTIVE,
};
use crate::region::tests::dual_layout;
use crate::win32::fake::{Call, FakeMarkerStore, FakeState, FakeWindow, FakeWindowManager};
use crate::win32::WS_EX_TOOLWINDOW;
use std::sync::{Arc, Mutex};

const GAME: isize = 50;
const GAME_PID: u32 = 77;
const OWN_WINDOW: isize = 900;
const OWN_PID: u32 = 1000;
const TRAY: isize = 10;
const SECONDARY_TRAY: isize = 11;
const TARGET: Rect = Rect::new(1280, 0, 2560, 1440);
const ORIGINAL: Rect = Rect::new(100, 140, 1920, 1080);

#[derive(Clone, Default)]
struct Recorder {
    statuses: Arc<Mutex<Vec<FitStatus>>>,
    notices: Arc<Mutex<Vec<Notice>>>,
    failures: Arc<Mutex<Vec<(AppError, FitSource)>>>,
}

impl EventSink for Recorder {
    fn status(&self, status: &FitStatus) {
        self.statuses.lock().unwrap().push(status.clone());
    }
    fn notice(&self, notice: &Notice) {
        self.notices.lock().unwrap().push(notice.clone());
    }
    fn fit_failed(&self, error: &AppError, src: FitSource) {
        self.failures.lock().unwrap().push((error.clone(), src));
    }
}

impl Recorder {
    fn last_status(&self) -> FitStatus {
        self.statuses.lock().unwrap().last().cloned().unwrap_or_default()
    }
    fn failures(&self) -> Vec<AppError> {
        self.failures.lock().unwrap().iter().map(|(e, _)| e.clone()).collect()
    }
}

struct Harness {
    wm: FakeWindowManager,
    engine: Engine,
    sink: Recorder,
    emergency: &'static AtomicBool,
}

impl Harness {
    fn new(state: FakeState) -> Self {
        Self::with_settings(state, Settings::default())
    }

    fn with_settings(state: FakeState, settings: Settings) -> Self {
        let wm = FakeWindowManager::new(state);
        let sink = Recorder::default();
        let emergency: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let engine = Engine::new(
            Box::new(wm.clone()),
            Box::new(sink.clone()),
            Box::new(FakeMarkerStore::new(&wm)),
            Box::new(NoHooks),
            emergency,
            settings,
        );
        Self { wm, engine, sink, emergency }
    }

    fn send(&mut self, msg: Msg) -> bool {
        self.engine.handle(msg)
    }

    fn toggle(&mut self) {
        self.send(Msg::Toggle { src: FitSource::Hotkey });
    }

    fn calls(&self) -> Vec<Call> {
        self.wm.lock().calls.clone()
    }

    fn clear_calls(&self) {
        self.wm.lock().calls.clear();
    }

    fn game(&self) -> FakeWindow {
        self.wm.lock().window(GAME).clone()
    }

    fn tray_visible(&self, hwnd: isize) -> bool {
        self.wm.lock().tray(hwnd).visible
    }

    fn marker_present(&self) -> bool {
        self.wm.lock().marker.is_some()
    }

    /// Makes the pending verify step due now (instead of sleeping in tests).
    fn verify_now(&mut self) {
        if let Some(session) = self.engine.session.as_mut() {
            if session.verify.due.is_some() {
                session.verify.due = Some(Instant::now());
            }
        }
        self.engine.tick();
    }
}

/// The ultrawide dual layout, a work area starting at y=40 (taskbar on top), explorer's two trays,
/// BorderFit's own window, and "Game" in the foreground.
fn desktop() -> FakeState {
    let mut s = FakeState::empty();
    s.own_pid = OWN_PID;
    s.monitors = dual_layout();
    s.work_origin = (0, 40);
    s.add_tray(TRAY, "Shell_TrayWnd", "explorer.exe");
    s.add_tray(SECONDARY_TRAY, "Shell_SecondaryTrayWnd", "explorer.exe");
    s.add_window(FakeWindow::normal(OWN_WINDOW, OWN_PID, "BorderFit"));
    s.add_window(FakeWindow::normal(GAME, GAME_PID, "Game"));
    s.foreground = Some(GAME);
    s
}

fn first_mutation_index(calls: &[Call]) -> Option<usize> {
    calls.iter().position(|c| c.hwnd().is_some())
}

// ---------------------------------------------------------------------------------------------
// Fit and restore
// ---------------------------------------------------------------------------------------------

#[test]
fn hotkey_fits_then_restores_exactly() {
    let mut h = Harness::new(desktop());
    let before = h.game();

    h.toggle();
    let fitted = h.game();
    assert_eq!(fitted.rect, TARGET);
    assert_eq!(fitted.style, before.style & !DEFAULT_STYLE_MASK);
    assert_eq!(fitted.ex_style, before.ex_style & !DEFAULT_EX_STYLE_MASK);
    assert!(!h.tray_visible(TRAY));
    assert!(h.tray_visible(SECONDARY_TRAY), "secondary trays stay unless enabled");
    assert!(h.marker_present());
    let status = h.sink.last_status();
    assert_eq!(status.state, FitState::Fitted);
    assert_eq!(status.target, Some(TARGET));
    assert!(status.taskbar_hidden);
    assert_eq!(status.restore.as_ref().map(|r| r.rect), Some(ORIGINAL));

    h.toggle();
    let restored = h.game();
    assert_eq!(restored.rect, before.rect);
    assert_eq!(restored.style, before.style);
    assert_eq!(restored.ex_style, before.ex_style);
    assert!(h.tray_visible(TRAY));
    assert!(!h.marker_present());
    assert_eq!(h.sink.last_status().state, FitState::Idle);
}

#[test]
fn marker_is_written_before_any_mutation() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let calls = h.calls();
    assert!(matches!(calls[0], Call::MarkerWrite { .. }), "{calls:?}");
    assert!(first_mutation_index(&calls) > Some(0));
}

#[test]
fn fit_sequence_order_is_foreground_taskbar_styles_dwm_pos() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let calls: Vec<Call> = h.calls().into_iter().filter(|c| c.hwnd().is_some()).collect();
    let kinds: Vec<&str> = calls
        .iter()
        .map(|c| match c {
            Call::SetForeground { .. } => "fg",
            Call::Show { hwnd: TRAY, .. } => "tray",
            Call::SetStyles { .. } => "styles",
            Call::DwmSquare { .. } => "dwm",
            Call::SetPos { .. } => "pos",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["fg", "tray", "fg", "styles", "dwm", "pos"]);
}

#[test]
fn maximized_window_is_restored_first_and_the_marker_rewritten() {
    let mut state = desktop();
    let game = state.window_mut(GAME);
    game.zoomed = true;
    game.rect = Rect::new(-8, 32, 5136, 1416);
    game.style |= WS_MAXIMIZE;
    game.placement.show_cmd = SW_SHOWMAXIMIZED;
    let normal_style = game.style & !WS_MAXIMIZE;
    let mut h = Harness::new(state);

    h.toggle();
    let calls = h.calls();
    assert_eq!(calls[0], Call::MarkerWrite { rect_provisional: true, taskbar_hidden: true });
    assert_eq!(calls[1], Call::Show { hwnd: GAME, cmd: ShowCmd::Restore });
    assert_eq!(calls[2], Call::MarkerWrite { rect_provisional: false, taskbar_hidden: true });
    assert_eq!(h.game().rect, TARGET);

    // The fit started from the un-maximized words, so restore never writes WS_MAXIMIZE back.
    h.wm.lock().window_mut(GAME).style = 0x1400_0000; // WS_VISIBLE | WS_CLIPSIBLINGS, borderless
    h.clear_calls();
    h.toggle();
    let calls = h.calls();
    assert!(calls.contains(&Call::SetStyles { hwnd: GAME, style: normal_style, ex_style: 0x100 }));
    assert!(calls.contains(&Call::SetPlacement { hwnd: GAME, show_cmd: SW_SHOWMAXIMIZED }));
    assert!(h.game().zoomed);
}

#[test]
fn window_without_a_caption_never_gains_one() {
    let mut state = desktop();
    // A popup with only a thick frame: the AHK script's OR-back would add WS_CAPTION.
    state.window_mut(GAME).style = 0x9404_0000;
    let mut h = Harness::new(state);
    h.toggle();
    assert_eq!(h.game().style, 0x9400_0000);
    h.toggle();
    assert_eq!(h.game().style, 0x9404_0000);
}

#[test]
fn normal_window_restores_to_its_window_rect_not_the_workspace_rect() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.clear_calls();
    h.toggle();
    // Workspace (100,100) + work origin (0,40) would be the same place; the engine must use
    // GetWindowRect (screen coordinates) with set_pos and never go through the placement.
    assert_eq!(h.game().rect, ORIGINAL);
    assert!(h.calls().iter().all(|c| !matches!(c, Call::SetPlacement { .. })));
    assert!(h.calls().contains(&Call::SetPos { hwnd: GAME, rect: ORIGINAL, z: ZOrder::Keep }));
}

#[test]
fn minimized_at_capture_is_restored_through_the_placement() {
    let mut state = desktop();
    let game = state.window_mut(GAME);
    game.iconic = true;
    game.placement.show_cmd = SW_SHOWMINIMIZED;
    let mut h = Harness::with_settings(state, Settings::default());
    h.send(Msg::Fit { hwnd: Some(GAME), src: FitSource::Ui });
    assert_eq!(h.game().rect, TARGET);
    assert!(!h.game().iconic);

    h.clear_calls();
    h.toggle();
    let calls = h.calls();
    assert!(calls.contains(&Call::SetPlacement { hwnd: GAME, show_cmd: SW_SHOWMINNOACTIVE }));
    assert!(calls.contains(&Call::Show { hwnd: GAME, cmd: ShowCmd::ShowMinNoActive }));
    assert!(h.game().iconic);
}

#[test]
fn window_that_was_already_borderless_gets_no_style_write() {
    let mut state = desktop();
    state.window_mut(GAME).style = 0x9400_0000;
    state.window_mut(GAME).ex_style = 0;
    let mut h = Harness::new(state);
    h.toggle();
    assert!(h.calls().iter().all(|c| !matches!(c, Call::SetStyles { .. })));
    assert_eq!(h.game().rect, TARGET);
    assert_eq!(h.sink.last_status().restore.map(|r| r.had_borders), Some(false));
}

// ---------------------------------------------------------------------------------------------
// Refusals with zero side effects
// ---------------------------------------------------------------------------------------------

#[test]
fn elevated_target_is_refused_with_zero_mutations() {
    let mut state = desktop();
    state.window_mut(GAME).elevated = true;
    let mut h = Harness::new(state);
    h.toggle();
    assert!(h.calls().is_empty(), "{:?}", h.calls());
    assert!(matches!(h.sink.failures()[..], [AppError::NeedsAdmin { .. }]));
    assert!(h.tray_visible(TRAY));
}

#[test]
fn higher_integrity_level_is_refused_before_the_probe() {
    let mut state = desktop();
    state.integrity_levels.insert(GAME_PID, 0x3000);
    state.integrity_levels.insert(OWN_PID, 0x2000);
    let mut h = Harness::new(state);
    h.toggle();
    assert!(h.calls().is_empty());
    assert_eq!(
        h.sink.failures(),
        vec![AppError::NeedsAdmin { exe: "game.exe".into(), title: "Game".into() }]
    );
}

#[test]
fn hotkey_with_borderfit_in_front_does_nothing() {
    let mut state = desktop();
    state.foreground = Some(OWN_WINDOW);
    let mut h = Harness::new(state);
    h.toggle();
    assert!(h.calls().is_empty());
    assert_eq!(
        h.sink.failures(),
        vec![AppError::NoEligibleWindow { reason: NoEligibleReason::OwnWindow }]
    );
}

#[test]
fn hotkey_on_the_taskbar_or_a_tool_window_does_nothing() {
    let mut state = desktop();
    let mut shell = FakeWindow::normal(60, 5, "Taskbar");
    shell.class = "Shell_TrayWnd".into();
    state.add_window(shell);
    let mut tool = FakeWindow::normal(61, 6, "Palette");
    tool.ex_style |= WS_EX_TOOLWINDOW;
    state.add_window(tool);

    state.foreground = Some(60);
    let mut h = Harness::new(state);
    h.toggle();
    h.wm.lock().foreground = Some(61);
    h.toggle();
    assert!(h.calls().is_empty());
    assert_eq!(
        h.sink.failures(),
        vec![
            AppError::NoEligibleWindow { reason: NoEligibleReason::ShellWindow },
            AppError::NoEligibleWindow { reason: NoEligibleReason::NotFittable },
        ]
    );
}

#[test]
fn tray_fit_takes_the_window_the_user_was_in() {
    let mut state = desktop();
    let mut tool = FakeWindow::normal(61, 6, "Palette");
    tool.ex_style |= WS_EX_TOOLWINDOW;
    state.add_window(tool);
    // z-order: BorderFit, a tool window, then the game.
    state.z_order = vec![OWN_WINDOW, 61, GAME];
    state.foreground = Some(OWN_WINDOW);
    let mut h = Harness::new(state);
    h.send(Msg::Toggle { src: FitSource::Tray });
    assert_eq!(h.game().rect, TARGET);
    assert_eq!(h.sink.last_status().window.map(|w| w.hwnd), Some(GAME));
}

#[test]
fn offscreen_region_is_refused_with_zero_side_effects() {
    let mut settings = Settings::default();
    settings.profile.region.override_rect = Some(Rect::new(5000, 0, 2560, 1440));
    let mut h = Harness::with_settings(desktop(), settings);
    h.toggle();
    assert!(h.calls().is_empty());
    assert!(matches!(h.sink.failures()[..], [AppError::RegionOffscreen { .. }]));
}

#[test]
fn marker_write_failure_is_refused_with_zero_mutations() {
    let mut state = desktop();
    state.fail_marker_writes = true;
    let mut h = Harness::new(state);
    h.toggle();
    assert!(h.calls().is_empty());
    assert!(matches!(h.sink.failures()[..], [AppError::System { .. }]));
    assert!(!h.engine.is_fitted());
}

#[test]
fn fit_request_while_fitted_reports_busy() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.send(Msg::Fit { hwnd: None, src: FitSource::Ui });
    assert_eq!(h.sink.failures(), vec![AppError::Busy { title: "Game".into() }]);
    assert!(h.engine.is_fitted());
}

#[test]
fn failed_move_after_the_taskbar_is_hidden_rolls_everything_back() {
    let mut state = desktop();
    state.window_mut(GAME).fail_set_pos_to = Some(TARGET);
    let mut h = Harness::new(state);
    let before = h.game();

    h.toggle();
    assert!(
        h.calls().contains(&Call::Show { hwnd: TRAY, cmd: ShowCmd::HideAsync }),
        "the failure comes after the taskbar was hidden"
    );
    assert!(matches!(h.sink.failures()[..], [AppError::Win32 { .. }]));
    assert!(!h.engine.is_fitted());
    assert!(h.tray_visible(TRAY));
    assert!(!h.marker_present());
    let after = h.game();
    assert_eq!(
        (after.style, after.ex_style, after.rect),
        (before.style, before.ex_style, before.rect)
    );
}

#[test]
fn failed_un_maximize_rolls_back_before_the_taskbar_is_touched() {
    let mut state = desktop();
    let game = state.window_mut(GAME);
    game.zoomed = true;
    game.style |= WS_MAXIMIZE;
    game.placement.show_cmd = SW_SHOWMAXIMIZED;
    game.fail_restore = true;
    let mut h = Harness::new(state);

    h.toggle();
    assert!(matches!(h.sink.failures()[..], [AppError::Win32 { .. }]));
    assert!(!h.engine.is_fitted());
    assert!(!h.marker_present());
    assert!(h.calls().iter().all(|c| !matches!(c, Call::Show { hwnd: TRAY, .. })));
    assert!(h.game().zoomed, "the window is left maximized");
}

// ---------------------------------------------------------------------------------------------
// Tick
// ---------------------------------------------------------------------------------------------

#[test]
fn game_exit_restores_the_taskbar_without_window_calls() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.clear_calls();
    h.wm.lock().kill_process(GAME_PID);
    h.engine.tick();
    assert!(!h.engine.is_fitted());
    assert!(h.tray_visible(TRAY));
    assert!(!h.marker_present());
    assert!(h.calls().iter().all(|c| c.hwnd() != Some(GAME)), "{:?}", h.calls());
}

#[test]
fn destroyed_window_of_a_living_process_is_confirmed_and_released() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.wm.lock().windows.remove(&GAME);
    h.engine.tick();
    assert!(!h.engine.is_fitted());
    assert!(h.tray_visible(TRAY));
}

#[test]
fn minimized_game_shows_the_taskbar_and_hides_it_again_when_back() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.wm.lock().window_mut(GAME).iconic = true;
    h.engine.tick();
    assert!(h.tray_visible(TRAY));
    let status = h.sink.last_status();
    assert!(!status.taskbar_hidden && !status.target_visible);
    assert_eq!(h.wm.lock().marker.as_ref().map(|m| m.taskbar.taskbar_hidden), Some(false));

    h.wm.lock().window_mut(GAME).iconic = false;
    h.engine.tick();
    assert!(!h.tray_visible(TRAY));
    assert!(h.sink.last_status().taskbar_hidden);
}

#[test]
fn taskbar_re_shown_by_the_shell_is_hidden_again_on_the_next_tick() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.wm.lock().trays[0].visible = true;
    h.engine.tick();
    assert!(!h.tray_visible(TRAY));
}

#[test]
fn explorer_restart_is_rediscovered() {
    let mut h = Harness::new(desktop());
    h.toggle();
    {
        let mut s = h.wm.lock();
        s.trays.clear();
        s.add_tray(20, "Shell_TrayWnd", "explorer.exe");
    }
    h.send(Msg::TaskbarCreated);
    assert!(!h.tray_visible(20));
}

#[test]
fn secondary_taskbars_follow_the_setting_live() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let mut settings = Settings::default();
    settings.profile.taskbar.hide_secondary_taskbars = true;
    h.send(Msg::Settings(Box::new(settings.clone())));
    assert!(!h.tray_visible(SECONDARY_TRAY));
    settings.profile.taskbar.hide_taskbar = false;
    h.send(Msg::Settings(Box::new(settings)));
    assert!(h.tray_visible(TRAY) && h.tray_visible(SECONDARY_TRAY));
}

// ---------------------------------------------------------------------------------------------
// Verify and refit
// ---------------------------------------------------------------------------------------------

#[test]
fn game_that_enforces_its_size_ends_degraded_but_fitted() {
    let mut state = desktop();
    state.window_mut(GAME).enforced_size = Some((2560, 1417));
    let mut h = Harness::new(state);
    h.toggle();
    for _ in 0..(MAX_REAPPLY + 1) {
        h.verify_now();
    }
    assert!(h.engine.is_fitted());
    let status = h.sink.last_status();
    assert_eq!(status.verify_attempts, MAX_REAPPLY);
    assert_eq!(
        status.degraded,
        Some(AppError::ResizeRefused {
            wanted: TARGET,
            got: Rect::new(1280, 0, 2560, 1417),
            attempts: MAX_REAPPLY
        })
    );
    let set_pos_count = h.calls().iter().filter(|c| matches!(c, Call::SetPos { .. })).count();
    assert_eq!(set_pos_count, 1 + usize::from(MAX_REAPPLY));
}

#[test]
fn game_that_moves_back_once_is_re_applied_and_accepted() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.wm.lock().window_mut(GAME).rect = Rect::new(0, 0, 1920, 1080);
    h.verify_now();
    assert_eq!(h.game().rect, TARGET);
    h.verify_now();
    h.verify_now();
    let status = h.sink.last_status();
    assert_eq!(status.degraded, None);
    assert_eq!(status.verify_attempts, 1);
}

#[test]
fn refit_moves_the_window_and_resets_verification() {
    let mut state = desktop();
    state.window_mut(GAME).enforced_size = Some((2560, 1417));
    let mut h = Harness::new(state);
    h.toggle();
    for _ in 0..(MAX_REAPPLY + 1) {
        h.verify_now();
    }
    assert!(h.sink.last_status().degraded.is_some());

    h.wm.lock().window_mut(GAME).enforced_size = None;
    let mut settings = Settings::default();
    settings.profile.region = RegionSpec { halign: HAlign::Left, ..RegionSpec::default() };
    h.send(Msg::Settings(Box::new(settings)));
    let status = h.sink.last_status();
    let left = Rect::new(0, 0, 2560, 1440);
    assert_eq!(status.target, Some(left));
    assert_eq!(status.verify_attempts, 0);
    assert_eq!(status.degraded, None);
    assert_eq!(h.game().rect, left);
    assert_eq!(
        h.wm.lock().marker.as_ref().and_then(|m| m.window.as_ref()).map(|w| w.target),
        Some(left)
    );
}

#[test]
fn refused_styles_warn_once_without_a_retry_loop() {
    let mut state = desktop();
    state.window_mut(GAME).refuse_styles = true;
    let mut h = Harness::new(state);
    h.toggle();
    h.verify_now();
    h.verify_now();
    let status = h.sink.last_status();
    assert!(matches!(status.degraded, Some(AppError::StyleRefused { .. })));
    assert_eq!(status.verify_attempts, 0);
}

#[test]
fn dpi_unaware_game_on_a_scaled_monitor_warns() {
    let mut state = desktop();
    state.monitors[0].dpi = 144;
    state.window_mut(GAME).dpi_awareness = DpiAwareness::Unaware;
    let mut h = Harness::new(state);
    h.toggle();
    assert!(matches!(
        h.sink.last_status().degraded,
        Some(AppError::DpiMismatch { monitor_dpi: 144, .. })
    ));
}

#[test]
fn missing_monitor_falls_back_to_the_primary_with_a_notice() {
    let mut settings = Settings::default();
    settings.profile.region.monitor = r"\\.\DISPLAY7".into();
    let mut h = Harness::with_settings(desktop(), settings);
    h.toggle();
    assert_eq!(h.game().rect, TARGET);
    let notices = h.sink.notices.lock().unwrap().clone();
    assert!(matches!(notices[..], [Notice { error: Some(AppError::MonitorGone { .. }), .. }]));
}

#[test]
fn aspect_preset_on_the_secondary_monitor() {
    let mut settings = Settings::default();
    settings.profile.region = RegionSpec {
        monitor: r"\\.\DISPLAY2".into(),
        preset: Preset::Aspect { num: 4, den: 3 },
        ..RegionSpec::default()
    };
    let mut h = Harness::with_settings(desktop(), settings);
    h.toggle();
    assert_eq!(h.game().rect, Rect::new(-1680, 0, 1440, 1080));
    assert_eq!(h.sink.last_status().monitor.as_deref(), Some(r"\\.\DISPLAY2"));
}

// ---------------------------------------------------------------------------------------------
// Moves after the fit (fullscreen browser videos)
// ---------------------------------------------------------------------------------------------

const MONITOR: Rect = Rect::new(0, 0, 5120, 1440);

/// "Game" as a browser in HTML5 fullscreen: captionless and covering the whole monitor.
fn fullscreen_browser() -> FakeState {
    let mut state = desktop();
    let browser = state.window_mut(GAME);
    browser.class = "Chrome_WidgetWin_1".to_string();
    browser.style &= !DEFAULT_STYLE_MASK;
    browser.rect = MONITOR;
    browser.placement.normal = MONITOR;
    state
}

impl Harness {
    fn gen(&self) -> u64 {
        self.engine.session.as_ref().unwrap().gen
    }

    fn moved(&mut self) {
        let gen = self.gen();
        self.send(Msg::WinEvent { gen, event: EVENT_OBJECT_LOCATIONCHANGE, hwnd: GAME });
    }

    /// Pretends the window has held the target for `STABLE` already.
    fn age_hold(&mut self) {
        if let Some(session) = self.engine.session.as_mut() {
            if session.verify.held_since.is_some() {
                session.verify.held_since = Instant::now().checked_sub(STABLE);
            }
        }
    }
}

#[test]
fn browser_reset_after_activation_is_put_back_every_time() {
    let mut h = Harness::new(fullscreen_browser());
    h.toggle();
    for _ in 0..3 {
        h.verify_now();
    }
    for _ in 0..(MAX_REAPPLY * 2) {
        // Chromium's background-fullscreen reset when another window is activated.
        h.wm.lock().window_mut(GAME).rect = Rect::new(0, 0, 5120, 1439);
        h.age_hold();
        h.moved();
        assert_eq!(h.game().rect, TARGET);
        h.moved(); // our own set_pos echoes back as a move: confirms the hold
    }
    assert!(h.engine.is_fitted());
    assert_eq!(h.sink.last_status().degraded, None);
}

#[test]
fn a_move_that_lands_on_the_target_changes_nothing() {
    let mut h = Harness::new(fullscreen_browser());
    h.toggle();
    h.clear_calls();
    h.moved();
    assert!(h.calls().iter().all(|c| !matches!(c, Call::SetPos { .. })));
}

#[test]
fn window_that_fights_back_on_every_move_still_gives_up() {
    let mut state = desktop();
    state.window_mut(GAME).enforced_size = Some((2560, 1417));
    let mut h = Harness::new(state);
    h.toggle();
    h.clear_calls();
    for _ in 0..(MAX_REAPPLY * 3) {
        h.moved();
    }
    let set_pos_count = h.calls().iter().filter(|c| matches!(c, Call::SetPos { .. })).count();
    assert_eq!(set_pos_count, usize::from(MAX_REAPPLY));
    assert!(matches!(h.sink.last_status().degraded, Some(AppError::ResizeRefused { .. })));
    assert!(h.engine.is_fitted());
}

/// The re-apply budget refills only after the window held the target for `STABLE`; resets that
/// come faster share one budget, so a window and BorderFit can't trade moves forever.
#[test]
fn resets_faster_than_the_stable_hold_share_one_re_apply_budget() {
    let mut h = Harness::new(fullscreen_browser());
    h.toggle();
    for _ in 0..3 {
        h.verify_now();
    }
    h.clear_calls();
    for _ in 0..(MAX_REAPPLY * 2) {
        h.wm.lock().window_mut(GAME).rect = Rect::new(0, 0, 5120, 1439);
        h.moved();
        h.moved(); // the echo of our own set_pos: held, but only for an instant
    }
    let set_pos_count = h.calls().iter().filter(|c| matches!(c, Call::SetPos { .. })).count();
    assert_eq!(set_pos_count, usize::from(MAX_REAPPLY));
    assert!(matches!(h.sink.last_status().degraded, Some(AppError::ResizeRefused { .. })));
    assert!(h.engine.is_fitted());
}

#[test]
fn browser_leaving_fullscreen_is_released_not_restored() {
    let mut h = Harness::new(fullscreen_browser());
    h.toggle();
    assert!(!h.tray_visible(TRAY));
    h.clear_calls();

    // Esc in the video: Chrome writes its windowed style back, which (custom-drawn frame) has
    // the resize border but no caption bit, then its windowed rect.
    let windowed = Rect::new(200, 100, 1600, 900);
    {
        let mut wm = h.wm.lock();
        let browser = wm.window_mut(GAME);
        browser.style |= WS_THICKFRAME;
        assert_eq!(browser.style & WS_CAPTION, 0);
        browser.rect = windowed;
    }
    h.moved();

    assert!(!h.engine.is_fitted());
    assert!(h.tray_visible(TRAY));
    assert!(!h.marker_present());
    assert_eq!(h.game().rect, windowed);
    let calls = h.calls();
    assert!(
        calls.iter().all(|c| !matches!(c, Call::SetPos { .. } | Call::SetStyles { .. })),
        "{calls:?}"
    );
    assert!(calls.contains(&Call::DwmSquare { hwnd: GAME, on: false }));
    assert_eq!(h.sink.last_status().state, FitState::Idle);
}

#[test]
fn browser_leaving_fullscreen_maximized_is_released() {
    let mut h = Harness::new(fullscreen_browser());
    h.toggle();
    {
        let mut wm = h.wm.lock();
        let browser = wm.window_mut(GAME);
        browser.style |= WS_MAXIMIZE | WS_THICKFRAME;
        browser.rect = Rect::new(-8, 32, 5136, 1416);
    }
    h.moved();
    assert!(!h.engine.is_fitted());
    assert!(h.tray_visible(TRAY));
}

#[test]
fn windowed_game_regaining_its_caption_is_fitted_again_not_released() {
    let mut h = Harness::new(desktop());
    h.toggle();
    h.wm.lock().window_mut(GAME).style |= DEFAULT_STYLE_MASK;
    h.moved();
    assert!(h.engine.is_fitted());
    assert_eq!(h.game().style & DEFAULT_STYLE_MASK, 0);
}

// ---------------------------------------------------------------------------------------------
// Events, shutdown, emergency, adoption
// ---------------------------------------------------------------------------------------------

#[test]
fn stale_win_events_are_dropped() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let gen = h.engine.session.as_ref().unwrap().gen;
    h.wm.lock().windows.remove(&GAME);
    h.send(Msg::WinEvent { gen: gen + 1, event: EVENT_OBJECT_DESTROY, hwnd: GAME });
    assert!(h.engine.is_fitted());
    h.send(Msg::WinEvent { gen, event: EVENT_OBJECT_DESTROY, hwnd: GAME });
    assert!(!h.engine.is_fitted());
}

#[test]
fn hide_event_without_corroboration_keeps_tracking() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let gen = h.engine.session.as_ref().unwrap().gen;
    h.wm.lock().window_mut(GAME).visible = false;
    h.send(Msg::WinEvent { gen, event: EVENT_OBJECT_HIDE, hwnd: GAME });
    assert!(h.engine.is_fitted());
    assert!(h.tray_visible(TRAY), "a hidden target shows the taskbar");
}

#[test]
fn shutdown_restores_then_acknowledges() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    assert!(!h.send(Msg::Shutdown { done: tx }));
    assert!(rx.try_recv().is_ok());
    assert_eq!(h.game().rect, ORIGINAL);
    assert!(h.tray_visible(TRAY));
}

#[test]
fn dropping_the_engine_restores() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let wm = h.wm.clone();
    drop(h);
    let s = wm.lock();
    assert_eq!(s.window(GAME).rect, ORIGINAL);
    assert!(s.tray(TRAY).visible);
    assert!(s.marker.is_none());
}

#[test]
fn restore_request_is_acknowledged_even_when_idle() {
    let mut h = Harness::new(desktop());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    h.send(Msg::Restore { done: Some(tx) });
    assert!(rx.try_recv().is_ok());
    assert!(h.calls().is_empty());
}

#[test]
fn engine_stands_down_after_an_emergency_and_fits_again_later() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let store = FakeMarkerStore::new(&h.wm);
    recovery::emergency_restore_from_marker(&h.wm, &store, h.emergency);
    assert_eq!(h.game().rect, ORIGINAL);
    h.clear_calls();

    h.engine.tick();
    assert!(!h.engine.is_fitted());
    assert!(h.calls().is_empty(), "standing down writes nothing: {:?}", h.calls());
    assert!(!h.emergency.load(Ordering::SeqCst));

    h.toggle();
    assert!(h.engine.is_fitted());
    assert_eq!(h.game().rect, TARGET);
}

#[test]
fn adopted_window_is_tracked_and_restored_later() {
    let mut state = desktop();
    let original = state.window(GAME).clone();
    let created = state.processes[&GAME_PID].created;
    let game = state.window_mut(GAME);
    game.style &= !DEFAULT_STYLE_MASK;
    game.rect = TARGET;
    let tracked = original.tracked(created, TARGET);
    let mut h = Harness::new(state);
    h.engine.adopt(Adopted { tracked, taskbar_hidden: true });
    assert!(h.engine.is_fitted());
    assert!(!h.tray_visible(TRAY), "the tick re-hides the taskbar while the game is visible");
    assert!(h.marker_present());
    assert_eq!(h.wm.lock().marker.as_ref().map(|m| m.pid), Some(OWN_PID));
    assert!(h.calls().iter().all(|c| !matches!(c, Call::SetPos { hwnd: GAME, .. })));

    h.toggle();
    assert_eq!(h.game().rect, ORIGINAL);
    assert_eq!(h.game().style, original.style);
    assert!(h.tray_visible(TRAY));
    assert_eq!(h.sink.last_status().restore, None);
}

#[test]
fn adopting_a_window_whose_process_died_restores_the_taskbar() {
    let mut state = desktop();
    state.trays[0].visible = false;
    let tracked = TrackedWindow { hwnd: GAME, pid: 4242, process_created: 1, ..Default::default() };
    let mut h = Harness::new(state);
    h.engine.adopt(Adopted { tracked, taskbar_hidden: true });
    assert!(!h.engine.is_fitted());
    assert!(h.tray_visible(TRAY));
}

#[test]
fn status_describes_the_restore_target() {
    let mut h = Harness::new(desktop());
    h.toggle();
    let restore = h.sink.last_status().restore.unwrap();
    assert_eq!(restore.rect, ORIGINAL);
    assert_eq!(restore.show, ShowState::Normal);
    assert!(restore.had_borders);
}
