//! Keyboard selection and multi-pick queue state for the emoji grid.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Insert(String),
}

#[derive(Debug, Clone, Default)]
pub struct PickerState {
    pub cols: usize,
    pub len: usize,
    pub selected: usize,
    pub queue: Vec<String>,
}

impl PickerState {
    /// Sets the grid shape and selects the first cell. The queue is kept.
    pub fn reset(&mut self, cols: usize, len: usize) {
        self.cols = cols.max(1);
        self.len = len;
        self.selected = 0;
    }

    /// Moves the selection. Left/Right step by one and cross row boundaries;
    /// Up/Down step by `cols`: Up stays put on the first row, Down from a
    /// short last row lands on the last cell. All moves clamp to `0..len`, no wrap.
    pub fn nav(&mut self, n: Nav) {
        if self.len == 0 {
            self.selected = 0;
            return;
        }
        let last = self.len - 1;
        let cols = self.cols.max(1);
        self.selected = match n {
            Nav::Left => self.selected.saturating_sub(1),
            Nav::Right => (self.selected + 1).min(last),
            Nav::Up if self.selected >= cols => self.selected - cols,
            Nav::Up => self.selected,
            Nav::Down => (self.selected + cols).min(last),
        };
    }

    /// Shift pushes the glyph to the queue and stays open. Without shift the
    /// queue plus this glyph is returned for insertion and the queue is cleared.
    pub fn pick(&mut self, glyph: &str, shift: bool) -> Outcome {
        if shift {
            self.queue.push(glyph.to_string());
            Outcome::Stay
        } else {
            let mut text = self.queue.concat();
            text.push_str(glyph);
            self.queue.clear();
            Outcome::Insert(text)
        }
    }

    /// Escape discards the queue and closes.
    pub fn escape(&mut self) -> Outcome {
        self.queue.clear();
        Outcome::Close
    }

    /// Losing focus discards the queue and closes: the click went to some other
    /// window, so inserting there could hit the wrong one.
    pub fn deactivated(&mut self) -> Outcome {
        self.queue.clear();
        Outcome::Close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(cols: usize, len: usize) -> PickerState {
        let mut p = PickerState::default();
        p.reset(cols, len);
        p
    }

    #[test]
    fn nav_wraps_rows_and_clamps() {
        let mut p = grid(9, 20);
        p.selected = 8;
        p.nav(Nav::Right);
        assert_eq!(p.selected, 9);
        p.nav(Nav::Left);
        assert_eq!(p.selected, 8);
        p.selected = 15;
        p.nav(Nav::Down);
        assert_eq!(p.selected, 19);
        p.selected = 3;
        p.nav(Nav::Up);
        assert_eq!(p.selected, 3);
        p.selected = 19;
        p.nav(Nav::Right);
        assert_eq!(p.selected, 19);
        p.selected = 0;
        p.nav(Nav::Left);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn up_down_move_by_cols() {
        let mut p = grid(9, 20);
        p.selected = 12;
        p.nav(Nav::Up);
        assert_eq!(p.selected, 3);
        p.nav(Nav::Down);
        assert_eq!(p.selected, 12);
    }

    #[test]
    fn single_pick_inserts() {
        let mut p = grid(9, 20);
        assert_eq!(p.pick("👍", false), Outcome::Insert("👍".into()));
    }

    #[test]
    fn queue_then_insert_all() {
        let mut p = grid(9, 20);
        assert_eq!(p.pick("👍", true), Outcome::Stay);
        p.pick("🔥", true);
        assert_eq!(p.pick("❤️", false), Outcome::Insert("👍🔥❤️".into()));
        assert!(p.queue.is_empty());
    }

    #[test]
    fn escape_discards_queue() {
        let mut p = grid(9, 20);
        p.pick("👍", true);
        assert_eq!(p.escape(), Outcome::Close);
        assert!(p.queue.is_empty());
    }

    #[test]
    fn deactivated_discards_queue() {
        let mut p = grid(9, 20);
        p.pick("👍", true);
        assert_eq!(p.deactivated(), Outcome::Close);
        assert!(p.queue.is_empty());
    }

    #[test]
    fn reset_keeps_queue_and_selects_first() {
        let mut p = grid(9, 20);
        p.pick("👍", true);
        p.selected = 7;
        p.reset(9, 5);
        assert_eq!(p.selected, 0);
        assert_eq!(p.queue, vec!["👍".to_string()]);
    }

    #[test]
    fn empty_grid_nav_is_safe() {
        let mut p = grid(9, 0);
        for n in [Nav::Left, Nav::Right, Nav::Up, Nav::Down] {
            p.nav(n);
            assert_eq!(p.selected, 0);
        }
    }

    #[test]
    fn zero_cols_is_safe() {
        let mut p = grid(0, 5);
        assert_eq!(p.cols, 1);
        p.nav(Nav::Down);
        assert_eq!(p.selected, 1);
    }
}
