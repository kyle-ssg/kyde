//! Three-way (diff3-style) merge model for the conflict-resolution view.
//!
//! Diffs `base→ours` and `base→theirs` at line level (same `similar` machinery as
//! [`FileDiff`](crate::FileDiff)), then aligns the two change sets over the base:
//! regions changed on one side become clean [`ChunkKind::Ours`]/[`ChunkKind::Theirs`]
//! chunks, identical changes collapse to [`ChunkKind::Same`], and overlapping different
//! changes become [`ChunkKind::Conflict`] chunks. NOTHING is applied automatically:
//! every non-stable chunk carries a [`Resolution`] the UI drives (apply / ignore, per
//! side) — the "apply non-conflicting changes" toolbar just bulk-applies the clean
//! ones. Comparison can ignore whitespace via [`WhitespaceMode`]. Pure Rust.

use similar::TextDiff;
use std::ops::Range;

/// How lines are COMPARED (display always shows the original text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhitespaceMode {
    /// Byte-exact comparison.
    #[default]
    Exact,
    /// Ignore leading/trailing whitespace (compare trimmed lines).
    Trim,
    /// Ignore ALL whitespace (compare lines with every whitespace char removed).
    IgnoreAll,
}

impl WhitespaceMode {
    /// The comparison key for one line under this mode.
    pub(crate) fn key(self, line: &str) -> String {
        match self {
            WhitespaceMode::Exact => line.to_string(),
            WhitespaceMode::Trim => line.trim().to_string(),
            WhitespaceMode::IgnoreAll => line.chars().filter(|c| !c.is_whitespace()).collect(),
        }
    }

    /// Whether two slices of lines compare equal under this mode.
    fn eq_lines(self, a: &[String], b: &[String]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(x, y)| self.key(x) == self.key(y))
    }
}

/// Which side(s) changed a [`MergeChunk`] relative to the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Identical on all three sides.
    Stable,
    /// Only "ours" changed — clean, driven by [`Resolution::ours`].
    Ours,
    /// Only "theirs" changed — clean, driven by [`Resolution::theirs`].
    Theirs,
    /// Both sides made the SAME change — clean, driven by [`Resolution::ours`].
    Same,
    /// Both sides changed the region differently — both sides need a decision.
    Conflict,
}

/// One aligned region of the 3-way merge: line ranges (0-based, half-open) into each
/// side's lines. Boundaries are sync points — every side agrees with the base outside
/// change regions — so the three ranges always describe the same logical region.
#[derive(Debug, Clone)]
pub struct MergeChunk {
    /// What changed here (and on which side).
    pub kind: ChunkKind,
    /// Line range in the base.
    pub base: Range<usize>,
    /// Line range in "ours".
    pub ours: Range<usize>,
    /// Line range in "theirs".
    pub theirs: Range<usize>,
}

impl MergeChunk {
    /// Whether every side of this chunk that needs a decision has one (stable chunks
    /// are always decided; `Same` is a single decision carried on the ours side).
    #[must_use]
    pub fn decided(&self, r: Resolution) -> bool {
        match self.kind {
            ChunkKind::Stable => true,
            ChunkKind::Ours | ChunkKind::Same => r.ours != SideState::Pending,
            ChunkKind::Theirs => r.theirs != SideState::Pending,
            ChunkKind::Conflict => r.resolved(),
        }
    }
}

/// How one side of a chunk has been dealt with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SideState {
    /// Not yet decided — the chunk is unresolved while a relevant side is pending.
    #[default]
    Pending,
    /// That side's lines are applied into the result.
    Applied,
    /// That side's change is discarded.
    Ignored,
}

/// Per-chunk resolution: what to do with each side's change. Clean chunks read only
/// their own side (`Same` reads `ours`); a conflict is resolved once BOTH sides are
/// non-[`SideState::Pending`] — with both applied, the result carries ours' lines then
/// theirs' (the accept-both ordering).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resolution {
    /// Decision for the "ours" side.
    pub ours: SideState,
    /// Decision for the "theirs" side.
    pub theirs: SideState,
}

