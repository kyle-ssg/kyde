#![deny(missing_docs)]
//! Diff model. Line-level hunks (for the side-by-side render + center-gutter
//! chevrons) plus word-level ranges inside changed lines (the inline highlight
//! in image 4). Built on `similar`. Pure Rust — compiles standalone.
//!
//! Two-phase, exactly like Zed/IntelliJ: diff lines, then re-diff each changed
//! region at word granularity to color only the words that actually changed.

use similar::{ChangeTag, TextDiff};
use std::ops::Range;

/// What kind of change a [`Hunk`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    /// Lines present only in the "after" side.
    Added,
    /// Lines present only in the "before" side.
    Deleted,
    /// Lines that differ between the two sides (paired old/new).
    Modified,
}

/// One change region. Line ranges are 0-based, half-open, into each side's lines.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// Whether this region was added, deleted, or modified.
    pub kind: HunkKind,
    /// Line range in the "before" side (half-open).
    pub old_range: Range<usize>,
    /// Line range in the "after" side (half-open).
    pub new_range: Range<usize>,
    /// Per-changed-line word ranges to highlight on the old side: `(line index, byte range)`.
    pub old_word_ranges: Vec<(usize, Range<usize>)>,
    /// Per-changed-line word ranges to highlight on the new side: `(line index, byte range)`.
    pub new_word_ranges: Vec<(usize, Range<usize>)>,
}

/// A computed diff between two texts: both sides split into lines, plus the change [`Hunk`]s.
#[derive(Clone)]
pub struct FileDiff {
    /// The "before" text, split into lines.
    pub old: Vec<String>,
    /// The "after" text, split into lines.
    pub new: Vec<String>,
    /// The change regions between `old` and `new`, in order.
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// Compute the line- and word-level diff between `before` and `after`.
    ///
    /// ```
    /// use kyde_diff::FileDiff;
    /// let d = FileDiff::compute("a\nb\nc\n", "a\nB\nc\n");
    /// assert_eq!(d.old.len(), d.new.len());
    /// assert_eq!(d.hunks.len(), 1); // only the middle line changed
    /// ```
    pub fn compute(before: &str, after: &str) -> Self {
        // Split on '\n' (NOT `.lines()`) so line indices match exactly how the editor
        // renders them — a trailing newline becomes a final empty line on both sides, so
        // identical text never shows a phantom diff and alignment stays in sync.
        let old_refs: Vec<&str> = before.split('\n').collect();
        let new_refs: Vec<&str> = after.split('\n').collect();
        let old: Vec<String> = old_refs
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let new: Vec<String> = new_refs
            .iter()
            .map(std::string::ToString::to_string)
            .collect();

        // Diff over those exact line vectors (consistent with `old`/`new` indices).
        let diff = TextDiff::from_slices(&old_refs, &new_refs);
        let mut hunks = Vec::new();

        // Group contiguous non-equal ops (similar gives us grouped ops with context=0).
        for group in diff.grouped_ops(0) {
            // Derive ranges from the ops directly (NOT from iter_changes): a pure insertion
            // has no Delete changes, so tracking the old-side position only from deletes
            // collapsed `old_range` to 0..0 and wrecked alignment. `op.old_range()` carries
            // the insertion point even for an Insert (its empty range sits at the right line).
            let mut old_lo = usize::MAX;
            let mut old_hi = 0usize;
            let mut new_lo = usize::MAX;
            let mut new_hi = 0usize;
            let mut has_del = false;
            let mut has_ins = false;

            for op in &group {
                use similar::DiffTag;
                match op.tag() {
                    DiffTag::Equal => continue,
                    DiffTag::Delete => has_del = true,
                    DiffTag::Insert => has_ins = true,
                    DiffTag::Replace => {
                        has_del = true;
                        has_ins = true;
                    }
                }
                let o = op.old_range();
                let n = op.new_range();
                old_lo = old_lo.min(o.start);
                old_hi = old_hi.max(o.end);
                new_lo = new_lo.min(n.start);
                new_hi = new_hi.max(n.end);
            }
            if !has_del && !has_ins {
                continue;
            }
            let kind = match (has_del, has_ins) {
                (true, true) => HunkKind::Modified,
                (false, true) => HunkKind::Added,
                (true, false) => HunkKind::Deleted,
                (false, false) => unreachable!("guarded above"),
            };
            // For a pure insert/delete the empty side's range is `lo..lo` (positioned at the
            // splice point); for the changed side it's the real `lo..hi`.
            let old_range = if has_del {
                old_lo..old_hi
            } else {
                old_lo..old_lo
            };
            let new_range = if has_ins {
                new_lo..new_hi
            } else {
                new_lo..new_lo
            };

            let (ow, nw) = if kind == HunkKind::Modified
                && !word_diff_too_large(&old[old_range.clone()], &new[new_range.clone()])
            {
                word_ranges(
                    &old[old_range.clone()],
                    &new[new_range.clone()],
                    old_range.start,
                    new_range.start,
                )
            } else {
                (Vec::new(), Vec::new())
            };

            hunks.push(Hunk {
                kind,
                old_range,
                new_range,
                old_word_ranges: ow,
                new_word_ranges: nw,
            });
        }

        FileDiff { old, new, hunks }
    }

