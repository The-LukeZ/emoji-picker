//! Window geometry: fullscreen detection, picker placement and atlas selection.

use crate::table::ATLAS_SIZES;

/// Gap in pixels between the caret (or cursor) and the picker.
const GAP: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// True when `window` is a fullscreen app on `monitor` (shell windows are excluded by the caller).
///
/// A normal maximized window (`maximized` and `has_caption`) is never fullscreen: its window
/// rect includes the invisible resize borders, so on a monitor without a visible taskbar it
/// covers the whole monitor rect. Borderless and exclusive fullscreen windows have no caption.
pub fn is_fullscreen(window: Rect, monitor: Rect, maximized: bool, has_caption: bool) -> bool {
    if maximized && has_caption {
        return false;
    }
    covers(window, monitor)
}

/// True when `window` covers the whole `monitor` rect.
fn covers(window: Rect, monitor: Rect) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// Top-left for the picker: below the caret (or cursor), above it when the bottom would
/// overflow `work`, then clamped into `work`.
pub fn place_picker(caret: Option<Rect>, cursor: Point, size: (i32, i32), work: Rect) -> Point {
    // A missing caret behaves like a zero-size rect at the cursor.
    let anchor = caret.unwrap_or(Rect {
        left: cursor.x,
        top: cursor.y,
        right: cursor.x,
        bottom: cursor.y,
    });
    let (width, height) = size;
    let mut y = anchor.bottom + GAP;
    if y + height > work.bottom {
        y = anchor.top - GAP - height;
    }
    // Clamp last; if the picker is larger than `work`, the top-left edge wins.
    let x = anchor.left.min(work.right - width).max(work.left);
    let y = y.min(work.bottom - height).max(work.top);
    Point { x, y }
}

/// Smallest atlas size that is at least `ceil(tile_px * scale)`, else the largest.
pub fn atlas_size_for(tile_px: u16, scale: f32) -> u16 {
    let wanted = (f32::from(tile_px) * scale).ceil();
    ATLAS_SIZES
        .iter()
        .copied()
        .find(|&size| f32::from(size) >= wanted)
        .unwrap_or(48)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_detection() {
        let mon = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let maximized_rect = Rect {
            left: -8,
            top: -8,
            right: 1928,
            bottom: 1088,
        };
        // borderless fullscreen (game, F11 browser)
        assert!(is_fullscreen(mon, mon, false, false));
        assert!(is_fullscreen(maximized_rect, mon, false, false));
        // borderless window that reports maximized
        assert!(is_fullscreen(maximized_rect, mon, true, false));
        // captioned window sized over the monitor but not maximized
        assert!(is_fullscreen(mon, mon, false, true));
        // maximized, taskbar visible
        assert!(!is_fullscreen(
            Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1032
            },
            mon,
            false,
            false
        ));
    }

    #[test]
    fn maximized_captioned_window_is_not_fullscreen() {
        let mon = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        // auto-hide taskbar or a monitor without a taskbar: the work area is the monitor rect,
        // so a normal maximized window with its resize borders covers the whole monitor.
        let maximized_rect = Rect {
            left: -8,
            top: -8,
            right: 1928,
            bottom: 1088,
        };
        assert!(!is_fullscreen(maximized_rect, mon, true, true));
        // same on a secondary monitor to the right
        let second = Rect {
            left: 1920,
            top: 0,
            right: 4480,
            bottom: 1440,
        };
        let on_second = Rect {
            left: 1912,
            top: -8,
            right: 4488,
            bottom: 1448,
        };
        assert!(!is_fullscreen(on_second, second, true, true));
        assert!(is_fullscreen(on_second, second, true, false));
    }

    #[test]
    fn placement() {
        let work = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1032,
        };
        let caret = |x, y| {
            Some(Rect {
                left: x,
                top: y,
                right: x + 2,
                bottom: y + 18,
            })
        };
        let origin = Point { x: 0, y: 0 };
        assert_eq!(
            place_picker(caret(100, 100), origin, (360, 420), work),
            Point { x: 100, y: 122 }
        );
        assert_eq!(
            place_picker(caret(100, 900), origin, (360, 420), work),
            Point { x: 100, y: 476 }
        );
        assert_eq!(
            place_picker(caret(1800, 100), origin, (360, 420), work).x,
            1560
        );
        assert_eq!(
            place_picker(None, Point { x: 50, y: 50 }, (360, 420), work),
            Point { x: 50, y: 54 }
        );
    }

    #[test]
    fn atlas_choice() {
        assert_eq!(atlas_size_for(32, 1.0), 32);
        assert_eq!(atlas_size_for(32, 1.25), 40);
        assert_eq!(atlas_size_for(32, 1.5), 48);
        assert_eq!(atlas_size_for(48, 2.0), 48);
    }
}
