//! Whether macOS Mission Control (or App Exposé) is open, told to every
//! window as the `mission-control` event, so a page can show its name large
//! while the window is a thumbnail among others.
//!
//! macOS has no public notice for Mission Control, and the page sees nothing
//! either: no visibility change, no blur, no resize (measured 2026-11-05: the
//! thumbnail is the window's live surface and the page keeps rendering). What
//! changes is the Dock's windows (traced at 16 ms, 2026-11-05):
//!
//! - open: a full-screen Dock window at layer 18 (`SPREAD_LAYER`), a new one
//!   (new window number) each time it opens;
//! - hovering a thumbnail: Mission Control's second full-screen window flips
//!   between layers 20 and 0, and a small title label comes and goes at
//!   layer 20 — none of which is a close;
//! - closing: a 1×1 Dock window at layer 0 (`CLOSING_LAYER`), parked far off
//!   screen, appears ~0.25 s before the layer-18 window goes (every close
//!   traced, never a hover). App Exposé closes with no such step: its window
//!   just goes.
//!
//! The event is `true` on each opening — again for a new layer-18 window even
//! when no close was seen between them (a quick out-and-in fits between two
//! reads, and the page hides its card on the first touch) — and `false` as
//! soon as it starts closing. Reading window ids, owners, layers and sizes
//! needs no Screen Recording or Accessibility permission (window *names*
//! would).
//!
//! One thread polls every `POLL` while the app has a window (`POLL_OPEN`
//! while open) and sends only changes. A poll asks for the on-screen window
//! *ids* only (`CGWindowListCreate`, ~0.3 ms, worst seen 12 ms). Describing
//! windows (owner, layer, size) costs the window server far more — ~1 ms
//! typical, with stalls of 30–130 ms measured, felt as a hitch of the whole
//! screen — so `Seen` describes each id once, the first time it appears, and
//! remembers it. Every signal above is a new id (the layer-18 window, the 1×1
//! marker) or an id leaving (App Exposé's close), so steady state describes
//! nothing, and hovering in Mission Control re-shows windows already known.

use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Duration;

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFArray, CFString};
use objc2_core_graphics::{
    kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerName,
    CGWindowID, CGWindowListCreate, CGWindowListCreateDescriptionFromArray, CGWindowListOption,
};
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSNumber, NSObject, NSString};
use tauri::{AppHandle, Emitter, Manager};

/// The event every window's page listens for.
pub(crate) const EVENT: &str = "mission-control";

/// While closed: long enough to cost nothing measurable, short enough that
/// the name is up while Mission Control is still zooming out.
const POLL: Duration = Duration::from_millis(250);

/// While open: the close must land at once, or the card rides the zoom back
/// into the desktop. Only while open, so it costs nothing the rest of the time.
const POLL_OPEN: Duration = Duration::from_millis(50);

/// The layer of the Dock's Mission Control / App Exposé window.
const SPREAD_LAYER: i64 = 18;

/// The layer of the 1×1 window that marks Mission Control closing.
const CLOSING_LAYER: i64 = 0;

/// How many window ids `Seen` remembers before it forgets those no longer on
/// screen. Ids only grow, so without a bound it would too.
const SEEN_CAP: usize = 1024;

/// One Dock window on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DockWindow {
    layer: i64,
    number: i64,
    width: f64,
    height: f64,
}

