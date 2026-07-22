#![deny(missing_docs)]
//! A lazy file-tree model built from the flat, sorted list of repo files
//! (`Repo::list_files`, gitignored paths already excluded). Pure Rust, no UI framework — the
//! Browse pane turns `visible()` into IntelliJ-style indented rows.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

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
        let names: Vec<_> = rows
            .iter()
            .map(|r| (r.path.to_string_lossy().into_owned(), r.is_dir))
            .collect();
        assert_eq!(
            names,
            vec![
                ("empty".to_string(), true),
                ("empty/nested".to_string(), true),
                ("src".to_string(), true),
                ("src/new".to_string(), true),
                ("src/main.rs".to_string(), false),
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
}
