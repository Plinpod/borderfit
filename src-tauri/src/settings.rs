//! `settings.json`: load with a migration ladder, validate, save atomically; plus the import
//! of the AutoHotkey script's `borderless_config.ini`.

use crate::hotkey;
use crate::model::{
    AppError, HAlign, ImportReport, MonitorInfo, Notice, Preset, Rect, RegionSpec, Settings,
    SETTINGS_SCHEMA_VERSION,
};
use crate::region;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE_NAME: &str = "settings.json";

/// Writes pretty JSON to `<path>.tmp`, fsyncs it, then renames it over `path`, so a crash
/// leaves either the old or the new file, never a torn one. Also used for the restore marker.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(value)?;
    let tmp = tmp_path(path);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// What `load` found.
#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// No settings file existed (first run).
    pub fresh: bool,
    /// Something the user should hear about (broken file moved aside, hotkey reset).
    pub notice: Option<Notice>,
}

/// Loads settings from `dir`, migrating and normalizing them. Never fails: a missing file
/// yields defaults; a broken one is moved aside (`settings.json.broken-<ts>`) with a notice.
pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Loaded { settings: Settings::default(), fresh: true, notice: None };
        }
        Err(e) => {
            log::error!("settings: read {} failed: {e}", path.display());
            let notice = Notice::info(format!("Settings could not be read ({e}); using defaults."));
            return Loaded { settings: Settings::default(), fresh: false, notice: Some(notice) };
        }
    };
    match parse(&text) {
        Ok((mut settings, migrated)) => {
            let notice = normalize_hotkey(&mut settings);
            if migrated || notice.is_some() {
                if let Err(e) = save(dir, &settings) {
                    log::error!("settings: save after migration failed: {e}");
                }
            }
            Loaded { settings, fresh: false, notice }
        }
        Err(e) => {
            let moved = move_aside(&path);
            log::error!("settings: {e}; moved to {moved:?}");
            let notice = Notice::info(
                "Settings were unreadable and have been reset. The old file was kept next to the new one.",
            );
            Loaded { settings: Settings::default(), fresh: false, notice: Some(notice) }
        }
    }
}

/// Parses and migrates settings JSON. Returns the settings and whether a migration ran.
pub fn parse(text: &str) -> Result<(Settings, bool), String> {
    let mut value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("settings.json is not a JSON object".into());
    }
    let from = value.get("schema_version").and_then(Value::as_u64).unwrap_or(0) as u32;
    if from > SETTINGS_SCHEMA_VERSION {
        log::warn!(
            "settings: schema {from} is newer than {SETTINGS_SCHEMA_VERSION}; reading what we know"
        );
    }
    for version in from..SETTINGS_SCHEMA_VERSION {
        value = migrate_step(version, value)?;
    }
    let settings = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok((settings, from < SETTINGS_SCHEMA_VERSION))
}

/// One rung of the migration ladder: `version` → `version + 1`.
fn migrate_step(version: u32, mut value: Value) -> Result<Value, String> {
    match version {
        // 0 = a file without schema_version; its fields already have the v1 shape.
        0 => {
            value["schema_version"] = Value::from(1);
            Ok(value)
        }
        _ => Err(format!("no migration from schema {version}")),
    }
}

/// Keeps a valid, normalized hotkey; resets an invalid one to the default with a notice.
fn normalize_hotkey(settings: &mut Settings) -> Option<Notice> {
    match hotkey::normalize(&settings.hotkey) {
        Ok(chord) => {
            settings.hotkey = chord;
            None
        }
        Err(e) => {
            log::warn!("settings: hotkey {:?} rejected ({e}); using the default", settings.hotkey);
            settings.hotkey = crate::model::DEFAULT_HOTKEY.to_string();
            Some(Notice::info(format!(
                "The saved hotkey was not valid, so it was reset to {}.",
                crate::model::DEFAULT_HOTKEY
            )))
        }
    }
}

fn move_aside(path: &Path) -> Option<PathBuf> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut name = path.file_name()?.to_os_string();
    name.push(format!(".broken-{stamp}"));
    let target = path.with_file_name(name);
    fs::rename(path, &target).ok().map(|()| target)
}