impl Resolution {
    /// Whether both sides have been decided (the CONFLICT-chunk gate; clean chunks use
    /// [`MergeChunk::decided`]).
    #[must_use]
    pub fn resolved(self) -> bool {
        self.ours != SideState::Pending && self.theirs != SideState::Pending
    }

    /// A resolution with both sides applied (accept-both).
    #[must_use]
    pub fn both_applied() -> Self {
        Resolution {
            ours: SideState::Applied,
            theirs: SideState::Applied,
        }
    }
}

/// A computed 3-way merge: the three texts split into lines + the aligned chunks
/// (stable regions included, in order, covering all three sides end to end).
#[derive(Debug)]
pub struct Merge3 {
    /// The common-ancestor text, split on `'\n'`.
    pub base: Vec<String>,
    /// The current branch's text ("ours"), split on `'\n'`.
    pub ours: Vec<String>,
    /// The incoming branch's text ("theirs"), split on `'\n'`.
    pub theirs: Vec<String>,
    /// The aligned regions, in order.
    pub chunks: Vec<MergeChunk>,
    /// The whitespace mode the chunks were computed under.
    pub ws: WhitespaceMode,
}

impl Merge3 {
    /// [`Self::compute_with`] under byte-exact comparison.
    #[must_use]
    pub fn compute(base: &str, ours: &str, theirs: &str) -> Self {
        Self::compute_with(base, ours, theirs, WhitespaceMode::Exact)
    }

