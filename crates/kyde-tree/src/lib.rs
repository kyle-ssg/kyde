#![deny(missing_docs)]
//! A lazy file-tree model built from the flat, sorted list of repo files
//! (`Repo::list_files`, gitignored paths already excluded). Pure Rust, no UI framework — the
//! Browse pane turns `visible()` into IntelliJ-style indented rows.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// One child entry under a directory.
#[derive(Clone)]
struct Entry {
    path: PathBuf,
    is_dir: bool,
}

/// Directory → its immediate children (dirs first, then files; both case-insensitive).
/// The repo root is the empty path `""`.
#[derive(Default)]
pub struct Tree {
    children: BTreeMap<PathBuf, Vec<Entry>>,
}

/// A flattened, currently-visible row.
pub struct Row {
    /// Repo-relative path of this row's file or directory.
    pub path: PathBuf,
    /// True if this row is a directory (drawn with a chevron + folder icon).
    pub is_dir: bool,
    /// Nesting depth (0 = top level) → indentation.
    pub depth: usize,
}

impl Tree {
    /// Build from the flat file list. Every ancestor directory of every file becomes a node.
    pub fn build(files: &[PathBuf]) -> Self {
        Self::build_with_dirs(files, &[])
    }

    /// [`Tree::build`] plus explicit directory nodes for `dirs`. File-derived trees
    /// cannot see EMPTY directories (git and the file walk both list files only), so
    /// a just-created folder rides in here until its first file exists.
    pub fn build_with_dirs(files: &[PathBuf], dirs: &[PathBuf]) -> Self {
        // Per-parent dedup set while building, then sorted into `children`.
        let mut sets: BTreeMap<PathBuf, Vec<Entry>> = BTreeMap::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        // `(path, every component is a dir?)` — a file's LAST component is a file.
        let entries = files
            .iter()
            .map(|f| (f, false))
            .chain(dirs.iter().map(|d| (d, true)));
        for (file, all_dirs) in entries {
            let mut parent = PathBuf::new();
            let comps: Vec<_> = file.components().collect();
            for (i, comp) in comps.iter().enumerate() {
                let mut child = parent.clone();
                child.push(comp);
                let is_dir = all_dirs || i < comps.len() - 1;
                if seen.insert(child.clone()) {
                    sets.entry(parent.clone()).or_default().push(Entry {
                        path: child.clone(),
                        is_dir,
                    });
                }
                parent = child;
            }
        }

        for entries in sets.values_mut() {
            entries.sort_by(|a, b| {
                // Folders before files, then case-insensitive name.
                b.is_dir.cmp(&a.is_dir).then_with(|| {
                    let an = a.path.file_name().unwrap_or_default().to_ascii_lowercase();
                    let bn = b.path.file_name().unwrap_or_default().to_ascii_lowercase();
                    an.cmp(&bn)
                })
            });
        }

        Tree { children: sets }
    }

    /// Whether `path` is a FILE node of the tree (false for folders and unknown paths) —
    /// a lookup among its siblings, no filesystem access.
    pub fn is_file(&self, path: &Path) -> bool {
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        self.children
            .get(parent)
            .and_then(|es| es.iter().find(|e| e.path == path))
            .is_some_and(|e| !e.is_dir)
    }

    /// DFS from the root, descending only into expanded directories.
    pub fn visible(&self, expanded: &HashSet<PathBuf>) -> Vec<Row> {
        let mut out = Vec::new();
        self.walk(&PathBuf::new(), 0, expanded, &mut out);
        out
    }

    fn walk(&self, dir: &PathBuf, depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Row>) {
        let Some(entries) = self.children.get(dir) else {
            return;
        };
        for e in entries {
            out.push(Row {
                path: e.path.clone(),
                is_dir: e.is_dir,
                depth,
            });
            if e.is_dir && expanded.contains(&e.path) {
                self.walk(&e.path, depth + 1, expanded, out);
            }
        }
    }
}

/// An arrow key pressed over a tree (see [`nav`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavKey {
    /// Previous row.
    Up,
    /// Next row.
    Down,
    /// Collapse the folder, else move to the parent row.
    Left,
    /// Expand the folder, else move to its first child.
    Right,
}

