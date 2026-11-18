//! Pure state machine for the Win+. hotkey, driven by the low-level keyboard hook.

pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_OEM_PERIOD: u16 = 0xBE;
/// Harmless key the caller injects (down then up) before a Win release, so the
/// Start menu does not open.
pub const VK_MASK: u16 = 0xE8;

/// What the hook should do with a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    /// Let the event through untouched.
    Pass,
    /// Eat the event.
    Swallow,
    /// Eat the event and open the picker.
    Open,
    /// Inject `VK_MASK` down and up, then let the Win release through.
    MaskThenPass,
}

/// Physical key state (`GetAsyncKeyState` high bit) read inside the hook, before the current
/// event updates it: for a key that is being pressed, `true` means autorepeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Physical {
    pub lwin: bool,
    pub rwin: bool,
    pub period: bool,
}

#[derive(Debug, Default)]
pub struct WinPeriodMatcher {
    lwin: bool,
    rwin: bool,
    /// An `Open` happened during the current Win press.
    opened: bool,
    /// A period down was swallowed and its key-up has not been seen yet.
    period_down: bool,
}

impl WinPeriodMatcher {
    pub fn on_key(&mut self, vk: u16, down: bool, injected: bool) -> HookAction {
        if injected {
            return HookAction::Pass;
        }
        match vk {
            VK_LWIN | VK_RWIN => self.on_win(vk, down),
            VK_OEM_PERIOD => self.on_period(down),
            _ => HookAction::Pass,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Like [`Self::on_key`], but first repairs state that missed key-ups left behind.
    ///
    /// Key-ups can fail to reach the hook (seen while our picker takes the foreground). On a
    /// fresh period press (`phys.period` still up, so not autorepeat) the matcher rebuilds
    /// its state from the physical Win keys, so a stale flag can neither eat a period nor
    /// turn the next Win+. into "autorepeat". `phys` is only read for a period key-down.
    pub fn on_key_checked(
        &mut self,
        vk: u16,
        down: bool,
        injected: bool,
        phys: Physical,
    ) -> HookAction {
        if vk == VK_OEM_PERIOD && down && !injected && !phys.period {
            self.resync(phys.lwin, phys.rwin);
        }
        self.on_key(vk, down, injected)
    }

    /// Forgets everything and takes the Win keys' state from the caller.
    pub fn resync(&mut self, lwin: bool, rwin: bool) {
        *self = Self {
            lwin,
            rwin,
            ..Self::default()
        };
    }

    fn win_held(&self) -> bool {
        self.lwin || self.rwin
    }

    fn on_win(&mut self, vk: u16, down: bool) -> HookAction {
        let key = if vk == VK_LWIN {
            &mut self.lwin
        } else {
            &mut self.rwin
        };
        *key = down;
        if down {
            return HookAction::Pass;
        }
        let action = if self.opened {
            HookAction::MaskThenPass
        } else {
            HookAction::Pass
        };
        if !self.win_held() {
            self.opened = false;
        }
        action
    }

    fn on_period(&mut self, down: bool) -> HookAction {
        if down {
            if self.period_down {
                // Autorepeat of a swallowed press.
                HookAction::Swallow
            } else if self.win_held() {
                self.period_down = true;
                self.opened = true;
                HookAction::Open
            } else {
                HookAction::Pass
            }
        } else if self.period_down {
            // Also covers a key-up arriving after Win was released.
            self.period_down = false;
            HookAction::Swallow
        } else {
            HookAction::Pass
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN_HELD: Physical = Physical {
        lwin: true,
        rwin: false,
        period: false,
    };

    #[test]
    fn stale_period_down_then_fresh_press_with_win_opens() {
        let mut m = WinPeriodMatcher::default();
        let p = Physical::default();
        assert_eq!(m.on_key_checked(VK_LWIN, true, false, p), HookAction::Pass);
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, WIN_HELD),
            HookAction::Open
        );
        // The period and Win key-ups never arrive. Next press:
        assert_eq!(m.on_key_checked(VK_LWIN, true, false, p), HookAction::Pass);
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, WIN_HELD),
            HookAction::Open
        );
    }

