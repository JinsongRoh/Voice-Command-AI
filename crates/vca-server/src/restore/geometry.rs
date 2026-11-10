//! Pure window geometry: where a saved window goes on the monitors of today.
//!
//! All coordinates are physical pixels of the virtual screen and may be
//! negative. A saved rectangle is kept exactly while it is usable: snapped
//! Windows Terminal windows stick out of the monitor by their invisible
//! resize borders, so "sticks out a little" is not "off screen".

use vca_core::{RestoreMonitor, WindowRect};

/// A window counts as visible when this much of it is on a monitor: enough
/// to grab the title bar.
const MIN_VISIBLE_W: i64 = 120;
const MIN_VISIBLE_H: i64 = 40;
/// The DPI of 100 % scaling.
const BASE_DPI: u32 = 96;
/// Smallest usable window.
const MIN_W: i32 = 320;
const MIN_H: i32 = 200;

/// How [`resolve_rect`] arrived at the final rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Exactly as saved.
    Exact,
    /// The saved monitor moved or changed size: same place on it.
    Relative,
    /// Off every monitor: moved onto the nearest one.
    Clamped,
}

fn overlap(a: &WindowRect, b: &WindowRect) -> (i64, i64) {
    let w = i64::from((a.x + a.w).min(b.x + b.w)) - i64::from(a.x.max(b.x));
    let h = i64::from((a.y + a.h).min(b.y + b.h)) - i64::from(a.y.max(b.y));
    (w.max(0), h.max(0))
}

/// The monitor holding the largest part of `rect`.
pub fn monitor_for_rect<'a>(
    rect: &WindowRect,
    monitors: &'a [RestoreMonitor],
) -> Option<&'a RestoreMonitor> {
    monitors
        .iter()
        .map(|m| {
            let (w, h) = overlap(rect, &m.rect);
            (w * h, m)
        })
        .filter(|(area, _)| *area > 0)
        .max_by_key(|(area, _)| *area)
        .map(|(_, m)| m)
}

/// True when enough of `rect` is on some monitor to reach it with the mouse.
pub fn is_reachable(rect: &WindowRect, monitors: &[RestoreMonitor]) -> bool {
    monitors.iter().any(|m| {
        let (w, h) = overlap(rect, &m.rect);
        w >= MIN_VISIBLE_W && h >= MIN_VISIBLE_H
    })
}

fn center(r: &WindowRect) -> (i64, i64) {
    (
        i64::from(r.x) + i64::from(r.w) / 2,
        i64::from(r.y) + i64::from(r.h) / 2,
    )
}

/// The monitor whose center is closest to the center of `rect`.
pub fn nearest_monitor<'a>(
    rect: &WindowRect,
    monitors: &'a [RestoreMonitor],
) -> Option<&'a RestoreMonitor> {
    let (cx, cy) = center(rect);
    monitors.iter().min_by_key(|m| {
        let (mx, my) = center(&m.rect);
        (mx - cx).pow(2) + (my - cy).pow(2)
    })
}

/// Fits `rect` inside `area`: size first, then position.
pub fn clamp_into(rect: &WindowRect, area: &WindowRect) -> WindowRect {
    let w = rect.w.clamp(MIN_W.min(area.w), area.w.max(1));
    let h = rect.h.clamp(MIN_H.min(area.h), area.h.max(1));
    WindowRect {
        x: rect.x.clamp(area.x, area.x + area.w - w),
        y: rect.y.clamp(area.y, area.y + area.h - h),
        w,
        h,
    }
}

/// Converts a size measured at `from_dpi` for a monitor at `to_dpi`, so the
/// window keeps its size on screen (`from_dpi` 0 means "unknown").
pub fn scale_size(rect: &WindowRect, from_dpi: u32, to_dpi: u32) -> WindowRect {
    if from_dpi == 0 || to_dpi == 0 || from_dpi == to_dpi {
        return *rect;
    }
    let scale = |v: i32| (i64::from(v) * i64::from(to_dpi) / i64::from(from_dpi)) as i32;
    WindowRect {
        w: scale(rect.w),
        h: scale(rect.h),
        ..*rect
    }
}