    /// Compute the 3-way merge alignment of `ours` and `theirs` over `base`, comparing
    /// lines under `ws` (whitespace-only differences vanish under `Trim`/`IgnoreAll`).
    ///
    /// Change regions that merely TOUCH (no stable base line between them) are merged
    /// into one region — same as git, which refuses to auto-merge adjacent edits.
    ///
    /// ```
    /// use kyde_diff::merge::{ChunkKind, Merge3};
    /// let m = Merge3::compute("a\nb\nc\n", "A\nb\nc\n", "a\nb\nC\n");
    /// let kinds: Vec<_> = m.chunks.iter().map(|c| c.kind).collect();
    /// assert_eq!(
    ///     kinds,
    ///     [ChunkKind::Ours, ChunkKind::Stable, ChunkKind::Theirs, ChunkKind::Stable]
    /// );
    /// assert_eq!(m.conflicts(), 0); // the changes don't overlap
    /// ```
    #[must_use]
    pub fn compute_with(base: &str, ours: &str, theirs: &str, ws: WhitespaceMode) -> Self {
        // Split on '\n' (NOT `.lines()`) so indices match the editors — a trailing
        // newline becomes a final empty line on every side (see `FileDiff::compute`).
        let base_r: Vec<&str> = base.split('\n').collect();
        let ours_r: Vec<&str> = ours.split('\n').collect();
        let theirs_r: Vec<&str> = theirs.split('\n').collect();
        let ours_hunks = side_hunks(&base_r, &ours_r, ws);
        let theirs_hunks = side_hunks(&base_r, &theirs_r, ws);

        let to_owned = |v: &[&str]| -> Vec<String> {
            v.iter().map(std::string::ToString::to_string).collect()
        };
        let mut out = Merge3 {
            base: to_owned(&base_r),
            ours: to_owned(&ours_r),
            theirs: to_owned(&theirs_r),
            chunks: Vec::new(),
            ws,
        };

        // Walk both hunk lists in base order, growing each change region until no more
        // hunks overlap/touch it, with stable regions emitted between. `o_pos`/`t_pos`
        // track the side line matching `base_pos` (valid at sync points only).
        let (mut oi, mut ti) = (0usize, 0usize);
        let mut base_pos = 0usize;
        let (mut o_pos, mut t_pos) = (0usize, 0usize);
        while oi < ours_hunks.len() || ti < theirs_hunks.len() {
            let next_o = ours_hunks.get(oi).map(|h| h.0.start);
            let next_t = theirs_hunks.get(ti).map(|h| h.0.start);
            let start = match (next_o, next_t) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => break,
            };
            // Stable run up to the region start.
            let stable = start - base_pos;
            if stable > 0 {
                out.chunks.push(MergeChunk {
                    kind: ChunkKind::Stable,
                    base: base_pos..start,
                    ours: o_pos..o_pos + stable,
                    theirs: t_pos..t_pos + stable,
                });
                o_pos += stable;
                t_pos += stable;
            }
            // Absorb every hunk that overlaps or touches the region; `<=` makes touching
            // hunks (insert at the boundary, back-to-back edits) one region → conflict,
            // matching git's refusal to auto-merge changes with no stable line between.
            let mut end = start;
            let (mut o_used, mut t_used) = (false, false);
            let (mut o_delta, mut t_delta) = (0isize, 0isize);
            loop {
                let mut grew = false;
                while oi < ours_hunks.len() && ours_hunks[oi].0.start <= end {
                    end = end.max(ours_hunks[oi].0.end);
                    o_delta += ours_hunks[oi].1.len() as isize - ours_hunks[oi].0.len() as isize;
                    o_used = true;
                    grew = true;
                    oi += 1;
                }
                while ti < theirs_hunks.len() && theirs_hunks[ti].0.start <= end {
                    end = end.max(theirs_hunks[ti].0.end);
                    t_delta +=
                        theirs_hunks[ti].1.len() as isize - theirs_hunks[ti].0.len() as isize;
                    t_used = true;
                    grew = true;
                    ti += 1;
                }
                if !grew {
                    break;
                }
            }
            let span = end - start;
            // Region boundaries are sync points, so each side's range is the base span
            // shifted by that side's net insert/delete delta inside the region.
            let o_end = (o_pos + span).saturating_add_signed(o_delta);
            let t_end = (t_pos + span).saturating_add_signed(t_delta);
            let (ours_range, theirs_range) = (o_pos..o_end, t_pos..t_end);
            let kind = match (o_used, t_used) {
                (true, false) => ChunkKind::Ours,
                (false, true) => ChunkKind::Theirs,
                _ => {
                    if ws.eq_lines(
                        &out.ours[ours_range.clone()],
                        &out.theirs[theirs_range.clone()],
                    ) {
                        ChunkKind::Same
                    } else {
                        ChunkKind::Conflict
                    }
                }
            };
            out.chunks.push(MergeChunk {
                kind,
                base: start..end,
                ours: ours_range,
                theirs: theirs_range,
            });
            base_pos = end;
            o_pos = o_end;
            t_pos = t_end;
        }
        // Trailing stable run.
        if base_pos < out.base.len() {
            out.chunks.push(MergeChunk {
                kind: ChunkKind::Stable,
                base: base_pos..out.base.len(),
                ours: o_pos..out.ours.len(),
                theirs: t_pos..out.theirs.len(),
            });
        }
        out
    }

    /// Number of [`ChunkKind::Conflict`] chunks.
    #[must_use]
    pub fn conflicts(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Conflict)
            .count()
    }

    /// Number of chunks still awaiting a decision under `resolution` (clean AND
    /// conflict chunks — the file-ready gate).
    pub fn undecided(&self, resolution: impl Fn(usize) -> Resolution) -> usize {
        self.chunks
            .iter()
            .enumerate()
            .filter(|(i, c)| !c.decided(resolution(*i)))
            .count()
    }

    /// Build the merge result as lines, plus each chunk's line range within it.
    /// EVERY non-stable chunk follows `resolution(chunk_index)` — nothing is applied
    /// automatically: an undecided/ignored chunk keeps the BASE lines. A conflict with
    /// both sides applied carries ours' lines then theirs'.
    ///
    /// ```
    /// use kyde_diff::merge::{Merge3, Resolution, SideState};
    /// let m = Merge3::compute("a\nb\nc\nd\ne\n", "A\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n");
    /// // Nothing decided → the result is still the base.
    /// assert_eq!(m.result_lines(|_| Resolution::default()).0.join("\n"), "a\nb\nc\nd\ne\n");
    /// // Apply everything (the "»« All" toolbar action) → both clean changes land.
    /// let (lines, ranges) = m.result_lines(|_| Resolution::both_applied());
    /// assert_eq!(lines.join("\n"), "A\nb\nc\nd\nE\n");
    /// assert_eq!(ranges.len(), m.chunks.len());
    /// ```
    pub fn result_lines(
        &self,
        resolution: impl Fn(usize) -> Resolution,
    ) -> (Vec<String>, Vec<Range<usize>>) {
        let mut out: Vec<String> = Vec::with_capacity(self.base.len());
        let mut ranges = Vec::with_capacity(self.chunks.len());
        for (idx, c) in self.chunks.iter().enumerate() {
            let start = out.len();
            let r = resolution(idx);
            match c.kind {
                ChunkKind::Stable => out.extend_from_slice(&self.base[c.base.clone()]),
                ChunkKind::Ours | ChunkKind::Same => {
                    if r.ours == SideState::Applied {
                        out.extend_from_slice(&self.ours[c.ours.clone()]);
                    } else {
                        out.extend_from_slice(&self.base[c.base.clone()]);
                    }
                }
                ChunkKind::Theirs => {
                    if r.theirs == SideState::Applied {
                        out.extend_from_slice(&self.theirs[c.theirs.clone()]);
                    } else {
                        out.extend_from_slice(&self.base[c.base.clone()]);
                    }
                }
                ChunkKind::Conflict => {
                    if r.ours == SideState::Applied {
                        out.extend_from_slice(&self.ours[c.ours.clone()]);
                    }
                    if r.theirs == SideState::Applied {
                        out.extend_from_slice(&self.theirs[c.theirs.clone()]);
                    }
                    if r.ours != SideState::Applied && r.theirs != SideState::Applied {
                        out.extend_from_slice(&self.base[c.base.clone()]);
                    }
                }
            }
            ranges.push(start..out.len());
        }
        (out, ranges)
    }

    /// The merge result as text (see [`Self::result_lines`]). Joining on `'\n'` restores
    /// the trailing newline carried as a final empty line, byte-for-byte.
    pub fn merged_text(&self, resolution: impl Fn(usize) -> Resolution) -> String {
        self.result_lines(resolution).0.join("\n")
    }
}

