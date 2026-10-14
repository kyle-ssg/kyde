//! Commit-graph lane layout for the History log (issue #92): assigns each commit a column
//! and lists the line segments to draw around its node, IDE "Git log" style. Pure — the UI
//! paints the segments. Input is newest-first in an order where no parent precedes any of
//! its children (`git log --date-order` / `--topo-order`).

/// A line segment in one half of a row: from column `from` to column `to`, drawn in the
/// lane's `color` (an index the UI maps onto a palette).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    /// Column at the segment's start (top of the half).
    pub from: usize,
    /// Column at the segment's end (bottom of the half).
    pub to: usize,
    /// Colour index of the lane.
    pub color: usize,
}

/// One commit's slice of the graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphRow {
    /// Column of the commit's node.
    pub lane: usize,
    /// Colour index of the node.
    pub color: usize,
    /// Segments from the row's top edge down to the node's line: lanes passing through,
    /// and children converging onto the node.
    pub up: Vec<Edge>,
    /// Segments from the node's line down to the row's bottom edge: lanes passing through,
    /// and the node branching out to its parents.
    pub down: Vec<Edge>,
}

impl GraphRow {
    /// Columns this row spans (one past the rightmost column it draws in).
    pub fn width(&self) -> usize {
        self.up
            .iter()
            .chain(&self.down)
            .map(|e| e.from.max(e.to) + 1)
            .chain([self.lane + 1])
            .max()
            .unwrap_or(1)
    }
}

/// Lay out `commits` (`(hash, parent hashes)`, newest first). Each open lane waits for one
/// commit (the parent its line leads to): a commit takes the leftmost lane waiting for it
/// (or a free one), every other lane waiting for it converges onto it, its first parent
/// continues its lane, and further parents open lanes — or join one already waiting for
/// them. Parents beyond the list (a truncated log) leave their lanes running off the bottom.
///
/// ```
/// use kyde_git::graph::layout;
/// let rows = layout(&[("b", vec!["a"]), ("a", vec![])]);
/// assert_eq!((rows[0].lane, rows[1].lane), (0, 0));
/// ```
pub fn layout<H: AsRef<str>>(commits: &[(H, Vec<H>)]) -> Vec<GraphRow> {
    // Each lane: (the hash it's waiting for, its colour).
    let mut lanes: Vec<Option<(String, usize)>> = Vec::new();
    let mut next_color = 0usize;
    let mut fresh = || {
        next_color += 1;
        next_color - 1
    };
    let free_slot = |lanes: &mut Vec<Option<(String, usize)>>| {
        lanes.iter().position(Option::is_none).unwrap_or_else(|| {
            lanes.push(None);
            lanes.len() - 1
        })
    };
    let waiting = |lanes: &[Option<(String, usize)>], h: &str| {
        lanes
            .iter()
            .position(|l| l.as_ref().is_some_and(|(w, _)| w == h))
    };
    let mut out = Vec::with_capacity(commits.len());
    for (hash, parents) in commits {
        let hash = hash.as_ref();
        let hits: Vec<usize> = (0..lanes.len())
            .filter(|&i| lanes[i].as_ref().is_some_and(|(w, _)| w == hash))
            .collect();
        let (lane, color) = match hits.first() {
            Some(&i) => (i, lanes[i].as_ref().map_or(0, |(_, c)| *c)),
            None => (free_slot(&mut lanes), fresh()),
        };
        // Top half: every open lane runs down; those waiting for this commit bend into it.
        let up: Vec<Edge> = lanes
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                let (_, c) = l.as_ref()?;
                let to = if hits.contains(&i) { lane } else { i };
                Some(Edge {
                    from: i,
                    to,
                    color: *c,
                })
            })
            .collect();
        for &i in &hits {
            lanes[i] = None;
        }
        // Bottom half: the node fans out to its parents; other open lanes run on.
        let mut passing: Vec<usize> = (0..lanes.len()).filter(|&i| lanes[i].is_some()).collect();
        let mut down: Vec<Edge> = Vec::new();
        for (k, p) in parents.iter().enumerate() {
            let p = p.as_ref();
            match (k, waiting(&lanes, p)) {
                // Another line already awaits this parent further right: the first-parent
                // line keeps its column and that line bends in to meet it — so a branch's
                // base stays on the leftmost line.
                (0, Some(j)) if j > lane => {
                    let cj = lanes[j].as_ref().map_or(color, |(_, c)| *c);
                    lanes[j] = None;
                    lanes[lane] = Some((p.to_string(), color));
                    passing.retain(|&i| i != j);
                    down.push(Edge {
                        from: lane,
                        to: lane,
                        color,
                    });
                    down.push(Edge {
                        from: j,
                        to: lane,
                        color: cj,
                    });
                }
                // Awaited further left (or by a lane this node already opened) — join it.
                (_, Some(j)) => {
                    let c = if k == 0 {
                        color
                    } else {
                        lanes[j].as_ref().map_or(color, |(_, c)| *c)
                    };
                    down.push(Edge {
                        from: lane,
                        to: j,
                        color: c,
                    });
                }
                (_, None) => {
                    let (j, c) = if k == 0 {
                        (lane, color)
                    } else {
                        (free_slot(&mut lanes), fresh())
                    };
                    lanes[j] = Some((p.to_string(), c));
                    down.push(Edge {
                        from: lane,
                        to: j,
                        color: c,
                    });
                }
            }
        }
        down.extend(passing.into_iter().filter_map(|i| {
            let (_, c) = lanes[i].as_ref()?;
            Some(Edge {
                from: i,
                to: i,
                color: *c,
            })
        }));
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        out.push(GraphRow {
            lane,
            color,
            up,
            down,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A branch + merge: `m` merges `b` (second parent) into `a`, both off `c`. `b` gets a
    /// second lane, both lanes converge back onto `c`, and colours follow their lanes.
    #[test]
    fn branch_and_merge_lanes() {
        let rows = layout(&[
            ("m", vec!["a", "b"]),
            ("b", vec!["c"]),
            ("a", vec!["c"]),
            ("c", vec![]),
        ]);
        let lanes: Vec<usize> = rows.iter().map(|r| r.lane).collect();
        assert_eq!(lanes, [0, 1, 0, 0], "the base stays on the leftmost line");
        // The merge fans out to lane 0 (first parent) and a new lane 1 (second parent).
        assert_eq!(
            rows[0].down.iter().map(|e| e.to).collect::<Vec<_>>(),
            [0, 1]
        );
        assert_ne!(rows[0].down[0].color, rows[0].down[1].color);
        assert_eq!(
            rows[1].color, rows[0].down[1].color,
            "b keeps its lane's colour"
        );
        // `b`'s line, awaiting `c` in lane 1, bends into lane 0 under `a`.
        assert!(rows[2].down.iter().any(|e| e.from == 1 && e.to == 0));
        assert_eq!(rows.iter().map(GraphRow::width).max(), Some(2));
    }
}