    #[test]
    fn stale_win_then_plain_period_passes() {
        let mut m = WinPeriodMatcher::default();
        let p = Physical::default();
        m.on_key_checked(VK_LWIN, true, false, p);
        m.on_key_checked(VK_OEM_PERIOD, true, false, WIN_HELD);
        // Both key-ups are missed; Win is physically up now.
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, p),
            HookAction::Pass
        );
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, false, false, p),
            HookAction::Pass
        );
    }

    #[test]
    fn autorepeat_after_open_stays_swallowed() {
        let mut m = WinPeriodMatcher::default();
        let p = Physical::default();
        let repeat = Physical {
            period: true,
            ..WIN_HELD
        };
        m.on_key_checked(VK_LWIN, true, false, p);
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, WIN_HELD),
            HookAction::Open
        );
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, repeat),
            HookAction::Swallow
        );
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, repeat),
            HookAction::Swallow
        );
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, false, false, WIN_HELD),
            HookAction::Swallow
        );
        assert_eq!(
            m.on_key_checked(VK_LWIN, false, false, p),
            HookAction::MaskThenPass
        );
    }

    #[test]
    fn rwin_counts_in_resync() {
        let mut m = WinPeriodMatcher::default();
        let rwin = Physical {
            rwin: true,
            ..Physical::default()
        };
        assert_eq!(
            m.on_key_checked(VK_OEM_PERIOD, true, false, rwin),
            HookAction::Open
        );
    }

    #[test]
    fn win_period_opens_and_masks() {
        let mut m = WinPeriodMatcher::default();
        assert_eq!(m.on_key(VK_LWIN, true, false), HookAction::Pass);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Swallow); // autorepeat
        assert_eq!(m.on_key(VK_OEM_PERIOD, false, false), HookAction::Swallow);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::MaskThenPass);
    }

    #[test]
    fn plain_period_passes() {
        let mut m = WinPeriodMatcher::default();
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Pass);
        assert_eq!(m.on_key(VK_OEM_PERIOD, false, false), HookAction::Pass);
    }

    #[test]
    fn win_alone_passes() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_RWIN, true, false);
        assert_eq!(m.on_key(VK_RWIN, false, false), HookAction::Pass);
    }

    #[test]
    fn injected_ignored() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_LWIN, true, false);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, true), HookAction::Pass);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::Pass);
    }

    #[test]
    fn second_combo_in_same_press() {
        let mut m = WinPeriodMatcher::default();
        assert_eq!(m.on_key(VK_LWIN, true, false), HookAction::Pass);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        assert_eq!(m.on_key(VK_OEM_PERIOD, false, false), HookAction::Swallow);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        assert_eq!(m.on_key(VK_OEM_PERIOD, false, false), HookAction::Swallow);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::MaskThenPass);
    }

    #[test]
    fn rwin_period_opens_and_masks() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_RWIN, true, false);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        m.on_key(VK_OEM_PERIOD, false, false);
        assert_eq!(m.on_key(VK_RWIN, false, false), HookAction::MaskThenPass);
    }

    #[test]
    fn period_up_after_win_release_is_swallowed() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_LWIN, true, false);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::MaskThenPass);
        assert_eq!(m.on_key(VK_OEM_PERIOD, false, false), HookAction::Swallow);
        // Orphan consumed: later plain period passes again.
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Pass);
        assert_eq!(m.on_key(VK_OEM_PERIOD, false, false), HookAction::Pass);
    }

    #[test]
    fn other_win_still_held_keeps_press_alive() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_LWIN, true, false);
        m.on_key(VK_RWIN, true, false);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        m.on_key(VK_OEM_PERIOD, false, false);
        // Releasing one Win key after an Open masks, but the press is not over.
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::MaskThenPass);
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Open);
        m.on_key(VK_OEM_PERIOD, false, false);
        assert_eq!(m.on_key(VK_RWIN, false, false), HookAction::MaskThenPass);
        // Fully released: state reset.
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Pass);
    }

    #[test]
    fn win_up_without_open_passes_while_other_held() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_LWIN, true, false);
        m.on_key(VK_RWIN, true, false);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::Pass);
        assert_eq!(m.on_key(VK_RWIN, false, false), HookAction::Pass);
    }

    #[test]
    fn new_press_after_reset_does_not_mask() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_LWIN, true, false);
        m.on_key(VK_OEM_PERIOD, true, false);
        m.on_key(VK_OEM_PERIOD, false, false);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::MaskThenPass);
        m.on_key(VK_LWIN, true, false);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::Pass);
    }

    #[test]
    fn reset_clears_state() {
        let mut m = WinPeriodMatcher::default();
        m.on_key(VK_LWIN, true, false);
        m.on_key(VK_OEM_PERIOD, true, false);
        m.reset();
        assert_eq!(m.on_key(VK_OEM_PERIOD, true, false), HookAction::Pass);
        assert_eq!(m.on_key(VK_LWIN, false, false), HookAction::Pass);
    }
}