/// Line-level change hunks of one side against the base: `(base_range, side_range)`,
/// sorted, non-overlapping. Same accumulation as `FileDiff::compute` (an insertion's
/// empty base range sits at its splice point, keeping alignment). Under a non-exact
/// [`WhitespaceMode`] the diff runs over normalized comparison keys, so whitespace-only
/// edits produce no hunk — ranges still index the ORIGINAL lines (1:1 mapping).
fn side_hunks(
    base: &[&str],
    side: &[&str],
    ws: WhitespaceMode,
) -> Vec<(Range<usize>, Range<usize>)> {
    fn collect(diff: &TextDiff<'_, '_, str>) -> Vec<(Range<usize>, Range<usize>)> {
        let mut hunks = Vec::new();
        for group in diff.grouped_ops(0) {
            let mut b_lo = usize::MAX;
            let mut b_hi = 0usize;
            let mut s_lo = usize::MAX;
            let mut s_hi = 0usize;
            let mut changed = false;
            for op in &group {
                if op.tag() == similar::DiffTag::Equal {
                    continue;
                }
                changed = true;
                let b = op.old_range();
                let s = op.new_range();
                b_lo = b_lo.min(b.start);
                b_hi = b_hi.max(b.end);
                s_lo = s_lo.min(s.start);
                s_hi = s_hi.max(s.end);
            }
            if changed {
                hunks.push((b_lo..b_hi.max(b_lo), s_lo..s_hi.max(s_lo)));
            }
        }
        hunks
    }
    if ws == WhitespaceMode::Exact {
        return collect(&TextDiff::from_slices(base, side));
    }
    let base_keys: Vec<String> = base.iter().map(|l| ws.key(l)).collect();
    let side_keys: Vec<String> = side.iter().map(|l| ws.key(l)).collect();
    let base_refs: Vec<&str> = base_keys.iter().map(String::as_str).collect();
    let side_refs: Vec<&str> = side_keys.iter().map(String::as_str).collect();
    collect(&TextDiff::from_slices(&base_refs, &side_refs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(m: &Merge3) -> Vec<ChunkKind> {
        m.chunks.iter().map(|c| c.kind).collect()
    }

    /// Everything applied — what the "»« All" toolbar action produces.
    fn all(_: usize) -> Resolution {
        Resolution::both_applied()
    }

    #[test]
    fn disjoint_changes_apply_cleanly_once_applied() {
        let m = Merge3::compute("a\nb\nc\nd\ne\n", "A\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n");
        assert_eq!(
            kinds(&m),
            [
                ChunkKind::Ours,
                ChunkKind::Stable,
                ChunkKind::Theirs,
                ChunkKind::Stable
            ]
        );
        assert_eq!(m.conflicts(), 0);
        // Pending → base; applied → both clean changes land.
        assert_eq!(m.merged_text(|_| Resolution::default()), "a\nb\nc\nd\ne\n");
        assert_eq!(m.undecided(|_| Resolution::default()), 2);
        assert_eq!(m.merged_text(all), "A\nb\nc\nd\nE\n");
        assert_eq!(m.undecided(all), 0);
    }

    #[test]
    fn identical_changes_collapse_to_same() {
        let m = Merge3::compute("a\nb\nc\n", "a\nX\nc\n", "a\nX\nc\n");
        assert_eq!(
            kinds(&m),
            [ChunkKind::Stable, ChunkKind::Same, ChunkKind::Stable]
        );
        // `Same` is a single decision carried on the ours side.
        let take_ours = |_: usize| Resolution {
            ours: SideState::Applied,
            theirs: SideState::Pending,
        };
        assert_eq!(m.merged_text(take_ours), "a\nX\nc\n");
        assert_eq!(m.undecided(take_ours), 0);
    }

    #[test]
    fn overlapping_different_changes_conflict_and_resolve() {
        let m = Merge3::compute("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        assert_eq!(m.conflicts(), 1);
        let ci = m
            .chunks
            .iter()
            .position(|c| c.kind == ChunkKind::Conflict)
            .unwrap();
        // Pending → base kept.
        assert_eq!(m.merged_text(|_| Resolution::default()), "a\nb\nc\n");
        let take = |ours, theirs| {
            m.merged_text(|i| {
                if i == ci {
                    Resolution { ours, theirs }
                } else {
                    Resolution::default()
                }
            })
        };
        assert_eq!(take(SideState::Applied, SideState::Ignored), "a\nOURS\nc\n");
        assert_eq!(
            take(SideState::Ignored, SideState::Applied),
            "a\nTHEIRS\nc\n"
        );
        // Both applied → ours then theirs.
        assert_eq!(
            take(SideState::Applied, SideState::Applied),
            "a\nOURS\nTHEIRS\nc\n"
        );
        // Both ignored → base survives.
        assert_eq!(take(SideState::Ignored, SideState::Ignored), "a\nb\nc\n");
    }

    /// Touching (adjacent) edits with no stable line between them must merge into ONE
    /// conflict region — git refuses to auto-merge these, and so do we.
    #[test]
    fn touching_changes_form_one_conflict() {
        // ours edits line b (idx 1), theirs edits line c (idx 2) — adjacent, no gap.
        let m = Merge3::compute("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nC\nd\n");
        assert_eq!(m.conflicts(), 1, "adjacent edits are one conflict: {m:?}");
        let c = &m
            .chunks
            .iter()
            .find(|c| c.kind == ChunkKind::Conflict)
            .unwrap();
        assert_eq!(c.base, 1..3, "the conflict spans both touched lines");
    }

    #[test]
    fn insertions_at_the_same_point_conflict() {
        let m = Merge3::compute("a\nb\n", "a\nX\nb\n", "a\nY\nb\n");
        assert_eq!(m.conflicts(), 1);
        // Insert-vs-insert has an EMPTY base range at the splice point.
        let c = m
            .chunks
            .iter()
            .find(|c| c.kind == ChunkKind::Conflict)
            .unwrap();
        assert_eq!(c.base.len(), 0);
        assert_eq!(c.ours.len(), 1);
        assert_eq!(c.theirs.len(), 1);
    }

    #[test]
    fn deletion_vs_edit_conflicts_and_pure_deletion_applies() {
        // ours deletes b, theirs edits b → conflict.
        let m = Merge3::compute("a\nb\nc\n", "a\nc\n", "a\nB!\nc\n");
        assert_eq!(m.conflicts(), 1);
        // ours deletes b, theirs untouched → clean once applied.
        let m = Merge3::compute("a\nb\nc\n", "a\nc\n", "a\nb\nc\n");
        assert_eq!(m.conflicts(), 0);
        assert_eq!(m.merged_text(all), "a\nc\n");
    }

    #[test]
    fn result_ranges_index_the_result_lines() {
        let m = Merge3::compute("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        let (lines, ranges) = m.result_lines(all);
        assert_eq!(ranges.len(), m.chunks.len());
        // Ranges tile the result exactly: contiguous, in order, covering everything.
        let mut pos = 0;
        for r in &ranges {
            assert_eq!(r.start, pos);
            pos = r.end;
        }
        assert_eq!(pos, lines.len());
    }

    #[test]
    fn identical_inputs_are_all_stable() {
        let m = Merge3::compute("a\nb\n", "a\nb\n", "a\nb\n");
        assert_eq!(kinds(&m), [ChunkKind::Stable]);
        assert_eq!(m.merged_text(|_| Resolution::default()), "a\nb\n");
        assert_eq!(m.undecided(|_| Resolution::default()), 0);
    }

    /// Whitespace modes: an indentation-only edit vanishes under `Trim`; a spacing-only
    /// edit vanishes under `IgnoreAll`; and two sides differing only in whitespace
    /// downgrade from Conflict to Same.
    #[test]
    fn whitespace_modes_ignore_ws_only_changes() {
        // ours re-indents line b; theirs is untouched.
        let (base, ours, theirs) = ("a\nb\nc\n", "a\n    b\nc\n", "a\nb\nc\n");
        let exact = Merge3::compute(base, ours, theirs);
        assert_eq!(
            exact
                .chunks
                .iter()
                .filter(|c| c.kind == ChunkKind::Ours)
                .count(),
            1
        );
        let trim = Merge3::compute_with(base, ours, theirs, WhitespaceMode::Trim);
        assert_eq!(
            kinds(&trim),
            [ChunkKind::Stable],
            "an indent-only change must vanish under Trim"
        );

        // ours respaces inside the line — survives Trim, vanishes under IgnoreAll.
        let (base, ours) = ("let x=1;\n", "let x = 1;\n");
        let trim = Merge3::compute_with(base, ours, base, WhitespaceMode::Trim);
        assert_eq!(
            trim.chunks
                .iter()
                .filter(|c| c.kind == ChunkKind::Ours)
                .count(),
            1
        );
        let all_ws = Merge3::compute_with(base, ours, base, WhitespaceMode::IgnoreAll);
        assert_eq!(kinds(&all_ws), [ChunkKind::Stable]);

        // Both sides change the same line, differing only in whitespace → Same, not
        // Conflict, under IgnoreAll.
        let m = Merge3::compute_with("a\n", "x( 1 )\n", "x(1)\n", WhitespaceMode::IgnoreAll);
        assert_eq!(
            m.chunks
                .iter()
                .filter(|c| c.kind == ChunkKind::Same)
                .count(),
            1,
            "ws-only divergence must collapse to Same: {m:?}"
        );
        assert_eq!(m.conflicts(), 0);
    }

    /// Performance regression guard — see CLAUDE.md "Performance regression tests".
    /// `compute` runs on every conflicted-file selection in the merge view; the region
    /// walk must stay linear-ish. Budget deliberately loose (algorithmic blowups, not
    /// CI jitter).
    #[test]
    fn perf_compute_large_merge_stays_fast() {
        let base: String = (0..4000).map(|i| format!("line {i}\n")).collect();
        let ours: String = (0..4000)
            .map(|i| {
                if i % 10 == 0 {
                    format!("OURS {i}\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let theirs: String = (0..4000)
            .map(|i| {
                if i % 10 == 5 {
                    format!("THEIRS {i}\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let start = std::time::Instant::now();
        let m = Merge3::compute(&base, &ours, &theirs);
        let (lines, _) = m.result_lines(all);
        let elapsed = start.elapsed();
        assert!(m.conflicts() == 0 && !lines.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "Merge3::compute on 4000 lines took {elapsed:?} (budget 2s) — perf regression?"
        );
    }
}