/// What an arrow key does to a tree: move the cursor, or fold a folder in place.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Nav {
    /// Move the cursor (selection) to this row.
    Select(PathBuf),
    /// Expand this folder.
    Expand(PathBuf),
    /// Collapse this folder.
    Collapse(PathBuf),
    /// Nothing to do (an edge, or a file has no children).
    Stay,
}

/// Keyboard navigation over the visible `rows` (IDE tree semantics): ↑/↓ step through rows
/// (from no cursor, ↓ lands on the first row and ↑ on the last); → expands a collapsed
/// folder or steps into an expanded one; ← collapses an expanded folder or jumps to the
/// parent row. A cursor that's no longer visible restarts from the top.
///
/// ```
/// use kyde_tree::{nav, Nav, NavKey, Row};
/// use std::{collections::HashSet, path::{Path, PathBuf}};
/// let rows = vec![
///     Row { path: "src".into(), is_dir: true, depth: 0 },
///     Row { path: "src/a.rs".into(), is_dir: false, depth: 1 },
/// ];
/// let open = HashSet::from([PathBuf::from("src")]);
/// assert_eq!(nav(&rows, Some(Path::new("src/a.rs")), &open, NavKey::Left), Nav::Select("src".into()));
/// ```
pub fn nav<S: std::hash::BuildHasher>(
    rows: &[Row],
    cursor: Option<&Path>,
    expanded: &HashSet<PathBuf, S>,
    key: NavKey,
) -> Nav {
    let at = cursor.and_then(|c| rows.iter().position(|r| r.path == c));
    let select = |i: usize| {
        rows.get(i)
            .map_or(Nav::Stay, |r| Nav::Select(r.path.clone()))
    };
    let Some(i) = at else {
        return match key {
            NavKey::Up => rows.len().checked_sub(1).map_or(Nav::Stay, select),
            _ => select(0),
        };
    };
    let row = &rows[i];
    match key {
        NavKey::Up => i.checked_sub(1).map_or(Nav::Stay, select),
        NavKey::Down => select(i + 1),
        NavKey::Right if row.is_dir && !expanded.contains(&row.path) => {
            Nav::Expand(row.path.clone())
        }
        NavKey::Right => match rows.get(i + 1) {
            Some(next) if row.is_dir && next.depth > row.depth => Nav::Select(next.path.clone()),
            _ => Nav::Stay,
        },
        NavKey::Left if row.is_dir && expanded.contains(&row.path) => {
            Nav::Collapse(row.path.clone())
        }
        NavKey::Left => rows[..i]
            .iter()
            .rposition(|r| r.depth < row.depth)
            .map_or(Nav::Stay, select),
    }
}