/// Keeps the window at the same relative place when its monitor changed
/// position, resolution or scaling.
fn follow_monitor(
    rect: &WindowRect,
    saved: &RestoreMonitor,
    now: &RestoreMonitor,
) -> Option<WindowRect> {
    if saved.rect == now.rect && saved.dpi == now.dpi {
        return None;
    }
    let ratio = |value: i32, to: i32, from: i32| {
        (i64::from(value) * i64::from(to) / i64::from(from.max(1))) as i32
    };
    let offset_x = rect.x - saved.rect.x;
    let offset_y = rect.y - saved.rect.y;
    let moved = if saved.rect.w == now.rect.w && saved.rect.h == now.rect.h {
        scale_size(rect, saved.dpi, now.dpi)
    } else {
        // Another resolution: keep the share of the monitor.
        WindowRect {
            w: ratio(rect.w, now.rect.w, saved.rect.w),
            h: ratio(rect.h, now.rect.h, saved.rect.h),
            ..*rect
        }
    };
    Some(WindowRect {
        x: now.rect.x + ratio(offset_x, now.rect.w, saved.rect.w),
        y: now.rect.y + ratio(offset_y, now.rect.h, saved.rect.h),
        ..moved
    })
}

/// Where a saved window goes now. A window off every monitor lands on the
/// nearest one, sized for that monitor's DPI.
pub fn resolve_rect(
    rect: &WindowRect,
    saved: Option<&RestoreMonitor>,
    monitors: &[RestoreMonitor],
) -> (WindowRect, Placement) {
    if monitors.is_empty() {
        return (*rect, Placement::Exact);
    }
    let followed = saved.and_then(|s| {
        let now = monitors.iter().find(|m| m.id == s.id)?;
        follow_monitor(rect, s, now)
    });
    let (candidate, placement) = match followed {
        Some(r) => (r, Placement::Relative),
        None => (*rect, Placement::Exact),
    };
    if is_reachable(&candidate, monitors) {
        return (candidate, placement);
    }
    let Some(target) = nearest_monitor(&candidate, monitors) else {
        return (candidate, placement);
    };
    let from_dpi = saved.map_or(0, |s| s.dpi);
    let sized = scale_size(&candidate, from_dpi, target.dpi);
    (clamp_into(&sized, &target.work), Placement::Clamped)
}