    /// Total `(added, removed)` line counts, summed over the hunks (a modified region
    /// counts on both sides). Matches `git diff --numstat` for the same pair of texts.
    #[must_use]
    pub fn stats(&self) -> (usize, usize) {
        self.hunks.iter().fold((0, 0), |(add, rem), h| {
            (add + h.new_range.len(), rem + h.old_range.len())
        })
    }

    /// Reconstruct the "after" text with only the hunks for which `include(hunk_index)`
    /// returns true applied; excluded hunks keep the base ("before") lines.
    ///
    /// This is the partial-commit primitive: the returned text is what gets staged when
    /// some of a file's changes are unticked in the diff gutter, so a commit can carry a
    /// subset of a file's changes (VSCode/IntelliJ-style partial commit).
    ///
    /// ```
    /// use kyde_diff::FileDiff;
    /// let d = FileDiff::compute("a\nb\nc\n", "a\nB\nc\n");
    /// assert_eq!(d.partial_new_content(|_| true), "a\nB\nc\n");
    /// assert_eq!(d.partial_new_content(|_| false), "a\nb\nc\n");
    /// ```
    pub fn partial_new_content(&self, include: impl Fn(usize) -> bool) -> String {
        let mut out: Vec<&str> = Vec::with_capacity(self.new.len());
        let mut cursor = 0usize; // consumed prefix of `old`
        for (i, h) in self.hunks.iter().enumerate() {
            // Unchanged region before this hunk (identical on both sides).
            out.extend(
                self.old[cursor..h.old_range.start]
                    .iter()
                    .map(String::as_str),
            );
            let side = if include(i) {
                &self.new[h.new_range.clone()]
            } else {
                &self.old[h.old_range.clone()]
            };
            out.extend(side.iter().map(String::as_str));
            cursor = h.old_range.end;
        }
        out.extend(self.old[cursor..].iter().map(String::as_str));
        // `compute` split both sides on '\n', so joining restores the text (and its
        // trailing newline, carried as a final empty element) byte-for-byte.
        out.join("\n")
    }
}

/// `(line_idx, byte_range)` pairs for one diff side's word-level highlights.
type WordRanges = Vec<(usize, Range<usize>)>;

/// Upper bound on the total lines (old + new) in a modified region before we skip the
/// inline word-diff. A lockfile / generated-code edit can change thousands of lines at
/// once; word-tokenizing them all is quadratic-ish and pointless (nobody scans per-word
/// highlights over a 5000-line replacement).
const MAX_WORD_DIFF_LINES: usize = 400;
/// Upper bound on any single line's byte length before we skip the inline word-diff.
/// Minified bundles are a single multi-hundred-KB line; joining + tokenizing that blows
/// memory and stalls the per-select diff for no useful highlight.
const MAX_WORD_DIFF_LINE_BYTES: usize = 2_000;

/// Whether a modified region is too big for the word-level (inline) diff — see the two
/// bounds above. When true, `compute` leaves the hunk as a whole-line change with no
/// per-word ranges, which keeps it responsive on pathological (machine-generated) files.
fn word_diff_too_large(old_lines: &[String], new_lines: &[String]) -> bool {
    old_lines.len() + new_lines.len() > MAX_WORD_DIFF_LINES
        || old_lines
            .iter()
            .chain(new_lines)
            .any(|l| l.len() > MAX_WORD_DIFF_LINE_BYTES)
}