/// Start the watcher. A shell without windows sleeps through its polls.
pub(crate) fn install(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("mission-control".into())
        .spawn(move || {
            let mut seen = Seen::default();
            let mut shown: Option<i64> = None;
            loop {
                std::thread::sleep(if shown.is_some() { POLL_OPEN } else { POLL });
                // Nothing to name: don't ask the window server at all.
                let now = if app.webview_windows().is_empty() {
                    None
                } else {
                    spread(&dock_windows(&mut seen))
                };
                if now != shown {
                    shown = now;
                    let _ = app.emit(EVENT, shown.is_some());
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "mission control watcher unavailable");
    }
}

/// The open spread's window number, or None when it is closed or closing.
fn spread(dock: &[DockWindow]) -> Option<i64> {
    let open = dock.iter().find(|w| w.layer == SPREAD_LAYER)?;
    let closing = dock
        .iter()
        .any(|w| w.layer == CLOSING_LAYER && w.width <= 1.0 && w.height <= 1.0);
    (!closing).then_some(open.number)
}

/// What each window id seen on screen is: a Dock window, or `None` for
/// anything else. A window keeps the layer and size it was first seen with;
/// every window the watcher reads is created at its final layer (traced).
#[derive(Default)]
struct Seen(HashMap<CGWindowID, Option<DockWindow>>);

impl Seen {
    /// The Dock windows among `on_screen`, describing only ids not seen yet.
    /// `describe` answers for the ids it is given: one it skips is gone or not
    /// the Dock's and is never asked about again; a call that fails (`None`)
    /// remembers nothing, so the next poll asks again.
    fn dock_windows(
        &mut self,
        on_screen: &[CGWindowID],
        describe: impl FnOnce(&[CGWindowID]) -> Option<Vec<(CGWindowID, Option<DockWindow>)>>,
    ) -> Vec<DockWindow> {
        let fresh: Vec<CGWindowID> = on_screen
            .iter()
            .copied()
            .filter(|id| !self.0.contains_key(id))
            .collect();
        if !fresh.is_empty() {
            if let Some(described) = describe(&fresh) {
                if self.0.len() + fresh.len() > SEEN_CAP {
                    self.0.retain(|id, _| on_screen.contains(id));
                }
                for id in &fresh {
                    self.0.insert(*id, None);
                }
                for (id, window) in described {
                    self.0.insert(id, window);
                }
            }
        }
        on_screen
            .iter()
            .filter_map(|id| self.0.get(id).copied().flatten())
            .collect()
    }
}

/// The Dock's on-screen windows.
///
/// Runs on the watcher's own thread, which has no autorelease pool: each read
/// gets one, or every autoreleased object it touches leaks for the life of
/// the app.
fn dock_windows(seen: &mut Seen) -> Vec<DockWindow> {
    autoreleasepool(|_| {
        let Some(ids) = CGWindowListCreate(CGWindowListOption::OptionOnScreenOnly, kCGNullWindowID)
        else {
            return Vec::new();
        };
        // The array holds the ids themselves as its values, not CF objects.
        let on_screen: Vec<CGWindowID> = (0..ids.count())
            .map(|i| unsafe { ids.value_at_index(i) } as usize as CGWindowID)
            .collect();
        seen.dock_windows(&on_screen, describe)
    })
}

/// Owner, layer and size of `ids`, one window-server call for all of them;
/// `None` when the call itself fails.
fn describe(ids: &[CGWindowID]) -> Option<Vec<(CGWindowID, Option<DockWindow>)>> {
    let mut values: Vec<*const c_void> =
        ids.iter().map(|&id| id as usize as *const c_void).collect();
    // No callbacks: the values are plain ids, as `CGWindowListCreate` returns them.
    let array = unsafe {
        CFArray::new(
            None,
            values.as_mut_ptr(),
            values.len() as isize,
            std::ptr::null(),
        )
    }?;
    let list = unsafe { CGWindowListCreateDescriptionFromArray(Some(&array)) }?;
    // CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary,
    // the CFString keys to NSString, CFNumber to NSNumber.
    let list: &CFArray = &list;
    let list: &NSArray<NSDictionary<NSString, NSObject>> =
        unsafe { &*(list as *const CFArray).cast::<NSArray<NSDictionary<NSString, NSObject>>>() };
    let key = |k: &CFString| -> &NSString { unsafe { &*(k as *const CFString).cast() } };
    let (owner_key, layer_key, number_key, bounds_key) = unsafe {
        (
            key(kCGWindowOwnerName),
            key(kCGWindowLayer),
            key(kCGWindowNumber),
            key(kCGWindowBounds),
        )
    };
    let number = |dict: &NSDictionary<NSString, NSObject>, k: &NSString| {
        dict.objectForKey(k)
            .and_then(|v| v.downcast::<NSNumber>().ok())
    };
    let described = list
        .iter()
        .filter_map(|window| {
            let id = number(&window, number_key)?.as_i64();
            let is_dock = window
                .objectForKey(owner_key)
                .and_then(|o| o.downcast::<NSString>().ok())
                .is_some_and(|owner| &*owner == ns_string!("Dock"));
            if !is_dock {
                return Some((id as CGWindowID, None));
            }
            let bounds = window
                .objectForKey(bounds_key)?
                .downcast::<NSDictionary>()
                .ok()?;
            // A window's bounds: string keys, number values.
            let bounds: &NSDictionary<NSString, NSObject> =
                unsafe { &*(&*bounds as *const NSDictionary).cast() };
            let dock = DockWindow {
                layer: number(&window, layer_key)?.as_i64(),
                number: id,
                width: number(bounds, ns_string!("Width"))?.as_f64(),
                height: number(bounds, ns_string!("Height"))?.as_f64(),
            };
            Some((id as CGWindowID, Some(dock)))
        })
        .collect();
    Some(described)
}

#[cfg(test)]
mod tests {
    use super::{spread, DockWindow, Seen, SEEN_CAP};

    fn w(layer: i64, number: i64, width: f64, height: f64) -> DockWindow {
        DockWindow {
            layer,
            number,
            width,
            height,
        }
    }
    // The Dock's own window (layer 20) and wallpaper (far below) are always
    // there; these are the shapes traced on 2026-11-05.
    fn dock() -> DockWindow {
        w(20, 38, 2560.0, 1440.0)
    }
    fn wallpaper() -> DockWindow {
        w(-2147483624, 39, 2560.0, 1440.0)
    }

    #[test]
    fn closed_is_none() {
        assert_eq!(spread(&[dock(), wallpaper()]), None);
    }

    #[test]
    fn mission_control_and_app_expose_are_open() {
        // Mission Control: an extra full-screen layer-20 window beside it.
        let mc = [
            dock(),
            w(20, 318457, 2560.0, 1440.0),
            w(18, 318456, 2560.0, 1440.0),
            wallpaper(),
        ];
        assert_eq!(spread(&mc), Some(318456));
        // App Exposé: the layer-18 window alone.
        assert_eq!(
            spread(&[dock(), w(18, 318455, 2560.0, 1440.0), wallpaper()]),
            Some(318455)
        );
    }

    #[test]
    fn closing_is_none_before_the_spread_window_goes() {
        let closing = [
            w(20, 318451, 587.0, 86.0),
            dock(),
            w(18, 318449, 2560.0, 1440.0),
            w(0, 318452, 1.0, 1.0),
            w(0, 318450, 2560.0, 1440.0),
            wallpaper(),
        ];
        assert_eq!(spread(&closing), None);
    }

    #[test]
    fn hovering_is_not_a_close() {
        // A thumbnail hovered: the second full-screen window at layer 0 and
        // the title label at layer 20.
        let hover = [
            w(20, 318722, 587.0, 86.0),
            dock(),
            w(18, 318720, 2560.0, 1440.0),
            w(0, 318721, 2560.0, 1440.0),
            wallpaper(),
        ];
        assert_eq!(spread(&hover), Some(318720));
        // Nothing hovered: the label alone, the full-screen one back at 20.
        let label = [
            w(20, 318722, 134.0, 86.0),
            dock(),
            w(18, 318720, 2560.0, 1440.0),
        ];
        assert_eq!(spread(&label), Some(318720));
    }

    #[test]
    fn closing_from_a_hover_is_still_a_close() {
        // The 1×1 off-screen marker beside a hover layer left at 0.
        let closing = [
            dock(),
            w(18, 318737, 2560.0, 1440.0),
            w(0, 318740, 1.0, 1.0),
            w(0, 318738, 2560.0, 1440.0),
            wallpaper(),
        ];
        assert_eq!(spread(&closing), None);
    }

    #[test]
    fn a_quick_reopen_is_a_new_window() {
        let first = [dock(), w(18, 318456, 2560.0, 1440.0)];
        let again = [dock(), w(18, 318458, 2560.0, 1440.0)];
        assert_ne!(spread(&first), spread(&again));
    }

    #[test]
    fn seen_describes_each_window_once() {
        let mut seen = Seen::default();
        let spread_window = w(18, 7, 2560.0, 1440.0);
        let first = seen.dock_windows(&[1, 7], |ids| {
            assert_eq!(ids, [1, 7]);
            Some(vec![(1, None), (7, Some(spread_window))])
        });
        assert_eq!(first, [spread_window]);
        // The same windows again: nothing to describe.
        let again = seen.dock_windows(&[7, 1], |_| panic!("described a known window"));
        assert_eq!(again, [spread_window]);
        // Only the new id is described; one it skips is not the Dock's.
        let marker = w(0, 9, 1.0, 1.0);
        let next = seen.dock_windows(&[1, 7, 9, 11], |ids| {
            assert_eq!(ids, [9, 11]);
            Some(vec![(9, Some(marker))])
        });
        assert_eq!(next, [spread_window, marker]);
        let _ = seen.dock_windows(&[11], |_| panic!("a skipped id was asked about again"));
        // Off screen: not returned, still known.
        assert_eq!(
            seen.dock_windows(&[1], |_| panic!("described a known window")),
            []
        );
    }

    #[test]
    fn a_failed_describe_is_asked_again() {
        let mut seen = Seen::default();
        let spread_window = w(18, 7, 2560.0, 1440.0);
        assert_eq!(seen.dock_windows(&[7], |_| None), []);
        let next = seen.dock_windows(&[7], |ids| {
            assert_eq!(ids, [7]);
            Some(vec![(7, Some(spread_window))])
        });
        assert_eq!(next, [spread_window]);
    }

    #[test]
    fn seen_stays_bounded() {
        let mut seen = Seen::default();
        for id in 0..(SEEN_CAP as u32 * 3) {
            let _ = seen.dock_windows(&[id], |ids| {
                Some(ids.iter().map(|&id| (id, None)).collect())
            });
        }
        assert!(seen.0.len() <= SEEN_CAP);
    }
}
