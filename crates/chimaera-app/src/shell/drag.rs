//! Coordinate math for cross-window drags and detached-window placement.
//!
//! The webview reports pointer positions in CLIENT coords (CSS px) only —
//! its own screenX/screenY are not trusted (they disagree across engines and
//! monitor mixes). The shell owns every window's position/size/scale, so it
//! is the one place client coords can be lifted into a common screen space.
//!
//! That space is platform-dependent: on macOS the OS-global coordinate
//! system is LOGICAL (points; CSS px == logical px, and per-monitor scaling
//! is a backing-store concern), while on Windows/Linux it is PHYSICAL
//! device pixels (per-monitor DPI changes the logical size of the same
//! physical rect). All math here is pure and unit-tested for both spaces;
//! the `#[cfg]` wrappers pick the right one for the running platform.

/// One window's rect in the platform's global drag space, plus its scale
/// factor (needed only where the space is physical).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WinRect {
    pub(crate) label: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
    pub(crate) scale: f64,
}

/// A live window's rect in the platform's global drag space. `inner_*` (not
/// `outer_*`): client coords are relative to the webview's content area, and
/// on the macOS overlay titlebar the two coincide anyway.
pub(crate) fn rect_of(window: &tauri::WebviewWindow) -> Option<WinRect> {
    let scale = window.scale_factor().ok()?;
    let pos = window.inner_position().ok()?;
    let size = window.inner_size().ok()?;
    #[cfg(target_os = "macos")]
    {
        let p = pos.to_logical::<f64>(scale);
        let s = size.to_logical::<f64>(scale);
        Some(WinRect {
            label: window.label().to_string(),
            x: p.x,
            y: p.y,
            w: s.width,
            h: s.height,
            scale,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(WinRect {
            label: window.label().to_string(),
            x: f64::from(pos.x),
            y: f64::from(pos.y),
            w: f64::from(size.width),
            h: f64::from(size.height),
            scale,
        })
    }
}

// The `#[cfg]` wrappers below each call exactly ONE of these per platform,
// so its sibling is dead code there — but every variant is exercised by the
// (platform-independent) unit tests, which is the point: the math for BOTH
// spaces stays verified wherever the tests run.

/// Lift a window's client-space point into the LOGICAL global space
/// (macOS: client px are logical px, so this is a plain translation).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn global_of_client_logical(win: &WinRect, cx: f64, cy: f64) -> (f64, f64) {
    (win.x + cx, win.y + cy)
}

/// Lift a window's client-space point into the PHYSICAL global space
/// (Windows/Linux: client px scale by the window's own DPI factor).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn global_of_client_physical(win: &WinRect, cx: f64, cy: f64) -> (f64, f64) {
    (win.x + cx * win.scale, win.y + cy * win.scale)
}

/// Drop a global point back into a window's client space — the inverse of
/// `global_of_client_*`, used to hand a hovered target its own coordinates.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn client_of_global_logical(win: &WinRect, gx: f64, gy: f64) -> (f64, f64) {
    (gx - win.x, gy - win.y)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn client_of_global_physical(win: &WinRect, gx: f64, gy: f64) -> (f64, f64) {
    ((gx - win.x) / win.scale, (gy - win.y) / win.scale)
}

pub(crate) fn global_of_client(win: &WinRect, cx: f64, cy: f64) -> (f64, f64) {
    #[cfg(target_os = "macos")]
    {
        global_of_client_logical(win, cx, cy)
    }
    #[cfg(not(target_os = "macos"))]
    {
        global_of_client_physical(win, cx, cy)
    }
}

pub(crate) fn client_of_global(win: &WinRect, gx: f64, gy: f64) -> (f64, f64) {
    #[cfg(target_os = "macos")]
    {
        client_of_global_logical(win, gx, gy)
    }
    #[cfg(not(target_os = "macos"))]
    {
        client_of_global_physical(win, gx, gy)
    }
}

/// A global point as a LOGICAL position for the window registry (records
/// store logical px). In the logical space this is identity; in the physical
/// space it divides by the reporting window's scale — exact on that
/// window's monitor, best-effort across a mixed-DPI boundary.
pub(crate) fn logical_of_global(win: &WinRect, gx: f64, gy: f64) -> (f64, f64) {
    #[cfg(target_os = "macos")]
    {
        let _ = win;
        (gx, gy)
    }
    #[cfg(not(target_os = "macos"))]
    {
        (gx / win.scale, gy / win.scale)
    }
}