/// Re-diff changed regions word-by-word; return (`line_idx`, `byte_range`) to highlight.
fn word_ranges(
    old_lines: &[String],
    new_lines: &[String],
    old_base: usize,
    new_base: usize,
) -> (WordRanges, WordRanges) {
    let old_join = old_lines.join("\n");
    let new_join = new_lines.join("\n");
    let diff = TextDiff::from_words(&old_join, &new_join);

    let mut old_out = Vec::new();
    let mut new_out = Vec::new();
    let (mut o_line, mut o_col) = (old_base, 0usize);
    let (mut n_line, mut n_col) = (new_base, 0usize);

    for change in diff.iter_all_changes() {
        let val = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                advance(val, &mut o_line, &mut o_col);
                advance(val, &mut n_line, &mut n_col);
            }
            ChangeTag::Delete => {
                mark(val, &mut o_line, &mut o_col, &mut old_out);
            }
            ChangeTag::Insert => {
                mark(val, &mut n_line, &mut n_col, &mut new_out);
            }
        }
    }
    (old_out, new_out)
}

fn advance(text: &str, line: &mut usize, col: &mut usize) {
    for ch in text.chars() {
        if ch == '\n' {
            *line += 1;
            *col = 0;
        } else {
            *col += ch.len_utf8();
        }
    }
}

