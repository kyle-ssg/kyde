//! Pure control-state of the bottom terminal panel — open / active / maximized + a deferred
//! focus flag. Everything EXCEPT the PTY-backed views (those stay in `Kyde.term_tabs`), so the
//! open / close / toggle / focus state machine is unit-testable without spawning a shell.
//! A crate-root module (NOT behind the `terminal` feature) so these tests always run; the
//! `Kyde.term_panel` field + the gpui glue in `views/terminal_panel.rs` are feature-gated.

/// Where keyboard focus should go after a panel operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FocusTarget {
    /// Focus the active terminal tab's widget.
    Terminal,
    /// Focus the app root (the panel hid, or there's no tab to focus).
    AppRoot,
}

/// What `⌃\`` should do, given the panel's current visibility + focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ToggleAction {
    /// Hidden → show it (`spawn` a first tab if there are none yet), then focus it.
    Open { spawn: bool },
    /// Visible AND focused → hide it, returning focus to the app root.
    Hide,
    /// Visible but unfocused → just focus the active terminal (VSCode `⌃\`` behaviour).
    FocusTerminal,
}

#[derive(Default)]
pub(crate) struct TermPanel {
    /// Panel is visible.
    pub open: bool,
    /// Panel fills the whole right column (tree + editor hidden).
    pub maximized: bool,
    /// Index of the active tab into `Kyde.term_tabs`.
    pub active: usize,
    /// A tab/visibility change happened where we couldn't focus directly (e.g. the `^D`
    /// close runs in a window-less subscription). The next paint consumes this to move focus.
    pub focus_pending: bool,
}

impl TermPanel {
    /// A new tab was appended (the caller pushed the view): it becomes active + visible, and
    /// should take focus.
    pub(crate) fn on_tab_added(&mut self, new_count: usize) {
        self.open = true;
        self.active = new_count.saturating_sub(1);
        self.focus_pending = true;
    }

    /// Close tab `idx` of `count_before` tabs (the caller removes the view). Updates the active
    /// index + visibility and returns where focus should go. Closing the active tab lands on
    /// the tab that slides into its slot ("next"); closing the last tab hides the panel.
    pub(crate) fn on_tab_closed(&mut self, count_before: usize, idx: usize) -> FocusTarget {
        self.focus_pending = true;
        match active_after_close(count_before, idx, self.active) {
            None => {
                self.open = false;
                self.maximized = false;
                self.active = 0;
                FocusTarget::AppRoot
            }
            Some(a) => {
                self.active = a;
                FocusTarget::Terminal
            }
        }
    }

    /// What `⌃\`` should do, given whether the terminal currently owns focus and whether any
    /// tabs exist. Pure decision — the caller applies the state change + focus.
    pub(crate) fn toggle(&self, focused: bool, has_tabs: bool) -> ToggleAction {
        if !self.open {
            ToggleAction::Open { spawn: !has_tabs }
        } else if focused {
            ToggleAction::Hide
        } else {
            ToggleAction::FocusTerminal
        }
    }
}

/// Which tab is active after closing tab `closed` (a valid index into the `len` tabs before
/// removal), given the previously-active index. `None` = no tabs left (panel should hide).
/// Closing a tab *before* the active one shifts the selection left so it stays on the same
/// logical tab; closing the active one lands on the tab that slides into its slot.
fn active_after_close(len: usize, closed: usize, active: usize) -> Option<usize> {
    let new_len = len.saturating_sub(1);
    if new_len == 0 || closed >= len {
        return (new_len > 0).then_some(active.min(new_len.saturating_sub(1)));
    }
    let a = if closed < active { active - 1 } else { active };
    Some(a.min(new_len - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_last_remaining_tab_hides_panel() {
        let mut p = TermPanel {
            open: true,
            active: 0,
            ..Default::default()
        };
        assert_eq!(p.on_tab_closed(1, 0), FocusTarget::AppRoot);
        assert!(!p.open);
        assert!(p.focus_pending);
    }

    #[test]
    fn closing_active_tab_lands_on_next_and_keeps_panel() {
        // [A,B,C] active=C(2), ^D closes C → lands on the new last (B), panel stays.
        let mut p = TermPanel {
            open: true,
            active: 2,
            ..Default::default()
        };
        assert_eq!(p.on_tab_closed(3, 2), FocusTarget::Terminal);
        assert_eq!(p.active, 1);
        assert!(p.open);
    }

    #[test]
    fn closing_tab_before_active_shifts_selection_left() {
        // [A,B,C] active=B(1), close A(0) → B is now index 0; stay on it.
        let mut p = TermPanel {
            open: true,
            active: 1,
            ..Default::default()
        };
        assert_eq!(p.on_tab_closed(3, 0), FocusTarget::Terminal);
        assert_eq!(p.active, 0);
    }

    #[test]
    fn closing_tab_after_active_keeps_selection() {
        let mut p = TermPanel {
            open: true,
            active: 0,
            ..Default::default()
        };
        p.on_tab_closed(3, 2);
        assert_eq!(p.active, 0);
    }

    #[test]
    fn adding_tab_makes_it_active_and_visible() {
        let mut p = TermPanel::default();
        p.on_tab_added(1);
        assert!(p.open && p.active == 0 && p.focus_pending);
        p.on_tab_added(3);
        assert_eq!(p.active, 2);
    }

    #[test]
    fn toggle_decision_table() {
        let hidden = TermPanel::default();
        // Hidden, no tabs → open + spawn the first.
        assert_eq!(hidden.toggle(false, false), ToggleAction::Open { spawn: true });
        // Hidden, tabs already exist → open, no spawn.
        assert_eq!(hidden.toggle(false, true), ToggleAction::Open { spawn: false });
        let open = TermPanel {
            open: true,
            ..Default::default()
        };
        // Visible + focused → hide. Visible + unfocused → focus it.
        assert_eq!(open.toggle(true, true), ToggleAction::Hide);
        assert_eq!(open.toggle(false, true), ToggleAction::FocusTerminal);
    }
}
