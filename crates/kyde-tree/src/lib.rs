//! A lazy file-tree model built from the flat, sorted list of repo files
//! (`Repo::list_files`, gitignored paths already excluded). Pure Rust, no gpui — the
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
    pub path: PathBuf,
    pub is_dir: bool,
    /// Nesting depth (0 = top level) → indentation.
    pub depth: usize,
}

impl Tree {
    /// Build from the flat file list. Every ancestor directory of every file becomes a node.
    pub fn build(files: &[PathBuf]) -> Self {
        // Per-parent dedup set while building, then sorted into `children`.
        let mut sets: BTreeMap<PathBuf, Vec<Entry>> = BTreeMap::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        for file in files {
            let mut parent = PathBuf::new();
            let comps: Vec<_> = file.components().collect();
            for (i, comp) in comps.iter().enumerate() {
                let mut child = parent.clone();
                child.push(comp);
                let is_dir = i < comps.len() - 1;
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
        let names: Vec<_> = rows
            .iter()
            .map(|r| r.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a", "src", "README.md"]);
        assert!(rows[0].is_dir && rows[1].is_dir && !rows[2].is_dir);

        // Expand src → its files appear under it at depth 1.
        exp.insert(p("src"));
        let rows = t.visible(&exp);
        let names: Vec<_> = rows
            .iter()
            .map(|r| r.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["a", "src", "src/git.rs", "src/main.rs", "README.md"]
        );
        assert_eq!(rows[2].depth, 1);
    }
}