fn mark(text: &str, line: &mut usize, col: &mut usize, out: &mut Vec<(usize, Range<usize>)>) {
    for seg in text.split_inclusive('\n') {
        let body = seg.strip_suffix('\n').unwrap_or(seg);
        if !body.is_empty() {
            out.push((*line, *col..*col + body.len()));
        }
        if seg.ends_with('\n') {
            *line += 1;
            *col = 0;
        } else {
            *col += body.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_modify_add_delete_and_identical() {
        let modified = FileDiff::compute("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(modified.hunks.len(), 1);
        assert_eq!(modified.hunks[0].kind, HunkKind::Modified);
        assert_eq!(modified.hunks[0].old_range, 1..2);

        let added = FileDiff::compute("a\nc\n", "a\nb\nc\n");
        assert_eq!(added.hunks[0].kind, HunkKind::Added);
        // The insertion point on the OLD side must be where the new line lands (after "a"),
        // NOT 0 — otherwise the side-by-side rows misalign (old "a" pairs with new "c").
        assert_eq!(added.hunks[0].old_range, 1..1);
        assert_eq!(added.hunks[0].new_range, 1..2);

        // Same guard for a deletion: the NEW-side position must be the splice point.
        let del = FileDiff::compute("a\nb\nc\n", "a\nc\n");
        assert_eq!(del.hunks[0].kind, HunkKind::Deleted);
        assert_eq!(del.hunks[0].old_range, 1..2);
        assert_eq!(del.hunks[0].new_range, 1..1);

        let deleted = FileDiff::compute("a\nb\nc\n", "a\nc\n");
        assert_eq!(deleted.hunks[0].kind, HunkKind::Deleted);

        assert!(FileDiff::compute("a\nb\n", "a\nb\n").hunks.is_empty());
    }

    #[test]
    fn stats_counts_added_and_removed_lines_across_hunks() {
        // b→B is +1 −1, the appended d/e is +2 — totals must span hunks.
        assert_eq!(
            FileDiff::compute("a\nb\nc\n", "a\nB\nc\nd\ne\n").stats(),
            (3, 1)
        );
        assert_eq!(FileDiff::compute("a\nb\n", "a\n").stats(), (0, 1));
        assert_eq!(FileDiff::compute("x\n", "x\n").stats(), (0, 0));
        // A new file is all additions.
        assert_eq!(FileDiff::compute("", "a\nb\n").stats(), (2, 0));
    }

    #[test]
    fn partial_content_all_or_nothing_reproduces_each_side() {
        let d = FileDiff::compute("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(d.partial_new_content(|_| true), "a\nB\nc\nd\n");
        assert_eq!(d.partial_new_content(|_| false), "a\nb\nc\n");
    }

    #[test]
    fn partial_content_applies_only_the_included_hunks() {
        // Two separated changes: a modification (b→B) and an insertion (x after d).
        let d = FileDiff::compute("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nx\ne\n");
        assert_eq!(d.hunks.len(), 2);
        // Only the first hunk: the modification lands, the insertion doesn't.
        assert_eq!(d.partial_new_content(|i| i == 0), "a\nB\nc\nd\ne\n");
        // Only the second hunk: the insertion lands, the modification doesn't.
        assert_eq!(d.partial_new_content(|i| i == 1), "a\nb\nc\nd\nx\ne\n");
    }

    #[test]
    fn partial_content_handles_deletions_and_new_files() {
        // Excluded deletion keeps the line; included deletion drops it.
        let d = FileDiff::compute("a\nb\nc\n", "a\nc\n");
        assert_eq!(d.partial_new_content(|_| false), "a\nb\nc\n");
        assert_eq!(d.partial_new_content(|_| true), "a\nc\n");

        // New (untracked) file: base is empty, the whole content is one Added hunk.
        let d = FileDiff::compute("", "x\ny\n");
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.partial_new_content(|_| true), "x\ny\n");
        assert_eq!(d.partial_new_content(|_| false), "")
    }

    #[test]
    fn word_ranges_mark_only_the_changed_word() {
        // Only "bravo"→"BRAVO" changed; the highlight range must cover just it.
        let d = FileDiff::compute("alpha bravo charlie\n", "alpha BRAVO charlie\n");
        let h = &d.hunks[0];
        assert!(
            !h.new_word_ranges.is_empty(),
            "expected an inline word range"
        );
        let (line, range) = h.new_word_ranges[0].clone();
        assert_eq!(line, 0);
        assert_eq!(&"alpha BRAVO charlie"[range], "BRAVO");
    }

    /// Performance regression guard — see CLAUDE.md "Performance regression tests".
    /// `compute` runs on every file selection; a quadratic regression would make
    /// large diffs janky. Budget is deliberately loose (catches algorithmic
    /// blowups, not CI jitter); on a dev machine this runs in a few ms.
    #[test]
    fn perf_compute_large_diff_stays_fast() {
        let before: String = (0..4000).map(|i| format!("line {i}\n")).collect();
        let after: String = (0..4000)
            .map(|i| {
                if i % 10 == 0 {
                    format!("LINE {i}\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let start = std::time::Instant::now();
        let d = FileDiff::compute(&before, &after);
        let elapsed = start.elapsed();
        assert!(!d.hunks.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "FileDiff::compute on 4000 lines took {elapsed:?} (budget 2s) — perf regression?"
        );
    }

    /// Hostile-input guard for the word-diff cap (see `word_diff_too_large`). The tame
    /// perf test above never exercises the pathological shapes that motivated the cap —
    /// a minified bundle (one enormous line) and a lockfile (thousands of lines changed
    /// at once). Without the cap, joining + word-tokenizing these stalls; with it, the
    /// inline pass is skipped and `compute` returns near-instantly.
    #[test]
    fn perf_hostile_word_diff_is_capped() {
        // Minified-file edit: a single ~500 KB line, one char changed.
        let before = format!("{}\n", "x".repeat(500_000));
        let after = format!("{}Y\n", "x".repeat(499_999));
        let start = std::time::Instant::now();
        let d = FileDiff::compute(&before, &after);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "hostile single-line word-diff exceeded budget — cap not applied?"
        );
        assert_eq!(d.hunks.len(), 1);
        assert!(
            d.hunks[0].new_word_ranges.is_empty(),
            "an over-long line must skip the inline word-diff"
        );

        // Lockfile-style edit: thousands of lines replaced at once.
        let many_before: String = (0..5000).map(|i| format!("a{i}\n")).collect();
        let many_after: String = (0..5000).map(|i| format!("b{i}\n")).collect();
        let start = std::time::Instant::now();
        let d = FileDiff::compute(&many_before, &many_after);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "hostile many-line word-diff exceeded budget — cap not applied?"
        );
        assert!(
            d.hunks
                .iter()
                .all(|h| h.old_word_ranges.is_empty() && h.new_word_ranges.is_empty()),
            "a huge modified region must skip the inline word-diff"
        );
    }
}
