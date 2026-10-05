//! Pure placement math: which monitor, what size, where. No Win32, fully unit-tested.

use crate::model::{AppError, HAlign, MonitorInfo, Preset, Rect, RegionSpec, VAlign};

/// Smallest width/height BorderFit will fit a window to.
pub const MIN_SIZE: i32 = 160;
/// Largest coordinate or size accepted from settings or the UI.
pub const MAX_COORD: i32 = 16384;
/// A region must cover at least this share of its own area on the chosen monitor.
const MIN_OVERLAP_PERCENT: i64 = 25;

/// A resolved region: the absolute rect plus the monitor it was resolved against.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub rect: Rect,
    pub monitor: MonitorInfo,
    /// Set when the configured monitor is missing and the primary was used instead.
    pub fallback: Option<AppError>,
}

/// The primary monitor, or the first one if none is flagged primary.
pub fn primary(monitors: &[MonitorInfo]) -> Option<&MonitorInfo> {
    monitors.iter().find(|m| m.primary).or_else(|| monitors.first())
}

/// The monitor with this device name; an empty name means the primary.
pub fn pick_monitor<'a>(monitors: &'a [MonitorInfo], device: &str) -> Option<&'a MonitorInfo> {
    if device.is_empty() {
        return primary(monitors);
    }
    monitors.iter().find(|m| m.device.eq_ignore_ascii_case(device))
}

/// The monitor whose rect contains the point, if any.
pub fn monitor_at(monitors: &[MonitorInfo], x: i32, y: i32) -> Option<&MonitorInfo> {
    monitors.iter().find(|m| m.rect.contains_point(x, y))
}

/// Width and height a preset asks for on this monitor.
pub fn preset_size(mon: Rect, preset: &Preset) -> Result<(i32, i32), AppError> {
    match *preset {
        Preset::Fixed { w, h } => {
            check_size(w, h)?;
            Ok((w, h))
        }
        Preset::Aspect { num, den } => aspect_size(mon, num, den),
        Preset::Fill => Ok((mon.w, mon.h)),
    }
}

/// Full monitor height at `num:den`; if that is wider than the monitor, full width instead.
fn aspect_size(mon: Rect, num: u32, den: u32) -> Result<(i32, i32), AppError> {
    check_aspect(num, den)?;
    let (num, den) = (i64::from(num), i64::from(den));
    let mut h = i64::from(mon.h);
    let mut w = round_div(h * num, den);
    if w > i64::from(mon.w) {
        w = i64::from(mon.w);
        h = round_div(w * den, num);
    }
    // Both values are bounded by the monitor size, so they fit in i32.
    Ok((w as i32, h as i32))
}

/// Integer division rounding half up, for non-negative operands.
fn round_div(numerator: i64, denominator: i64) -> i64 {
    (2 * numerator + denominator) / (2 * denominator)
}

/// Rejects an aspect ratio with a zero term.
pub fn check_aspect(num: u32, den: u32) -> Result<(), AppError> {
    if num == 0 || den == 0 {
        return Err(AppError::settings(format!("Invalid aspect ratio {num}:{den}.")));
    }
    Ok(())
}

/// Rejects a width or height below `MIN_SIZE` or above `MAX_COORD`.
pub fn check_size(w: i32, h: i32) -> Result<(), AppError> {
    if w < MIN_SIZE || h < MIN_SIZE {
        return Err(AppError::settings(format!(
            "The size must be at least {MIN_SIZE}×{MIN_SIZE} pixels."
        )));
    }
    if w > MAX_COORD || h > MAX_COORD {
        return Err(AppError::settings(format!(
            "The size must be at most {MAX_COORD}×{MAX_COORD} pixels."
        )));
    }
    Ok(())
}

/// Offset of a span of `size` inside `room` for the given alignment. Floor division, so an odd
/// remainder goes right/bottom exactly like the AHK script.
fn align_offset(room: i32, size: i32, start: bool, end: bool) -> i32 {
    if start {
        0
    } else if end {
        room - size
    } else {
        (room - size).div_euclid(2)
    }
}

/// Absolute rect for `spec` on `mon`, validated but never clamped.
pub fn resolve(mon: &MonitorInfo, spec: &RegionSpec) -> Result<Rect, AppError> {
    let rect = match spec.override_rect {
        Some(o) => {
            check_size(o.w, o.h)?;
            Rect::new(mon.rect.x + o.x, mon.rect.y + o.y, o.w, o.h)
        }
        None => {
            let (w, h) = preset_size(mon.rect, &spec.preset)?;
            let dx = align_offset(
                mon.rect.w,
                w,
                spec.halign == HAlign::Left,
                spec.halign == HAlign::Right,
            );
            let dy = align_offset(
                mon.rect.h,
                h,
                spec.valign == VAlign::Top,
                spec.valign == VAlign::Bottom,
            );
            Rect::new(mon.rect.x + dx + spec.offset_x, mon.rect.y + dy + spec.offset_y, w, h)
        }
    };
    validate(mon, rect)
}

