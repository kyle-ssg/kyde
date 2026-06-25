//! Panic-safe scrollbar thumb geometry.

/// Scrollbar thumb length + position for a track, kept **panic-safe** at any window size.
/// `track` = usable track length (px), `max` = max scroll offset (px), `off` = current
/// (negative) scroll offset (px), `end` = inset at each track end (px). Returns
/// `(thumb_len, thumb_pos)`, both clamped within the track.
///
/// Why this exists: the thumb length used to be `(…).clamp(28.0, track - 2*end)` inline.
/// `f32::clamp` PANICS when `min > max`, and `track - 2*end` drops below 28 once the window is
/// shrunk past ~44px — so resizing tiny aborted the process (SIGABRT). Pinning the min under
/// the max here makes it impossible. Pure so it can be unit-tested (below).
pub(crate) fn scrollbar_thumb(track: f32, max: f32, off: f32, end: f32) -> (f32, f32) {
    let hi = (track - 2.0 * end).max(8.0);
    let len = if max > 0.0 {
        (track * track / (track + max)).clamp(28.0_f32.min(hi), hi)
    } else {
        hi
    };
    let span = (track - len - 2.0 * end).max(0.0);
    let frac = if max > 0.0 {
        (-off / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (len, end + frac * span)
}

#[cfg(test)]
mod tests {
    use super::scrollbar_thumb;

    /// Regression guard for the resize-to-tiny SIGABRT: `scrollbar_thumb` must never panic and
    /// must stay within the track for any size. Keep this — it's why the helper is pure.
    #[test]
    fn scrollbar_thumb_never_panics_when_tiny() {
        let tracks = [-50.0, 0.0, 1.0, 10.0, 28.0, 43.9, 44.0, 200.0, 5000.0];
        let maxes = [0.0, 0.5, 1.0, 50.0, 100_000.0];
        let offs = [-1e9, -100.0, 0.0, 50.0, 1e9];
        for &tr in &tracks {
            for &mx in &maxes {
                for &of in &offs {
                    let (len, pos) = scrollbar_thumb(tr, mx, of, 8.0);
                    assert!(len.is_finite() && len > 0.0, "len {len} (track {tr})");
                    assert!(pos.is_finite() && pos >= 0.0, "pos {pos} (track {tr})");
                    assert!(pos <= tr.max(8.0) + 1.0, "pos {pos} past track {tr}");
                }
            }
        }
    }
}