/// `dpi` as a percentage of 100 % scaling.
pub fn scale_percent(dpi: u32) -> u32 {
    dpi * 100 / BASE_DPI
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> WindowRect {
        WindowRect { x, y, w, h }
    }

    fn monitor(id: &str, r: WindowRect, dpi: u32) -> RestoreMonitor {
        RestoreMonitor {
            id: id.into(),
            rect: r,
            work: WindowRect { h: r.h - 60, ..r },
            dpi,
        }
    }

    /// The owner's layout: four monitors, 150 %, some at negative coordinates.
    fn four() -> Vec<RestoreMonitor> {
        vec![
            monitor("A", rect(0, 0, 1920, 1080), 144),
            monitor("B", rect(1920, 0, 1920, 1080), 144),
            monitor("C", rect(-1920, 0, 1920, 1080), 144),
            monitor("D", rect(1920, -2160, 1920, 2160), 144),
        ]
    }

    #[test]
    fn a_snapped_window_that_sticks_out_stays_exact() {
        // Measured values: 9 px of invisible border on each side.
        let saved = rect(-1929, 0, 1938, 1053);
        let (out, how) = resolve_rect(&saved, None, &four());
        assert_eq!((out, how), (saved, Placement::Exact));
        let above = rect(1899, -2160, 1938, 1053);
        assert_eq!(
            resolve_rect(&above, None, &four()),
            (above, Placement::Exact)
        );
    }

    #[test]
    fn off_screen_rect_is_clamped_to_the_nearest_monitor() {
        // The right monitor is gone.
        let monitors = vec![monitor("A", rect(0, 0, 1920, 1080), 144)];
        let saved = rect(3831, 1033, 1938, 1053);
        let (out, how) = resolve_rect(&saved, None, &monitors);
        assert_eq!(how, Placement::Clamped);
        assert!(is_reachable(&out, &monitors));
        assert_eq!(out.w, 1920);
        assert_eq!(out.h, 1020);
        assert_eq!((out.x, out.y), (0, 0));
    }

    #[test]
    fn a_sliver_on_a_monitor_is_not_reachable() {
        let monitors = four();
        assert!(!is_reachable(&rect(3830, 0, 1000, 600), &monitors));
        assert!(is_reachable(&rect(3700, 0, 1000, 600), &monitors));
    }

    #[test]
    fn a_moved_monitor_keeps_the_relative_place() {
        let saved_mon = monitor("B", rect(1920, 0, 1920, 1080), 144);
        let now = vec![
            monitor("A", rect(0, 0, 1920, 1080), 144),
            monitor("B", rect(-1920, 0, 1920, 1080), 144),
        ];
        let saved = rect(2000, 100, 800, 600);
        let (out, how) = resolve_rect(&saved, Some(&saved_mon), &now);
        assert_eq!(how, Placement::Relative);
        assert_eq!(out, rect(-1840, 100, 800, 600));
    }

    #[test]
    fn a_new_resolution_keeps_the_share_of_the_monitor() {
        let saved_mon = monitor("A", rect(0, 0, 1920, 1080), 96);
        let now = vec![monitor("A", rect(0, 0, 3840, 2160), 96)];
        let (out, how) = resolve_rect(&rect(960, 0, 960, 1080), Some(&saved_mon), &now);
        assert_eq!(how, Placement::Relative);
        assert_eq!(out, rect(1920, 0, 1920, 2160));
    }

    #[test]
    fn a_new_scaling_keeps_the_size_on_screen() {
        let saved_mon = monitor("A", rect(0, 0, 3840, 2160), 96);
        let now = vec![monitor("A", rect(0, 0, 3840, 2160), 144)];
        let (out, how) = resolve_rect(&rect(100, 100, 1000, 800), Some(&saved_mon), &now);
        assert_eq!(how, Placement::Relative);
        assert_eq!(out, rect(100, 100, 1500, 1200));
    }

    #[test]
    fn clamped_windows_take_the_dpi_of_their_new_monitor() {
        let saved_mon = monitor("gone", rect(5000, 0, 1920, 1080), 96);
        let now = vec![monitor("A", rect(0, 0, 3840, 2160), 192)];
        let (out, how) = resolve_rect(&rect(5100, 50, 800, 600), Some(&saved_mon), &now);
        assert_eq!(how, Placement::Clamped);
        assert_eq!((out.w, out.h), (1600, 1200));
    }

    #[test]
    fn no_monitors_keeps_the_rect() {
        let r = rect(-5000, -5000, 100, 100);
        assert_eq!(resolve_rect(&r, None, &[]), (r, Placement::Exact));
    }

    #[test]
    fn scale_size_ignores_unknown_dpi() {
        let r = rect(0, 0, 100, 100);
        assert_eq!(scale_size(&r, 0, 144), r);
        assert_eq!(scale_size(&r, 144, 144), r);
        assert_eq!(scale_size(&r, 96, 144).w, 150);
        assert_eq!(scale_percent(144), 150);
    }

    #[test]
    fn monitor_for_rect_picks_the_largest_overlap() {
        let monitors = four();
        let m = monitor_for_rect(&rect(1800, 0, 400, 300), &monitors).unwrap();
        assert_eq!(m.id, "B");
        assert!(monitor_for_rect(&rect(9000, 9000, 10, 10), &monitors).is_none());
    }
}