/// The window under a global point. Overlap is real (windows stack) and the
/// windowing API exposes no z-order, so `focus_order` (most-recently-focused
/// first) breaks ties: among containing rects the most recently focused
/// wins, and a rect absent from the order loses to any that is present.
pub(crate) fn hit_test<'a>(
    rects: &'a [WinRect],
    focus_order: &[String],
    gx: f64,
    gy: f64,
) -> Option<&'a WinRect> {
    let mut best: Option<(&WinRect, usize)> = None;
    for r in rects {
        if gx < r.x || gx > r.x + r.w || gy < r.y || gy > r.y + r.h {
            continue;
        }
        let rank = focus_order
            .iter()
            .position(|l| l == &r.label)
            .unwrap_or(usize::MAX);
        if best.is_none_or(|(_, b)| rank < b) {
            best = Some((r, rank));
        }
    }
    best.map(|(r, _)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(label: &str, x: f64, y: f64, w: f64, h: f64, scale: f64) -> WinRect {
        WinRect {
            label: label.to_string(),
            x,
            y,
            w,
            h,
            scale,
        }
    }

    #[test]
    fn logical_space_round_trips_client_points() {
        let win = rect("a", 100.0, 50.0, 800.0, 600.0, 2.0);
        let (gx, gy) = global_of_client_logical(&win, 40.0, 30.0);
        assert_eq!((gx, gy), (140.0, 80.0));
        assert_eq!(client_of_global_logical(&win, gx, gy), (40.0, 30.0));
    }

    #[test]
    fn physical_space_scales_client_points_per_window() {
        // A 2x window: client (40, 30) is physical (80, 60) from its origin.
        let win = rect("a", 100.0, 50.0, 1600.0, 1200.0, 2.0);
        let (gx, gy) = global_of_client_physical(&win, 40.0, 30.0);
        assert_eq!((gx, gy), (180.0, 110.0));
        assert_eq!(client_of_global_physical(&win, gx, gy), (40.0, 30.0));
    }

    #[test]
    fn mixed_dpi_translation_lands_in_the_target_client_space() {
        // Source on a 2x monitor, target on a 1x monitor (physical space):
        // the same global point maps to each window's own client px.
        let src = rect("src", 0.0, 0.0, 1600.0, 1200.0, 2.0);
        let dst = rect("dst", 1600.0, 0.0, 800.0, 600.0, 1.0);
        let (gx, gy) = global_of_client_physical(&src, 850.0, 100.0); // (1700, 200)
        assert_eq!(client_of_global_physical(&dst, gx, gy), (100.0, 200.0));
    }

    #[test]
    fn hit_test_prefers_the_most_recently_focused_of_overlapping_windows() {
        let rects = vec![
            rect("under", 0.0, 0.0, 800.0, 600.0, 1.0),
            rect("over", 400.0, 300.0, 800.0, 600.0, 1.0),
        ];
        let order = vec!["over".to_string(), "under".to_string()];
        assert_eq!(
            hit_test(&rects, &order, 500.0, 400.0).map(|r| r.label.as_str()),
            Some("over"),
        );
        // Reversing recency flips the winner in the overlap.
        let order = vec!["under".to_string(), "over".to_string()];
        assert_eq!(
            hit_test(&rects, &order, 500.0, 400.0).map(|r| r.label.as_str()),
            Some("under"),
        );
        // Outside the overlap only the containing rect can win.
        assert_eq!(
            hit_test(&rects, &order, 100.0, 100.0).map(|r| r.label.as_str()),
            Some("under"),
        );
        assert_eq!(hit_test(&rects, &order, 2000.0, 50.0), None);
    }

    #[test]
    fn hit_test_ranks_unknown_labels_behind_known_ones() {
        let rects = vec![
            rect("unknown", 0.0, 0.0, 800.0, 600.0, 1.0),
            rect("known", 0.0, 0.0, 800.0, 600.0, 1.0),
        ];
        let order = vec!["known".to_string()];
        assert_eq!(
            hit_test(&rects, &order, 10.0, 10.0).map(|r| r.label.as_str()),
            Some("known"),
        );
        // With no order at all the first containing rect still hits.
        assert!(hit_test(&rects, &[], 10.0, 10.0).is_some());
    }
}