impl Tree {
    /// [`Tree::visible`] under a project-root row (path `""`, depth 0): everything nests one
    /// level under it, and collapsing the root (removing `""` from `expanded`) hides it all.
    /// The shape every app tree shows (Browse, Commit, History, Rollback).
    pub fn visible_with_root(&self, expanded: &HashSet<PathBuf>) -> Vec<Row> {
        let mut rows = vec![Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if expanded.contains(Path::new("")) {
            rows.extend(self.visible(expanded).into_iter().map(|mut r| {
                r.depth += 1;
                r
            }));
        }
        rows
    }
}

/// Narrow `rows` to a search: keep the root row (`""`), files among `files` whose path
/// contains `query` (case-insensitive; blank = keep everything), and folders holding such
/// a file. Linear in rows + matches — each file is lowercased once, and folders are kept
/// by collecting the matches' ancestors rather than rescanning every file per folder.
///
/// ```
/// use kyde_tree::{retain_matching, Row};
/// use std::path::{Path, PathBuf};
/// let row = |p: &str, is_dir| Row { path: PathBuf::from(p), is_dir, depth: 0 };
/// let mut rows = vec![row("", true), row("src", true), row("src/a.rs", false), row("b.md", false)];
/// retain_matching(&mut rows, [Path::new("src/a.rs"), Path::new("b.md")], "A.RS");
/// let kept: Vec<_> = rows.iter().map(|r| r.path.clone()).collect();
/// assert_eq!(kept, ["", "src", "src/a.rs"].map(PathBuf::from));
/// ```
pub fn retain_matching<'a>(
    rows: &mut Vec<Row>,
    files: impl IntoIterator<Item = &'a Path>,
    query: &str,
) {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return;
    }
    let hits: HashSet<&Path> = files
        .into_iter()
        .filter(|f| f.to_string_lossy().to_lowercase().contains(&q))
        .collect();
    let dirs: HashSet<&Path> = hits.iter().flat_map(|f| f.ancestors().skip(1)).collect();
    rows.retain(|r| {
        r.path.as_os_str().is_empty()
            || if r.is_dir {
                dirs.contains(r.path.as_path())
            } else {
                hits.contains(r.path.as_path())
            }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn explicit_dirs_show_even_when_empty() {
        let files = vec![p("src/main.rs")];
        let dirs = vec![p("src/new"), p("empty/nested")];
        let t = Tree::build_with_dirs(&files, &dirs);
        let mut exp = HashSet::new();
        exp.insert(p("src"));
        exp.insert(p("empty"));
        let rows = t.visible(&exp);
        let entries: Vec<_> = rows.iter().map(|r| (r.path.clone(), r.is_dir)).collect();
        assert_eq!(
            entries,
            vec![
                (p("empty"), true),
                (p("empty/nested"), true),
                (p("src"), true),
                (p("src/new"), true),
                (p("src/main.rs"), false),
            ]
        );
    }

    #[test]
    fn folders_before_files_and_nesting() {
        let files = vec![
            p("src/main.rs"),
            p("src/git.rs"),
            p("README.md"),
            p("a/b/c.rs"),
        ];
        let t = Tree::build(&files);
        let mut exp = HashSet::new();
        // Collapsed: only top-level entries, folders first.
        let rows = t.visible(&exp);
        let paths: Vec<_> = rows.iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, vec![p("a"), p("src"), p("README.md")]);
        assert!(rows[0].is_dir && rows[1].is_dir && !rows[2].is_dir);

        // Expand src → its files appear under it at depth 1.
        exp.insert(p("src"));
        let rows = t.visible(&exp);
        let paths: Vec<_> = rows.iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                p("a"),
                p("src"),
                p("src").join("git.rs"),
                p("src").join("main.rs"),
                p("README.md")
            ]
        );
        assert_eq!(rows[2].depth, 1);
    }

    /// ↑↓ walk the rows, ← / → fold or hop between a folder and its children, and both
    /// stop at the edges.
    #[test]
    fn nav_walks_folds_and_hops() {
        let rows = vec![
            Row {
                path: p("src"),
                is_dir: true,
                depth: 0,
            },
            Row {
                path: p("src/a.rs"),
                is_dir: false,
                depth: 1,
            },
            Row {
                path: p("lib"),
                is_dir: true,
                depth: 0,
            },
        ];
        let exp = HashSet::from([p("src")]);
        let at = |c: &str, k| nav(&rows, Some(Path::new(c)), &exp, k);
        assert_eq!(nav(&rows, None, &exp, NavKey::Down), Nav::Select(p("src")));
        assert_eq!(nav(&rows, None, &exp, NavKey::Up), Nav::Select(p("lib")));
        assert_eq!(at("src", NavKey::Up), Nav::Stay);
        assert_eq!(at("lib", NavKey::Down), Nav::Stay);
        assert_eq!(at("src", NavKey::Down), Nav::Select(p("src/a.rs")));
        assert_eq!(at("src", NavKey::Right), Nav::Select(p("src/a.rs")));
        assert_eq!(at("src", NavKey::Left), Nav::Collapse(p("src")));
        assert_eq!(at("lib", NavKey::Right), Nav::Expand(p("lib")));
        assert_eq!(at("src/a.rs", NavKey::Left), Nav::Select(p("src")));
        assert_eq!(at("src/a.rs", NavKey::Right), Nav::Stay);
    }
}
