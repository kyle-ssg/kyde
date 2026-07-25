//! Local-history settings, persisted as `history.json` next to keymap.json.
//!
//! Local history (issue #7) snapshots files as you work, independent of git. It is
//! ON by default — snapshots are content-deduped and debounced, so the steady-state
//! cost is near zero — with a hard off switch here, plus the retention window and
//! the save-burst throttle, all editable in Settings → Local History.

use serde::{Deserialize, Serialize};

/// Local-history settings (persisted to `history.json`).
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct HistoryCfg {
    /// Master switch. Off = nothing is recorded, read, or pruned — zero work.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Days a snapshot is kept before pruning (clamped to 1..=90).
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Minimum seconds between snapshots of the same file during a save burst
    /// (clamped to 1..=300). The burst's LAST state is always captured — throttling
    /// delays the write, it never drops it.
    #[serde(default = "default_throttle_secs")]
    pub throttle_secs: u32,
}

fn default_enabled() -> bool {
    true
}
fn default_retention_days() -> u32 {
    7
}
fn default_throttle_secs() -> u32 {
    10
}

impl Default for HistoryCfg {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            retention_days: default_retention_days(),
            throttle_secs: default_throttle_secs(),
        }
    }
}

impl HistoryCfg {
    /// Load from `history.json` (missing/corrupt → defaults).
    #[must_use]
    pub fn load() -> Self {
        crate::store::load_or_default::<Self>("history.json").clamped()
    }

    /// Persist to `history.json`.
    pub fn save(&self) {
        crate::store::save("history.json", &self.clamped());
    }

    /// The retention window in milliseconds.
    #[must_use]
    pub fn retention_ms(&self) -> u64 {
        u64::from(self.clamped().retention_days) * 24 * 60 * 60 * 1000
    }

    /// The save-burst throttle in milliseconds.
    #[must_use]
    pub fn throttle_ms(&self) -> u64 {
        u64::from(self.clamped().throttle_secs) * 1000
    }

    /// Bounds-checked copy — a hand-edited `history.json` can't zero the throttle
    /// (snapshot per keystroke) or set a 10-year retention.
    ///
    /// ```
    /// use kyde_config::history::HistoryCfg;
    /// let silly = HistoryCfg { enabled: true, retention_days: 0, throttle_secs: 9999 };
    /// let c = silly.clamped();
    /// assert_eq!((c.retention_days, c.throttle_secs), (1, 300));
    /// ```
    #[must_use]
    pub fn clamped(&self) -> Self {
        Self {
            enabled: self.enabled,
            retention_days: self.retention_days.clamp(1, 90),
            throttle_secs: self.throttle_secs.clamp(1, 300),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_on_with_sane_windows() {
        let c = HistoryCfg::default();
        assert!(c.enabled);
        assert_eq!(c.retention_days, 7);
        assert_eq!(c.throttle_secs, 10);
        assert_eq!(c.retention_ms(), 7 * 24 * 60 * 60 * 1000);
        assert_eq!(c.throttle_ms(), 10_000);
    }

    #[test]
    fn missing_fields_fill_from_defaults() {
        // A pre-existing/hand-trimmed file with only `enabled` keeps the other defaults.
        let c: HistoryCfg = serde_json::from_str("{\"enabled\":false}").unwrap();
        assert!(!c.enabled);
        assert_eq!(c.retention_days, 7);
        assert_eq!(c.throttle_secs, 10);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let c = HistoryCfg {
            enabled: true,
            retention_days: 10_000,
            throttle_secs: 0,
        }
        .clamped();
        assert_eq!(c.retention_days, 90);
        assert_eq!(c.throttle_secs, 1);
    }
}
