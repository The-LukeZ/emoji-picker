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

/// True when `window` covers the whole `monitor` rect (shell windows are excluded by the caller).
pub fn is_fullscreen(window: Rect, monitor: Rect) -> bool {
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
        assert!(is_fullscreen(mon, mon));
        assert!(is_fullscreen(
            Rect {
                left: -8,
                top: -8,
                right: 1928,
                bottom: 1088
            },
            mon
        ));
        // maximized, taskbar visible
        assert!(!is_fullscreen(
            Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1032
            },
            mon
        ));
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