/// Rejects rects outside the coordinate range or covering < 25% of their area on `mon`.
pub fn validate(mon: &MonitorInfo, rect: Rect) -> Result<Rect, AppError> {
    let in_range = |v: i32| (-MAX_COORD..=MAX_COORD).contains(&v);
    if !(in_range(rect.x) && in_range(rect.y) && in_range(rect.right()) && in_range(rect.bottom()))
    {
        return Err(offscreen(mon, rect));
    }
    if rect.overlap_area(&mon.rect) * 100 < rect.area() * MIN_OVERLAP_PERCENT {
        return Err(offscreen(mon, rect));
    }
    Ok(rect)
}

fn offscreen(mon: &MonitorInfo, rect: Rect) -> AppError {
    AppError::RegionOffscreen { rect, monitor: mon.device.clone() }
}

/// Picks the configured monitor (falling back to the primary) and resolves the region on it.
pub fn resolve_on(monitors: &[MonitorInfo], spec: &RegionSpec) -> Result<Resolved, AppError> {
    let (monitor, fallback) = match pick_monitor(monitors, &spec.monitor) {
        Some(m) => (m, None),
        None => {
            let m =
                primary(monitors).ok_or(AppError::MonitorGone { device: spec.monitor.clone() })?;
            (m, Some(AppError::MonitorGone { device: spec.monitor.clone() }))
        }
    };
    // A hand-edited rect belongs to the configured monitor; on the fallback use the preset.
    let spec = if fallback.is_some() {
        RegionSpec { override_rect: None, ..spec.clone() }
    } else {
        spec.clone()
    };
    let rect = resolve(monitor, &spec)?;
    Ok(Resolved { rect, monitor: monitor.clone(), fallback })
}