pub fn save(dir: &Path, settings: &Settings) -> io::Result<()> {
    write_json_atomic(&dir.join(FILE_NAME), settings)
}

/// Settings for a first run: the default region shape depends on the primary monitor.
pub fn first_run_defaults(monitors: &[MonitorInfo]) -> Settings {
    let mut settings = Settings::default();
    if let Some(primary) = region::primary(monitors) {
        settings.profile.region.preset = region::default_preset(primary);
    }
    settings
}

/// Checks settings coming from the UI before they are saved; returns them normalized.
pub fn validate(mut settings: Settings) -> Result<Settings, AppError> {
    settings.schema_version = SETTINGS_SCHEMA_VERSION;
    settings.hotkey = hotkey::normalize(&settings.hotkey)?;
    let region = &settings.profile.region;
    if let Some(r) = region.override_rect {
        check_rect(r)?;
    }
    match region.preset {
        Preset::Fixed { w, h } => region::check_size(w, h)?,
        Preset::Aspect { num, den } => region::check_aspect(num, den)?,
        Preset::Fill => {}
    }
    let offsets_ok =
        [region.offset_x, region.offset_y].iter().all(|v| v.abs() <= region::MAX_COORD);
    if !offsets_ok {
        return Err(AppError::settings("Offsets are out of range."));
    }
    Ok(settings)
}

