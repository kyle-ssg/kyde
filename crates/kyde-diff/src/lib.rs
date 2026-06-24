//! Diff model. Line-level hunks (for the side-by-side render + center-gutter
//! chevrons) plus word-level ranges inside changed lines (the inline highlight
//! in image 4). Built on `similar`. Pure Rust — compiles standalone.
//!
//! Two-phase, exactly like Zed/IntelliJ: diff lines, then re-diff each changed
//! region at word granularity to color only the words that actually changed.

use similar::{ChangeTag, TextDiff};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    Added,
    Deleted,
    Modified,
}

/// One change region. Line ranges are 0-based, half-open, into each side's lines.
#[derive(Debug, Clone)]
pub struct Hunk {
    pub kind: HunkKind,
    pub old_range: Range<usize>, // lines in "before"
    pub new_range: Range<usize>, // lines in "after"
    /// Per-changed-line word ranges to highlight (byte ranges within that line).
    pub old_word_ranges: Vec<(usize, Range<usize>)>, // (line idx, byte range)
    pub new_word_ranges: Vec<(usize, Range<usize>)>,
}

#[derive(Clone)]
pub struct FileDiff {
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn compute(before: &str, after: &str) -> Self {
        // Split on '\n' (NOT `.lines()`) so line indices match exactly how the editor
        // renders them — a trailing newline becomes a final empty line on both sides, so
        // identical text never shows a phantom diff and alignment stays in sync.
        let old_refs: Vec<&str> = before.split('\n').collect();
        let new_refs: Vec<&str> = after.split('\n').collect();
        let old: Vec<String> = old_refs.iter().map(|s| s.to_string()).collect();
        let new: Vec<String> = new_refs.iter().map(|s| s.to_string()).collect();

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

            let (ow, nw) = if kind == HunkKind::Modified {
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
}

/// `(line_idx, byte_range)` pairs for one diff side's word-level highlights.
type WordRanges = Vec<(usize, Range<usize>)>;

/// Re-diff changed regions word-by-word; return (line_idx, byte_range) to highlight.
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
}