/// First-run default: 2560x1440 centered on an ultrawide (wider than 2:1) that fits it, else Full.
pub fn default_preset(primary: &MonitorInfo) -> Preset {
    let r = primary.rect;
    let ultrawide = i64::from(r.w) > 2 * i64::from(r.h);
    if ultrawide && r.w >= 2560 && r.h >= 1440 {
        Preset::Fixed { w: 2560, h: 1440 }
    } else {
        Preset::Fill
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Preset::{Aspect, Fill, Fixed};

    pub(crate) fn monitor(device: &str, rect: Rect, primary: bool) -> MonitorInfo {
        MonitorInfo {
            device: device.to_string(),
            name: crate::model::display_name(device),
            rect,
            work: Rect::new(rect.x, rect.y, rect.w, rect.h - 48),
            dpi: 96,
            primary,
        }
    }

    /// An ultrawide setup: 5120x1440 primary with a 1920x1080 display on its left.
    pub(crate) fn dual_layout() -> Vec<MonitorInfo> {
        vec![
            monitor(r"\\.\DISPLAY1", Rect::new(0, 0, 5120, 1440), true),
            monitor(r"\\.\DISPLAY2", Rect::new(-1920, 0, 1920, 1080), false),
        ]
    }

    fn spec(preset: Preset, halign: HAlign) -> RegionSpec {
        RegionSpec { preset, halign, ..RegionSpec::default() }
    }

    fn ultrawide() -> MonitorInfo {
        monitor(r"\\.\DISPLAY1", Rect::new(0, 0, 5120, 1440), true)
    }

    #[test]
    fn centered_2560_on_5120() {
        let r = resolve(&ultrawide(), &spec(Fixed { w: 2560, h: 1440 }, HAlign::Center));
        assert_eq!(r, Ok(Rect::new(1280, 0, 2560, 1440)));
    }

    #[test]
    fn aspect_presets_at_monitor_height() {
        let mon = ultrawide().rect;
        assert_eq!(preset_size(mon, &Aspect { num: 16, den: 9 }), Ok((2560, 1440)));
        assert_eq!(preset_size(mon, &Aspect { num: 4, den: 3 }), Ok((1920, 1440)));
        assert_eq!(preset_size(mon, &Aspect { num: 16, den: 10 }), Ok((2304, 1440)));
    }

    #[test]
    fn aspect_wider_than_monitor_is_capped_at_its_width() {
        let qhd = Rect::new(0, 0, 2560, 1440);
        assert_eq!(preset_size(qhd, &Aspect { num: 21, den: 9 }), Ok((2560, 1097)));
    }

    #[test]
    fn fill_is_the_monitor() {
        let r = resolve(&ultrawide(), &spec(Fill, HAlign::Left));
        assert_eq!(r, Ok(Rect::new(0, 0, 5120, 1440)));
    }

    #[test]
    fn right_aligned_on_a_monitor_left_of_primary() {
        let mon = monitor(r"\\.\DISPLAY2", Rect::new(-2560, 0, 2560, 1440), false);
        let r = resolve(&mon, &spec(Fixed { w: 1920, h: 1080 }, HAlign::Right));
        assert_eq!(r, Ok(Rect::new(-1920, 180, 1920, 1080)));
    }

    #[test]
    fn odd_remainder_goes_right_and_down() {
        let mon = monitor(r"\\.\DISPLAY1", Rect::new(0, 0, 1921, 1081), true);
        let r = resolve(&mon, &spec(Fixed { w: 1600, h: 900 }, HAlign::Center)).unwrap();
        assert_eq!((r.x, r.y), (160, 90));
    }

    #[test]
    fn offsets_shift_the_aligned_rect() {
        let s = RegionSpec {
            offset_x: 10,
            offset_y: -5,
            ..spec(Fixed { w: 2560, h: 1400 }, HAlign::Center)
        };
        assert_eq!(resolve(&ultrawide(), &s), Ok(Rect::new(1290, 15, 2560, 1400)));
    }

    #[test]
    fn override_rect_is_monitor_relative_and_wins() {
        let mon = monitor(r"\\.\DISPLAY2", Rect::new(-2560, 0, 2560, 1440), false);
        let s = RegionSpec {
            override_rect: Some(Rect::new(100, 50, 1920, 1080)),
            ..spec(Fill, HAlign::Right)
        };
        assert_eq!(resolve(&mon, &s), Ok(Rect::new(-2460, 50, 1920, 1080)));
    }

    #[test]
    fn mostly_offscreen_is_rejected_not_clamped() {
        let s = RegionSpec {
            override_rect: Some(Rect::new(4800, 0, 2560, 1440)),
            ..RegionSpec::default()
        };
        assert!(matches!(resolve(&ultrawide(), &s), Err(AppError::RegionOffscreen { .. })));
        // 25% or more on the monitor is accepted.
        let s = RegionSpec {
            override_rect: Some(Rect::new(4480, 0, 2560, 1440)),
            ..RegionSpec::default()
        };
        assert_eq!(resolve(&ultrawide(), &s), Ok(Rect::new(4480, 0, 2560, 1440)));
    }

    #[test]
    fn tiny_or_huge_sizes_are_rejected() {
        assert!(resolve(&ultrawide(), &spec(Fixed { w: 100, h: 1440 }, HAlign::Center)).is_err());
        assert!(resolve(&ultrawide(), &spec(Fixed { w: 20000, h: 1440 }, HAlign::Center)).is_err());
        assert!(preset_size(ultrawide().rect, &Aspect { num: 16, den: 0 }).is_err());
    }

    #[test]
    fn missing_monitor_falls_back_to_primary_with_a_warning() {
        let s = RegionSpec {
            monitor: r"\\.\DISPLAY3".into(),
            override_rect: Some(Rect::new(0, 0, 1000, 1000)),
            ..spec(Fixed { w: 2560, h: 1440 }, HAlign::Center)
        };
        let resolved = resolve_on(&dual_layout(), &s).unwrap();
        assert_eq!(resolved.rect, Rect::new(1280, 0, 2560, 1440));
        assert_eq!(resolved.monitor.device, r"\\.\DISPLAY1");
        assert_eq!(
            resolved.fallback,
            Some(AppError::MonitorGone { device: r"\\.\DISPLAY3".into() })
        );
    }

    #[test]
    fn empty_monitor_means_primary() {
        assert_eq!(pick_monitor(&dual_layout(), "").unwrap().device, r"\\.\DISPLAY1");
        assert_eq!(pick_monitor(&dual_layout(), r"\\.\display2").unwrap().rect.x, -1920);
        assert_eq!(monitor_at(&dual_layout(), -5, 10).unwrap().device, r"\\.\DISPLAY2");
    }

    #[test]
    fn first_run_default_depends_on_the_primary_shape() {
        assert_eq!(default_preset(&ultrawide()), Fixed { w: 2560, h: 1440 });
        let flat = monitor(r"\\.\DISPLAY1", Rect::new(0, 0, 2560, 1440), true);
        assert_eq!(default_preset(&flat), Fill);
        let small_ultrawide = monitor(r"\\.\DISPLAY1", Rect::new(0, 0, 3840, 1080), true);
        assert_eq!(default_preset(&small_ultrawide), Fill);
    }
}