/// A hand-edited or imported rect: a valid size at a position within the coordinate range.
fn check_rect(r: Rect) -> Result<(), AppError> {
    region::check_size(r.w, r.h)?;
    if r.x.abs() > region::MAX_COORD || r.y.abs() > region::MAX_COORD {
        return Err(AppError::settings("The position is out of range."));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// AutoHotkey INI import
// ---------------------------------------------------------------------------------------------

/// Reads the keys of the `[DefaultSection]` section (the only one the AHK script writes).
fn read_default_section(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line[1..line.len() - 1].trim().eq_ignore_ascii_case("DefaultSection");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    values
}

/// Keys the AHK script accepts but a global hotkey here cannot use.
fn ahk_key_unsupported(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "appskey" || lower == "lwin" || lower == "rwin" || lower.starts_with("browser_")
}

/// Builds new settings from the AHK script's INI (does not save; the UI confirms first).
/// The INI's absolute rect becomes a monitor-relative override on the monitor containing its
/// center (the primary as a fallback).
pub fn import_ahk_ini(
    text: &str,
    current: &Settings,
    monitors: &[MonitorInfo],
) -> Result<ImportReport, AppError> {
    let values = read_default_section(text);
    if values.is_empty() {
        return Err(AppError::settings(
            "No [DefaultSection] found; is this BorderlessGaming's borderless_config.ini?",
        ));
    }
    let mut settings = current.clone();
    let mut warnings = Vec::new();

    let number = |key: &str| -> Option<i32> { values.get(key).and_then(|v| v.parse().ok()) };
    let (x, y, w, h) =
        (number("xoffset"), number("yoffset"), number("reswidth"), number("resheight"));
    let rect = match (x, y, w, h) {
        (Some(x), Some(y), Some(w), Some(h)) => Some(Rect::new(x, y, w, h)),
        _ => {
            warnings.push("The INI has no complete position and size; the region was kept.".into());
            None
        }
    };
    if let Some(rect) = rect {
        check_rect(rect)?;
        let (cx, cy) = rect.center();
        let monitor = region::monitor_at(monitors, cx, cy)
            .or_else(|| region::primary(monitors))
            .ok_or_else(|| AppError::settings("No monitors found."))?;
        settings.profile.region = region_from_ini_rect(rect, monitor);
    }

    if let Some(hide) = values.get("hidetaskbar") {
        settings.profile.taskbar.hide_taskbar = matches!(hide.as_str(), "1" | "true" | "True");
    }

    if let Some(raw) = values.get("mainhotkey") {
        import_hotkey(raw, &mut settings, &mut warnings);
    }

    let summary = import_summary(&settings, rect);
    Ok(ImportReport { settings, warnings, summary })
}

/// A size chip plus alignment when the INI rect is exactly that (so the UI shows "2560×1440,
/// Center" rather than "custom"), otherwise a monitor-relative override.
fn region_from_ini_rect(rect: Rect, monitor: &MonitorInfo) -> RegionSpec {
    let device = if monitor.primary { String::new() } else { monitor.device.clone() };
    let preset = Preset::Fixed { w: rect.w, h: rect.h };
    for halign in [HAlign::Center, HAlign::Left, HAlign::Right] {
        let spec = RegionSpec { monitor: device.clone(), preset, halign, ..RegionSpec::default() };
        if region::resolve(monitor, &spec) == Ok(rect) {
            return spec;
        }
    }
    RegionSpec {
        monitor: device,
        preset,
        override_rect: Some(Rect::new(
            rect.x - monitor.rect.x,
            rect.y - monitor.rect.y,
            rect.w,
            rect.h,
        )),
        ..RegionSpec::default()
    }
}

fn import_hotkey(raw: &str, settings: &mut Settings, warnings: &mut Vec<String>) {
    let name = raw.trim();
    if ahk_key_unsupported(name) {
        warnings.push(format!("The hotkey {name} can't be used here; kept {}.", settings.hotkey));
        return;
    }
    // `normalize` also understands AHK's names (PgUp, Del, BS, Break, NumpadDot, ...).
    match hotkey::normalize(name) {
        Ok(chord) => {
            if chord == "F12" {
                settings.legacy_f12_ack = false;
                warnings.push(
                    "F12 is Steam's screenshot key and reserved by Windows for debuggers; consider another key.".into(),
                );
            }
            settings.hotkey = chord;
        }
        Err(e) => warnings
            .push(format!("The hotkey {name} was not imported: {e} Kept {}.", settings.hotkey)),
    }
}

fn import_summary(settings: &Settings, rect: Option<Rect>) -> String {
    let mut parts = Vec::new();
    if let Some(r) = rect {
        parts.push(format!("{}×{} at {},{}", r.w, r.h, r.x, r.y));
    }
    let taskbar = settings.profile.taskbar.hide_taskbar;
    parts.push(if taskbar { "taskbar hidden".into() } else { "taskbar shown".into() });
    parts.push(format!("hotkey {}", settings.hotkey));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DEFAULT_HOTKEY;
    use crate::region::tests::dual_layout;
    use crate::test_support::TempDir;

    #[test]
    fn roundtrip() {
        let dir = TempDir::new("roundtrip");
        let mut settings = Settings { hotkey: "F9".into(), ..Settings::default() };
        settings.profile.region.override_rect = Some(Rect::new(1, 2, 300, 400));
        save(&dir, &settings).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.settings, settings);
        assert!(!loaded.fresh && loaded.notice.is_none());
        assert!(!dir.join("settings.json.tmp").exists());
    }

    #[test]
    fn missing_file_is_a_first_run() {
        let loaded = load(&TempDir::new("missing"));
        assert!(loaded.fresh);
        assert_eq!(loaded.settings, Settings::default());
    }

    #[test]
    fn file_without_schema_version_is_migrated_and_saved() {
        let dir = TempDir::new("migrate");
        fs::write(dir.join(FILE_NAME), r#"{ "hotkey": "ctrl + shift + keyf", "future": true }"#)
            .unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.settings.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(loaded.settings.hotkey, "Ctrl+Shift+F");
        let saved = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(saved.contains("\"schema_version\": 1"));
    }

    #[test]
    fn broken_file_is_moved_aside_with_a_notice() {
        let dir = TempDir::new("broken");
        fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.notice.is_some());
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.starts_with("settings.json.broken-")), "{names:?}");
    }

    #[test]
    fn invalid_hotkey_is_reset_with_a_notice() {
        let dir = TempDir::new("badkey");
        fs::write(dir.join(FILE_NAME), r#"{ "schema_version": 1, "hotkey": "A" }"#).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.settings.hotkey, DEFAULT_HOTKEY);
        assert!(loaded.notice.is_some());
    }

    #[test]
    fn atomic_write_replaces_the_file() {
        let dir = TempDir::new("atomic");
        let path = dir.join("x.json");
        write_json_atomic(&path, &1).unwrap();
        write_json_atomic(&path, &2).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "2");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn validate_rejects_bad_values() {
        let mut s = Settings::default();
        s.profile.region.override_rect = Some(Rect::new(0, 0, 100, 100));
        assert!(validate(s).is_err());
        let mut s = Settings::default();
        s.profile.region.preset = Preset::Fixed { w: 20_000, h: 1440 };
        assert!(validate(s).is_err(), "a size the fit would refuse must not be saved");
        let mut s = Settings::default();
        s.profile.region.preset = Preset::Aspect { num: 16, den: 0 };
        assert!(validate(s).is_err());
        let s = Settings { hotkey: "Q".into(), ..Settings::default() };
        assert!(validate(s).is_err());
        let s = Settings { hotkey: "alt+f10".into(), ..Settings::default() };
        assert_eq!(validate(s).unwrap().hotkey, "Alt+F10");
    }

    #[test]
    fn first_run_defaults_follow_the_primary() {
        let s = first_run_defaults(&dual_layout());
        assert_eq!(s.profile.region.preset, Preset::Fixed { w: 2560, h: 1440 });
    }

    const ULTRAWIDE_INI: &str = "[DefaultSection]\r\nXOffset=1280\r\nYOffset=0\r\nResWidth=2560\r\nResHeight=1440 \r\nHideTaskbar=1\r\nMainHotkey=F12\r\n";

    #[test]
    fn imports_the_ahk_ini_on_the_primary() {
        let report = import_ahk_ini(ULTRAWIDE_INI, &Settings::default(), &dual_layout()).unwrap();
        let region = &report.settings.profile.region;
        assert_eq!(region.monitor, "");
        // Exactly "2560×1440 centered", so it becomes the chip, not a custom rect.
        assert_eq!(region.override_rect, None);
        assert_eq!(region.halign, HAlign::Center);
        assert_eq!(region.preset, Preset::Fixed { w: 2560, h: 1440 });
        assert!(report.settings.profile.taskbar.hide_taskbar);
        assert_eq!(report.settings.hotkey, "F12");
        assert!(!report.settings.legacy_f12_ack);
        assert_eq!(report.warnings.len(), 1, "F12 warning");
        assert_eq!(report.summary, "2560×1440 at 1280,0, taskbar hidden, hotkey F12");
    }

    #[test]
    fn ini_rect_centred_on_the_secondary_becomes_relative_to_it() {
        let ini = "[DefaultSection]\nXOffset=-1920\nYOffset=0\nResWidth=1600\nResHeight=900\nHideTaskbar=0\nMainHotkey=PgUp\n";
        let report = import_ahk_ini(ini, &Settings::default(), &dual_layout()).unwrap();
        let region = &report.settings.profile.region;
        assert_eq!(region.monitor, r"\\.\DISPLAY2");
        assert_eq!(region.override_rect, Some(Rect::new(0, 0, 1600, 900)));
        assert!(!report.settings.profile.taskbar.hide_taskbar);
        // A bare PageUp is not allowed, so the current hotkey is kept with a warning.
        assert_eq!(report.settings.hotkey, DEFAULT_HOTKEY);
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn unsupported_ahk_keys_keep_the_current_hotkey() {
        let ini = "[DefaultSection]\nMainHotkey=AppsKey\n";
        let report = import_ahk_ini(ini, &Settings::default(), &dual_layout()).unwrap();
        assert_eq!(report.settings.hotkey, DEFAULT_HOTKEY);
        assert!(report.warnings.iter().any(|w| w.contains("AppsKey")));
    }

    #[test]
    fn ahk_names_are_mapped() {
        let ini = "[DefaultSection]\nMainHotkey=Break\n";
        let report = import_ahk_ini(ini, &Settings::default(), &dual_layout()).unwrap();
        assert_eq!(report.settings.hotkey, "Pause");
    }

    #[test]
    fn a_file_without_the_section_is_rejected() {
        assert!(import_ahk_ini("[Other]\nx=1\n", &Settings::default(), &dual_layout()).is_err());
    }
}
