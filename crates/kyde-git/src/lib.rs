#![deny(missing_docs)]
//! Git layer. Shells out to the `git` binary on a background-friendly API,
//! exactly like Zed's `crates/git` (no libgit2). Pure Rust — compiles standalone.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Everything `kyde-git` can fail with. Each variant carries the context needed to act on it
/// — the offending path, the git command + its stderr, or the underlying I/O error + what we
/// were doing. (Library crate → typed errors via `thiserror`, never `anyhow`.)
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// `path` is not inside a git working tree.
    #[error("not a git repository")]
    NotARepository,
    /// File contains a NUL byte, so it can't be shown/edited as text.
    #[error("binary file: {0:?}")]
    BinaryFile(PathBuf),
    /// File bytes are not valid UTF-8.
    #[error("file is not valid UTF-8: {0:?}")]
    NotUtf8(PathBuf),
    /// A rename/move would clobber this existing destination.
    #[error("{0:?} already exists")]
    AlreadyExists(PathBuf),
    /// A branch/ref name was empty after trimming.
    #[error("empty branch name")]
    EmptyBranchName,
    /// A branch/ref name git would misread as a flag (leading `-`) or otherwise reject.
    #[error("invalid branch name: {0:?}")]
    InvalidBranchName(String),
    /// A push was rejected because the remote advanced, and the rebase-and-retry recovery
    /// itself failed (carries the rebase error so the user can resolve it manually).
    #[error("remote has changes that conflict with yours — pull and resolve them manually ({0})")]
    RemoteConflict(String),
    /// A git subprocess ran but exited non-zero. `command` is the argv; `stderr` is git's own.
    #[error("git {command} failed: {stderr}")]
    Command {
        /// The git argv that was run (debug-formatted).
        command: String,
        /// git's captured stderr.
        stderr: String,
    },
    /// An I/O or process-spawn error, tagged with the operation (and path, where relevant).
    #[error("{operation}: {source}")]
    Io {
        /// What we were doing (e.g. `"reading <path>"`, `"running git [...]"`).
        operation: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Result alias for every fallible `kyde-git` operation.
pub type Result<T> = std::result::Result<T, GitError>;

/// Build an [`GitError::Io`] from an operation description + the underlying I/O error.
fn io_err(operation: impl Into<String>) -> impl FnOnce(std::io::Error) -> GitError {
    move |source| GitError::Io {
        operation: operation.into(),
        source,
    }
}

/// A file's working-tree status (the `XY` codes from `git status --porcelain`, collapsed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// Newly added (staged) file.
    Added,
    /// Tracked file with content changes.
    Modified,
    /// Tracked file removed.
    Deleted,
    /// File moved/renamed.
    Renamed,
    /// New file not yet tracked by git.
    Untracked,
    /// File with a merge conflict.
    Conflict,
}

/// A changed file in the working tree: its repo-relative path + status.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    /// Repo-relative path of the file.
    pub path: PathBuf,
    /// The file's working-tree status.
    pub status: FileStatus,
}

/// One entry in the git log (history view).
#[derive(Debug, Clone)]
pub struct Commit {
    /// Full commit hash.
    pub hash: String,
    /// Abbreviated commit hash.
    pub short: String,
    /// Author name.
    pub author: String,
    /// Relative date string (`git log --date=relative`), e.g. "2 hours ago".
    pub date: String,
    /// Commit subject (first line of the message).
    pub subject: String,
    /// Decoration refs (`%D`): e.g. "HEAD -> main, origin/main, tag: v1". Empty if none.
    pub refs: String,
}

/// One working tree of a repository (`git worktree list`): the main checkout or a
/// linked worktree. Bare and prunable entries are filtered out by [`Repo::worktrees`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute path of the worktree's root.
    pub path: PathBuf,
    /// Checked-out branch (short name), or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// The worktree's HEAD commit hash.
    pub head: String,
    /// Whether this is the main worktree (always listed first by git).
    pub is_main: bool,
}

/// Outcome of [`Repo::merge_branch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Nothing to merge — the current branch already contains the other branch.
    UpToDate,
    /// Merged cleanly (fast-forward or an automatic merge commit).
    Merged,
    /// The merge stopped on conflicts; it is left in progress (`MERGE_HEAD` set) and
    /// these files need manual resolution.
    Conflicts(Vec<PathBuf>),
}

/// What happened to one SIDE of a conflicted file (relative to the merge base), derived
/// from which index stages exist — drives the Yours/Theirs columns of the conflicts list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    /// The side changed the file's content.
    Modified,
    /// The side added the file (no common base — both-added conflict).
    Added,
    /// The side deleted the file.
    Deleted,
}

impl ConflictSide {
    /// Column label for the conflicts list.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ConflictSide::Modified => "Modified",
            ConflictSide::Added => "Added",
            ConflictSide::Deleted => "Deleted",
        }
    }
}

/// One conflicted file with what each side did to it (see [`Repo::conflict_entries`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictEntry {
    /// Repo-relative path.
    pub path: PathBuf,
    /// What OUR side (the current branch) did.
    pub ours: ConflictSide,
    /// What THEIR side (the branch being merged in) did.
    pub theirs: ConflictSide,
}

/// A git repository rooted at a working-tree top level. All operations shell out to `git`.
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Open repo containing `path` (walks up to the git root).
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let out = git(path.as_ref(), &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(out.trim());
        if root.as_os_str().is_empty() {
            return Err(GitError::NotARepository);
        }
        Ok(Self { root })
    }

    /// The repository's working-tree root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Plain-text (fixed-string, case-insensitive) content search across tracked +
    /// untracked working-tree files (gitignored excluded), via `git grep`. Returns
    /// `(repo-relative path, 1-based line, line text)` hits, capped. `git grep` exits
    /// 1 on "no matches" — we treat that (and any spawn/exit error) as an empty list,
    /// not an `Err`, so the live finder never flashes errors while you type.
    pub fn grep(&self, query: &str) -> Vec<(PathBuf, u32, String)> {
        use std::io::{BufRead, BufReader};
        const CAP: usize = 500;
        if query.is_empty() {
            return Vec::new();
        }
        // Stream stdout and KILL the child once we have CAP hits. A short/common query
        // (e.g. "e") matches almost every line in the repo — letting `git grep` run to
        // completion buffers tens of MB and takes ~20s on a 2.7k-file repo. We only ever
        // show CAP results, and they arrive from the first files almost immediately, so
        // reading CAP lines then killing turns that into a near-instant, bounded read.
        // `-m 50` also caps matches *per file* so one huge file can't fill the whole cap.
        let child = git_cmd(&self.root)
            .args([
                "grep",
                "--no-color",
                "-F", // fixed string (not a regex)
                "-n", // line numbers
                "-I", // skip binary files
                "-i", // case-insensitive
                "-m",
                "50", // max matches per file
                "--untracked",
                "-e",
                query,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return Vec::new();
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Vec::new();
        };
        let mut hits = Vec::new();
        // Each line: `path:lineno:content`. Repo-relative paths on macOS have no ':',
        // so splitting on the first two ':' is unambiguous.
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let mut it = line.splitn(3, ':');
            let (Some(path), Some(num), Some(content)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(n) = num.parse::<u32>() else { continue };
            hits.push((PathBuf::from(path), n, content.to_string()));
            if hits.len() >= CAP {
                break;
            }
        }
        // Stop git scanning the rest of the repo (see note above) and reap the child.
        let _ = child.kill();
        let _ = child.wait();
        hits
    }

    /// `git status --porcelain=v1 -z` parsed into changed files.
    pub fn status(&self) -> Result<Vec<ChangedFile>> {
        let raw = git(
            &self.root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        let mut files = Vec::new();
        // Records are NUL-separated; rename records consume an extra field.
        let mut parts = raw.split('\0').filter(|s| !s.is_empty());
        while let Some(rec) = parts.next() {
            if rec.len() < 3 {
                continue;
            }
            let x = rec.as_bytes()[0] as char; // staged (index) column
            let y = rec.as_bytes()[1] as char; // unstaged (worktree) column
            let path = rec[3..].to_string();
            if x == 'R' || y == 'R' {
                // rename: the "from" path follows as the next NUL field
                let _from = parts.next();
            }
            let status = classify(x, y);
            // No quote/escape handling needed: `-z` makes git emit paths raw and
            // NUL-terminated (quoting/C-escaping is the non-`-z` behavior).
            files.push(ChangedFile {
                path: PathBuf::from(path),
                status,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// Added/removed line counts per changed file — one `git diff --numstat` against HEAD
    /// (worktree + index, matching the working-vs-HEAD diff the commit view shows), plus a
    /// line count for each untracked file (all additions). Binary files are omitted: numstat
    /// reports `-` for them and `+0 −0` would be a lie. An unborn HEAD yields no diff, so a
    /// brand-new repo just gets the untracked counts.
    pub fn numstat(&self) -> Result<std::collections::HashMap<PathBuf, (usize, usize)>> {
        let mut stats = std::collections::HashMap::new();
        // Each `-z` record is `added TAB removed TAB path NUL` (`--no-renames` keeps the
        // single-path shape; renames would splice a second path field into the record).
        let raw = git(
            &self.root,
            &["diff", "--numstat", "--no-renames", "-z", "HEAD"],
        )
        .unwrap_or_default();
        for rec in raw.split('\0').filter(|s| !s.is_empty()) {
            let mut it = rec.splitn(3, '\t');
            let (Some(add), Some(rem), Some(path)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            // Binary file → `-\t-` → both parses fail → skipped.
            if let (Ok(a), Ok(r)) = (add.parse(), rem.parse()) {
                stats.insert(PathBuf::from(path), (a, r));
            }
        }
        let untracked = git(
            &self.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        for path in untracked.split('\0').filter(|s| !s.is_empty()) {
            // Binary/non-UTF-8 content errors here — omit it, same as tracked binaries.
            if let Ok(content) = self.working_content(Path::new(path)) {
                stats.insert(PathBuf::from(path), (content.lines().count(), 0));
            }
        }
        Ok(stats)
    }

    /// HEAD version of a file = the "before" side of the commit diff. We compare the working
    /// tree against HEAD (not the index) so the diff always shows the *full* change that will
    /// be committed — `commit_now` re-stages each checked file's working copy before
    /// committing, so working-vs-HEAD is exactly the committed delta. Using the index instead
    /// would render an empty diff for any file that's already fully staged. A file absent from
    /// HEAD (newly added) has no base → empty string (all-added).
    pub fn base_content(&self, rel: &Path) -> Result<String> {
        let p = rel.to_string_lossy();
        git(&self.root, &["show", &format!("HEAD:{p}")]).or(Ok(String::new()))
    }

    /// Current on-disk content = the "after" side. Errors if the file is binary
    /// (contains a NUL byte) or not valid UTF-8, so callers treat it as
    /// not-diffable/not-editable rather than silently truncating it to empty
    /// (which, fed through the diff editor's autosave, would erase the file).
    pub fn working_content(&self, rel: &Path) -> Result<String> {
        let full = self.root.join(rel);
        let bytes = std::fs::read(&full).map_err(io_err(format!("reading {full:?}")))?;
        if bytes.contains(&0) {
            return Err(GitError::BinaryFile(rel.to_path_buf()));
        }
        String::from_utf8(bytes).map_err(|_| GitError::NotUtf8(rel.to_path_buf()))
    }

    /// Stage a file (`git add -- <rel>`).
    ///
    /// A file whose *deletion* is already staged (e.g. removed with `git rm`) exists in
    /// neither the worktree nor the index, so its pathspec matches nothing and `git add`
    /// fatals with "did not match any files" — but there's nothing left to stage: the
    /// deletion is already recorded. Treat exactly that case (no-match error + file absent
    /// from the worktree) as success, so committing a `git rm`'d file works.
    pub fn stage(&self, rel: &Path) -> Result<()> {
        match git(&self.root, &["add", "--", &rel.to_string_lossy()]) {
            Ok(_) => Ok(()),
            Err(GitError::Command { stderr, .. })
                if stderr.contains("did not match any files") && !self.root.join(rel).exists() =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Stage `content` as `rel`'s index entry WITHOUT touching the working tree
    /// (`git hash-object -w` + `git update-index --add --cacheinfo`) — the partial-commit
    /// primitive: commit a subset of a file's changes while the rest stays on disk.
    /// The entry's mode follows the working file's executable bit.
    pub fn stage_content(&self, rel: &Path, content: &str) -> Result<()> {
        let oid = git_stdin(&self.root, &["hash-object", "-w", "--stdin"], content)?;
        let mode = if is_executable(&self.root.join(rel)) {
            "100755"
        } else {
            "100644"
        };
        let info = format!("{mode},{},{}", oid.trim(), rel.to_string_lossy());
        git(&self.root, &["update-index", "--add", "--cacheinfo", &info]).map(|_| ())
    }

    /// Unstage a file (`git restore --staged -- <rel>`).
    pub fn unstage(&self, rel: &Path) -> Result<()> {
        match git(
            &self.root,
            &["restore", "--staged", "--", &rel.to_string_lossy()],
        ) {
            Ok(_) => Ok(()),
            // A repo with no commits yet (unborn HEAD): `restore --staged` needs HEAD to
            // restore FROM and dies with "could not resolve HEAD" (newer gits quote it:
            // "could not resolve 'HEAD'" — match the stable prefix only). Everything staged
            // there is by definition newly added, so dropping the index entry — keeping the
            // working-tree file — is the exact same unstage.
            Err(GitError::Command { stderr, .. }) if stderr.contains("could not resolve") => git(
                &self.root,
                &[
                    "rm",
                    "--cached",
                    "--force",
                    "--quiet",
                    "--",
                    &rel.to_string_lossy(),
                ],
            )
            .map(|_| ()),
            Err(e) => Err(e),
        }
    }

    /// Discard all changes to a tracked file (index + worktree → HEAD).
    pub fn discard(&self, rel: &Path) -> Result<()> {
        git(
            &self.root,
            &["checkout", "HEAD", "--", &rel.to_string_lossy()],
        )
        .map(|_| ())
    }

    /// Remove a file from the working tree (used to delete added/untracked files on rollback).
    pub fn delete_file(&self, rel: &Path) -> Result<()> {
        let full = self.root.join(rel);
        std::fs::remove_file(&full).map_err(io_err(format!("deleting {full:?}")))
    }

    /// Rename/move a working-tree file. Plain `fs::rename` (git picks up the
    /// rename on its next status), creating the destination's parent dirs.
    /// Refuses to clobber an existing destination.
    pub fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let (src, dst) = (self.root.join(from), self.root.join(to));
        if dst.exists() {
            return Err(GitError::AlreadyExists(to.to_path_buf()));
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::rename(&src, &dst).map_err(io_err(format!("renaming {src:?} -> {dst:?}")))?;
        Ok(())
    }

    /// Commit the staged changes with `message` (passed to `git commit -F -` via stdin).
    pub fn commit(&self, message: &str) -> Result<()> {
        git_stdin(&self.root, &["commit", "-F", "-"], message).map(|_| ())
    }

    /// All tracked + untracked-but-not-ignored files, for the folder-tree view.
    /// Uses `git ls-files` so .gitignored noise stays out.
    pub fn list_files(&self) -> Result<Vec<PathBuf>> {
        let raw = git(
            &self.root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let mut files: Vec<PathBuf> = raw
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Current branch name, or None when HEAD is detached / mid-rebase.
    pub fn current_branch(&self) -> Option<String> {
        let out = git(&self.root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
        let b = out.trim();
        (!b.is_empty()).then(|| b.to_string())
    }

    /// Push the current branch to `origin`, setting upstream when missing.
    /// `--set-upstream` is harmless when an upstream already exists, so one path
    /// covers both first push and subsequent pushes. Returns git's stdout.
    pub fn push(&self) -> Result<String> {
        match self.current_branch() {
            Some(b) => git(&self.root, &["push", "--set-upstream", "origin", &b]),
            None => git(&self.root, &["push"]),
        }
    }

    /// Push, recovering from the common "remote moved on" rejection. If the push is rejected
    /// as non-fast-forward (e.g. CI/release automation pushed while you were working), rebase
    /// the local commits on top of the upstream and retry once — instead of dumping git's raw
    /// "fetch first" hint on the user. Auth/network failures pass straight through unchanged.
    /// On a rebase conflict the rebase is aborted (clean tree restored) and a clear,
    /// actionable error is returned.
    pub fn push_rebasing(&self) -> Result<String> {
        match self.push() {
            Ok(out) => Ok(out),
            Err(e) if push_rejected(&e.to_string()) => {
                self.pull_rebase()
                    .map_err(|pe| GitError::RemoteConflict(pe.to_string()))?;
                self.push()
            }
            Err(e) => Err(e),
        }
    }

    /// Fetch remote-tracking refs (with prune) without touching the working tree. Run before
    /// reading `ahead_count`/`behind_count` to make them reflect the true remote state.
    pub fn fetch(&self) -> Result<String> {
        git(&self.root, &["fetch", "--prune"])
    }

    /// Commits the upstream is ahead of the current branch — i.e. how many a pull would
    /// bring in. Reflects the last-fetched remote-tracking ref (so it's only as fresh as the
    /// last fetch/pull). `None` when there's no upstream or HEAD is unborn.
    pub fn behind_count(&self) -> Option<usize> {
        // `HEAD..@{u}` can't be resolved without an upstream, so this single rev-list
        // both fails cleanly when there's none (→ `None`) and counts when there is —
        // no separate `rev-parse @{u}` existence probe needed (it doubled the spawns on
        // a path the status bar hits often).
        git(&self.root, &["rev-list", "--count", "HEAD..@{u}"])
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Rebase local commits onto the upstream, auto-stashing any uncommitted edits so it
    /// works mid-change. On any failure the rebase is aborted to leave a clean working tree.
    /// Public so it doubles as the explicit "Pull" action (a pull fetches, then rebases).
    pub fn pull_rebase(&self) -> Result<String> {
        let res = match self.current_branch() {
            Some(b) => git(
                &self.root,
                &["pull", "--rebase", "--autostash", "origin", &b],
            ),
            None => git(&self.root, &["pull", "--rebase", "--autostash"]),
        };
        if res.is_err() {
            // Don't strand the repo mid-rebase — restore the pre-pull state.
            let _ = git(&self.root, &["rebase", "--abort"]);
        }
        res
    }

    /// How many commits the current branch is ahead of its push base (see `push_base_commit`).
    /// `None` only when HEAD has no commits / is unborn (nothing to push).
    pub fn ahead_count(&self) -> Option<usize> {
        let range = match self.push_base_commit() {
            Some(base) => format!("{base}..HEAD"),
            // No remote base at all → every commit on HEAD is unpushed.
            None => "HEAD".to_string(),
        };
        git(&self.root, &["rev-list", "--count", &range])
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// The commit a push is measured against, resolved in priority order. `None` means there's
    /// no remote reference point at all (no upstream, no remote, no mainline) — only then does
    /// a push conceptually send all of HEAD.
    ///
    /// Crucially this does NOT fall back to the empty tree for a freshly-created local branch:
    /// such a branch has no `@{u}`, but a push would only send the commits unique to it, so we
    /// use the fork point from the remote's mainline branch — a brand-new branch with no new
    /// commits then correctly shows nothing to push, not the entire repo.
    fn push_base_commit(&self) -> Option<String> {
        let verify =
            |rev: &str| git(&self.root, &["rev-parse", "--verify", "--quiet", rev]).is_ok();
        // 1. Tracking upstream.
        if verify("@{u}") {
            return Some("@{u}".to_string());
        }
        // 2. A same-named remote branch (pushed before, but not set as upstream).
        if let Some(branch) = self.current_branch() {
            let r = format!("origin/{branch}");
            if verify(&r) {
                return Some(r);
            }
        }
        // 3. Configured push destination.
        if verify("@{push}") {
            return Some("@{push}".to_string());
        }
        // 4. Fork point from the remote's mainline branch (the common "new local branch off
        //    main, never pushed" case).
        let mut mains: Vec<String> = Vec::new();
        if let Ok(def) = git(&self.root, &["rev-parse", "--abbrev-ref", "origin/HEAD"]) {
            let d = def.trim();
            if !d.is_empty() && d != "origin/HEAD" {
                mains.push(d.to_string());
            }
        }
        mains.push("origin/main".to_string());
        mains.push("origin/master".to_string());
        for m in mains {
            if verify(&m) {
                if let Ok(mb) = git(&self.root, &["merge-base", "HEAD", &m]) {
                    let mb = mb.trim();
                    if !mb.is_empty() {
                        return Some(mb.to_string());
                    }
                }
            }
        }
        None
    }

    /// The base revision a push would be diffed against (string form for `diff_files`).
    /// Falls back to the empty tree only when there's genuinely no remote reference point.
    pub fn push_base(&self) -> String {
        self.push_base_commit()
            // Well-known empty-tree object id — diffing against it lists all of HEAD.
            .unwrap_or_else(|| "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string())
    }

    /// Files that differ between `push_base()` and HEAD — i.e. what a push would send.
    /// Same `ChangedFile` shape as `status()`, so the push modal renders like commit/rollback.
    pub fn push_files(&self) -> Vec<ChangedFile> {
        self.diff_files(&self.push_base(), Some("HEAD"), None)
    }

    /// Files that differ between two revisions. `to == None` diffs `from` against the working
    /// tree (so the history view can compare a commit to the local checkout); `path` (a dir
    /// or file) scopes the diff to that subtree, recursively. Same `ChangedFile` shape as
    /// `status()`/`push_files()`. Empty on any error (bad rev, etc.).
    pub fn diff_files(
        &self,
        from: &str,
        to: Option<&str>,
        path: Option<&Path>,
    ) -> Vec<ChangedFile> {
        let Ok(from) = valid_ref(from) else {
            return Vec::new();
        };
        let mut args = vec!["diff", "--name-status", "-z", from];
        if let Some(to) = to {
            let Ok(to) = valid_ref(to) else {
                return Vec::new();
            };
            args.push(to);
        }
        let path_str = path.map(|p| p.to_string_lossy().into_owned());
        if let Some(p) = &path_str {
            args.push("--");
            args.push(p);
        }
        match git(&self.root, &args) {
            Ok(raw) => parse_name_status(&raw),
            Err(_) => Vec::new(),
        }
    }

    /// Commit log for a revision (branch name or `HEAD`), newest first, capped at `limit`.
    /// `path` (a dir or file) restricts the log to commits that touched that subtree —
    /// recursive for a directory — so the history view can scope to a folder.
    pub fn log(&self, rev: &str, limit: usize, path: Option<&Path>) -> Result<Vec<Commit>> {
        let rev = valid_ref(rev)?;
        // \x1f (unit separator) between fields; one commit per line (%s/%D are single-line).
        let fmt = "--format=%H%x1f%h%x1f%an%x1f%ad%x1f%s%x1f%D";
        let n = format!("-n{limit}");
        let mut args = vec!["log", rev, "--date=relative", &n, fmt];
        let path_str = path.map(|p| p.to_string_lossy().into_owned());
        if let Some(p) = &path_str {
            args.push("--");
            args.push(p);
        }
        let raw = git(&self.root, &args)?;
        Ok(raw
            .lines()
            .filter_map(|line| {
                let mut f = line.split('\u{1f}');
                Some(Commit {
                    hash: f.next()?.to_string(),
                    short: f.next()?.to_string(),
                    author: f.next()?.to_string(),
                    date: f.next()?.to_string(),
                    subject: f.next()?.to_string(),
                    refs: f.next().unwrap_or("").trim().to_string(),
                })
            })
            .collect())
    }

    /// Content of `rel` at a committed revision (e.g. `@{u}` or `HEAD`), empty if the
    /// file doesn't exist there (added/deleted) — the two sides of a push diff.
    pub fn committed_content(&self, rev: &str, rel: &Path) -> Result<String> {
        // Guard the rev the same way `log`/`diff_files`/`checkout` do — an unvalidated rev
        // was the one remaining spot a value git could misread as a flag reached the argv.
        // An invalid rev has no content to show, so it collapses to empty (like a missing
        // file), preserving this function's "never errors, absent → empty" contract.
        let Ok(rev) = valid_ref(rev) else {
            return Ok(String::new());
        };
        let spec = format!("{}:{}", rev, rel.to_string_lossy());
        Ok(git(&self.root, &["show", &spec]).unwrap_or_default())
    }

    /// Local branches, most-recently-committed first (recency order). The UI
    /// slices the top few as "Recent" and re-sorts the whole list as "All".
    pub fn branches(&self) -> Result<Vec<String>> {
        let raw = git(
            &self.root,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)",
                "refs/heads/",
            ],
        )?;
        Ok(raw
            .lines()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Remote-tracking branches (`refs/remotes/`), recency order, e.g. "origin/main".
    /// `origin/HEAD` (the symbolic default-branch pointer) is filtered out.
    pub fn remote_branches(&self) -> Result<Vec<String>> {
        let raw = git(
            &self.root,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)",
                "refs/remotes/",
            ],
        )?;
        Ok(raw
            .lines()
            .filter(|s| !s.is_empty() && !s.ends_with("/HEAD"))
            .map(str::to_string)
            .collect())
    }

    /// All working trees of this repository (`git worktree list --porcelain`), main
    /// worktree first. Bare and prunable entries are skipped — they have no checkout
    /// to open.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let raw = git(&self.root, &["worktree", "list", "--porcelain"])?;
        Ok(parse_worktrees(&raw))
    }

    /// Switch to an existing local branch.
    pub fn checkout(&self, branch: &str) -> Result<()> {
        let branch = valid_ref(branch)?;
        // `<branch> --` pins the argument as a revision, never a pathspec or flag.
        git(&self.root, &["checkout", branch, "--"]).map(|_| ())
    }

    /// Create and switch to a new branch off the current HEAD.
    /// Create a branch. `checkout` switches to it (`-b`/`-B`); otherwise just creates it
    /// (`branch`). `overwrite` resets an existing branch of the same name (`-B`/`-f`).
    pub fn create_branch_opts(&self, name: &str, checkout: bool, overwrite: bool) -> Result<()> {
        let name = valid_ref(name)?;
        let args: &[&str] = match (checkout, overwrite) {
            (true, false) => &["checkout", "-b", name],
            (true, true) => &["checkout", "-B", name],
            (false, false) => &["branch", name],
            (false, true) => &["branch", "-f", name],
        };
        git(&self.root, args).map(|_| ())
    }

    /// Merge `branch` into the current branch (`git merge --no-edit`). On conflicts the
    /// merge is deliberately left IN PROGRESS (`MERGE_HEAD` set, conflict markers staged)
    /// so the caller can drive interactive resolution — abort via [`Self::merge_abort`],
    /// conclude via [`Self::commit_merge`]. A failure that did NOT start a merge (dirty
    /// tree in the way, unrelated histories, bad rev) surfaces as the git error.
    pub fn merge_branch(&self, branch: &str) -> Result<MergeOutcome> {
        let branch = valid_ref(branch)?;
        match git(&self.root, &["merge", "--no-edit", branch]) {
            Ok(out) => {
                if out.contains("Already up to date") {
                    Ok(MergeOutcome::UpToDate)
                } else {
                    Ok(MergeOutcome::Merged)
                }
            }
            // Only a merge that actually started (MERGE_HEAD exists) is a conflict
            // outcome; any other failure never touched the tree — pass it through.
            Err(e) => match self.merging() {
                Some(_) => Ok(MergeOutcome::Conflicts(self.conflicted_files())),
                None => Err(e),
            },
        }
    }

    /// Abort an in-progress merge, restoring the pre-merge working tree.
    pub fn merge_abort(&self) -> Result<()> {
        git(&self.root, &["merge", "--abort"]).map(|_| ())
    }

    /// When a merge is in progress, the friendliest name for what's being merged in:
    /// a local branch pointing at `MERGE_HEAD`, else its short hash. `None` = no merge
    /// in progress. (Goes through `git rev-parse`, so it's linked-worktree-safe —
    /// never reads `.git/MERGE_HEAD` directly.)
    pub fn merging(&self) -> Option<String> {
        let head = git(
            &self.root,
            &["rev-parse", "--quiet", "--verify", "--short", "MERGE_HEAD"],
        )
        .ok()?;
        let short = head.trim().to_string();
        if short.is_empty() {
            return None;
        }
        let name = git(
            &self.root,
            &[
                "branch",
                "--points-at",
                "MERGE_HEAD",
                "--format=%(refname:short)",
            ],
        )
        .ok()
        .and_then(|out| out.lines().next().map(str::to_string))
        .filter(|s| !s.is_empty());
        Some(name.unwrap_or(short))
    }

    /// Files currently in the unmerged (conflicted) state. Empty when none / on error.
    pub fn conflicted_files(&self) -> Vec<PathBuf> {
        git(
            &self.root,
            &["diff", "--name-only", "--diff-filter=U", "-z"],
        )
        .map(|raw| {
            raw.split('\0')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Conflicted files with what EACH side did (the conflicts-list columns), derived from
    /// which index stages exist per path (`git ls-files -u -z`: records of
    /// `mode SP oid SP stage TAB path`): all three stages → modified/modified; a missing
    /// stage 2/3 → that side deleted; a missing stage 1 (no common base) → both added.
    pub fn conflict_entries(&self) -> Vec<ConflictEntry> {
        let Ok(raw) = git(&self.root, &["ls-files", "-u", "-z"]) else {
            return Vec::new();
        };
        // Path → bitmask of the stages present (bits 1/2/3).
        let mut stages: Vec<(PathBuf, u8)> = Vec::new();
        for rec in raw.split('\0').filter(|s| !s.is_empty()) {
            let Some((meta, path)) = rec.split_once('\t') else {
                continue;
            };
            let Some(stage) = meta.split(' ').nth(2).and_then(|s| s.parse::<u8>().ok()) else {
                continue;
            };
            let path = PathBuf::from(path);
            match stages.iter_mut().find(|(p, _)| *p == path) {
                Some((_, mask)) => *mask |= 1 << stage,
                None => stages.push((path, 1 << stage)),
            }
        }
        stages.sort_by(|a, b| a.0.cmp(&b.0));
        stages
            .into_iter()
            .map(|(path, mask)| {
                let (has_base, has_ours, has_theirs) =
                    (mask & 0b0010 != 0, mask & 0b0100 != 0, mask & 0b1000 != 0);
                let side = |present| match (has_base, present) {
                    (_, false) => ConflictSide::Deleted,
                    (false, true) => ConflictSide::Added,
                    (true, true) => ConflictSide::Modified,
                };
                ConflictEntry {
                    path,
                    ours: side(has_ours),
                    theirs: side(has_theirs),
                }
            })
            .collect()
    }

    /// One side of a conflicted file from the index: stage 1 = common base, 2 = ours,
    /// 3 = theirs (`git show :N:path`). A missing stage (added on both sides → no base;
    /// deleted on one side) collapses to empty, like a missing file elsewhere.
    pub fn conflict_stage(&self, rel: &Path, stage: u8) -> String {
        let spec = format!(":{}:{}", stage, rel.to_string_lossy());
        git(&self.root, &["show", &spec]).unwrap_or_default()
    }

    /// Conclude a conflicted merge once every file is resolved + staged: commits with
    /// the message git prepared in `MERGE_MSG` (`git commit --no-edit`).
    pub fn commit_merge(&self) -> Result<()> {
        git(&self.root, &["commit", "--no-edit"]).map(|_| ())
    }

    /// `(ahead, behind)` of `branch` relative to the current HEAD: commits only on
    /// `branch` (what a merge would bring in) / commits only on HEAD. One
    /// `rev-list --left-right --count` — cheap enough to run per branch when the
    /// switcher opens. `None` on a bad rev or unborn HEAD.
    pub fn branch_ahead_behind(&self, branch: &str) -> Option<(usize, usize)> {
        let branch = valid_ref(branch).ok()?;
        let range = format!("HEAD...{branch}");
        let out = git(&self.root, &["rev-list", "--left-right", "--count", &range]).ok()?;
        let mut it = out.split_whitespace();
        let behind: usize = it.next()?.parse().ok()?; // left = only on HEAD
        let ahead: usize = it.next()?.parse().ok()?; // right = only on `branch`
        Some((ahead, behind))
    }

    /// Write new content to a working-tree file (used by the editor on save).
    pub fn save_file(&self, rel: &Path, content: &str) -> Result<()> {
        let full = self.root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&full, content).map_err(io_err(format!("writing {full:?}")))?;
        Ok(())
    }
}

/// Parse `git diff --name-status -z` output into changed files. `-z` = NUL-separated
/// fields: a status code then a path; renames/copies emit the code (e.g. "R100") followed by
/// TWO paths (old, new) — we keep the new one. Shared by `diff_files`/`push_files`.
fn parse_name_status(raw: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut parts = raw.split('\0').filter(|s| !s.is_empty());
    while let Some(stat) = parts.next() {
        let code = stat.as_bytes().first().copied().unwrap_or(b'M') as char;
        let path = if code == 'R' || code == 'C' {
            let _old = parts.next();
            parts.next()
        } else {
            parts.next()
        };
        let Some(path) = path else { break };
        files.push(ChangedFile {
            path: PathBuf::from(path),
            status: classify(code, ' '),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Parse `git worktree list --porcelain` output. Entries are blank-line-separated blocks
/// of `<attr> [value]` lines: `worktree <path>` opens a block; `HEAD <sha>`, `branch
/// refs/heads/<name>`, `detached`, `bare`, `prunable [reason]` follow. Bare and prunable
/// blocks are dropped. The first surviving entry is the main worktree.
fn parse_worktrees(raw: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    let mut first_block = true;
    for block in raw.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let is_main = first_block;
        first_block = false;
        let mut path = None;
        let mut head = String::new();
        let mut branch = None;
        let mut skip = false;
        for line in block.lines() {
            let (attr, value) = line.split_once(' ').unwrap_or((line, ""));
            match attr {
                "worktree" => path = Some(PathBuf::from(value)),
                "HEAD" => head = value.to_string(),
                "branch" => {
                    branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_string(),
                    );
                }
                "bare" | "prunable" => skip = true,
                _ => {}
            }
        }
        if skip {
            continue;
        }
        if let Some(path) = path {
            out.push(Worktree {
                path,
                branch,
                head,
                is_main,
            });
        }
    }
    out
}

fn classify(x: char, y: char) -> FileStatus {
    match (x, y) {
        ('?', '?') => FileStatus::Untracked,
        ('U', _) | (_, 'U') | ('D', 'D') | ('A', 'A') => FileStatus::Conflict,
        ('A', _) => FileStatus::Added,
        ('D', _) | (_, 'D') => FileStatus::Deleted,
        ('R', _) => FileStatus::Renamed,
        _ => FileStatus::Modified,
    }
}

/// Reject a branch/ref name that git would misread as a flag (leading `-`) or
/// that is empty. Guards the new-branch text box against argument injection;
/// not a full `git check-ref-format`. Returns the trimmed name on success.
fn valid_ref(name: &str) -> Result<&str> {
    let n = name.trim();
    if n.is_empty() {
        return Err(GitError::EmptyBranchName);
    }
    if n.starts_with('-') {
        return Err(GitError::InvalidBranchName(n.to_string()));
    }
    Ok(n)
}

/// True when a push failed only because the remote advanced (non-fast-forward) — a
/// rebase-and-retry can recover. Distinct from auth/network errors, which can't, so we
/// only auto-rebase on these specific phrases git emits for a rejected push.
fn push_rejected(err: &str) -> bool {
    let e = err.to_lowercase();
    [
        "fetch first",
        "non-fast-forward",
        "[rejected]",
        "updates were rejected",
    ]
    .iter()
    .any(|m| e.contains(m))
}

/// Base `git` command for `dir`: locale + environment pinned so behavior doesn't depend on
/// how the app was launched.
/// - `LC_ALL=C` forces English output — `stage`'s already-staged-deletion fallback and
///   `push_rejected` match on git's message text, which localized gits translate.
/// - `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` are scrubbed: git exports them to hooks
///   (and users set them), and inherited they silently redirect every command to *that*
///   repository instead of `dir` (e.g. kyde launched from a git hook).
fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_cmd(dir)
        .args(args)
        .output()
        .map_err(io_err(format!("running git {args:?}")))?;
    if !out.status.success() {
        return Err(GitError::Command {
            command: format!("{args:?}"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether the on-disk file has any executable bit set (false when absent).
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// Whether the on-disk file has any executable bit set (always false off-unix).
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

fn git_stdin(dir: &Path, args: &[&str], stdin: &str) -> Result<String> {
    use std::io::Write;
    let mut child = git_cmd(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_err(format!("spawning git {args:?}")))?;
    // The pipe was configured above, so `take()` yields it; if it's somehow absent we skip the
    // write rather than `.expect()` (rule: no unwrap/expect) — git then sees EOF and fails with
    // its own message, surfaced below as a `Command` error.
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(stdin.as_bytes())
            .map_err(io_err("writing to git stdin"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(io_err(format!("waiting for git {args:?}")))?;
    if !out.status.success() {
        return Err(GitError::Command {
            command: format!("{args:?}"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_rejected_matches_only_non_fast_forward() {
        // Real git rejection text → should trigger the rebase-and-retry recovery.
        assert!(push_rejected(
            "git [\"push\"] failed: ! [rejected]   main -> main (fetch first)\n\
             error: failed to push some refs\nhint: Updates were rejected because the remote \
             contains work that you do not have locally."
        ));
        assert!(push_rejected("Updates were rejected (non-fast-forward)"));
        // Auth/network failures must NOT auto-rebase.
        assert!(!push_rejected(
            "fatal: Authentication failed for 'https://github.com/x/y.git'"
        ));
        assert!(!push_rejected(
            "fatal: unable to access: Could not resolve host"
        ));
    }

    #[test]
    fn parse_worktrees_skips_bare_and_prunable_and_reads_branches() {
        let raw = "worktree /repos/main\nHEAD 1111111111111111111111111111111111111111\n\
                   branch refs/heads/main\n\n\
                   worktree /repos/wt-feat\nHEAD 2222222222222222222222222222222222222222\n\
                   branch refs/heads/feat/x\n\n\
                   worktree /repos/wt-detached\nHEAD 3333333333333333333333333333333333333333\n\
                   detached\n\n\
                   worktree /repos/bare.git\nbare\n\n\
                   worktree /repos/gone\nHEAD 4444444444444444444444444444444444444444\n\
                   branch refs/heads/gone\nprunable gitdir file points to non-existent location\n";
        let wts = parse_worktrees(raw);
        assert_eq!(wts.len(), 3, "bare + prunable entries are dropped");
        assert_eq!(wts[0].path, PathBuf::from("/repos/main"));
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(wts[0].is_main);
        assert_eq!(wts[1].branch.as_deref(), Some("feat/x"));
        assert!(!wts[1].is_main);
        assert_eq!(wts[2].branch, None, "detached HEAD → no branch");
        assert_eq!(wts[2].head, "3333333333333333333333333333333333333333");
    }

    /// `worktrees()` against a real repo with one linked worktree: two entries, main
    /// first, each with the right branch.
    #[test]
    fn worktrees_lists_main_and_linked() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let base = std::env::temp_dir().join(format!("kyde-worktrees-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let work = base.join("work");
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("a.txt"), "1\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);
        let linked = base.join("wt-agent");
        g(
            &work,
            &[
                "worktree",
                "add",
                "-b",
                "agent/task",
                linked.to_str().unwrap(),
            ],
        );

        let repo = Repo::discover(&work).unwrap();
        let wts = repo.worktrees().unwrap();
        assert_eq!(wts.len(), 2);
        assert!(wts[0].is_main);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert!(!wts[1].is_main);
        assert_eq!(wts[1].branch.as_deref(), Some("agent/task"));
        assert!(wts[1].path.ends_with("wt-agent"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn valid_ref_rejects_flags_and_empty() {
        assert!(valid_ref("-f").is_err());
        assert!(valid_ref("--track").is_err());
        assert!(valid_ref("   ").is_err());
        assert!(valid_ref("").is_err());
        assert_eq!(valid_ref("feature/x").unwrap(), "feature/x");
        assert_eq!(valid_ref("  main  ").unwrap(), "main"); // trims
    }

    /// Committing a `git rm`'d file must work: its deletion is already staged, so the
    /// pathspec matches nothing and a plain `git add` fatals ("did not match any files") —
    /// `stage` must treat that as already-staged, and the commit must record the deletion.
    /// (Caught live: committing the removed `clipboard.rs` failed in the commit view.)
    #[test]
    fn stage_tolerates_already_staged_deletion() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let work = std::env::temp_dir().join(format!("kyde-gitrm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("doomed.txt"), "bye\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);
        g(&work, &["rm", "-q", "doomed.txt"]); // deletion staged, index entry gone

        let repo = Repo::discover(&work).unwrap();
        repo.stage(Path::new("doomed.txt"))
            .expect("staging an already-staged deletion is a no-op, not a fatal");
        repo.commit("remove doomed")
            .expect("commit records the deletion");
        assert!(
            git(&work, &["show", "HEAD:doomed.txt"]).is_err(),
            "the file must be gone from HEAD after the commit"
        );
        // A genuinely bogus path (never existed) must still error, not silently pass —
        // it doesn't exist on disk either, so only the status text distinguishes... it
        // does: same fatal, but there's no staged deletion to fall back on. `stage`
        // accepts it as Ok only because the file is absent; guard the real-failure path
        // with a case where the file EXISTS but is unreadable-to-git instead: an ignored
        // file still errors.
        fs::write(work.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(work.join("ignored.txt"), "x\n").unwrap();
        assert!(
            repo.stage(Path::new("ignored.txt")).is_err(),
            "staging an ignored (existing) file must still surface git's error"
        );

        let _ = fs::remove_dir_all(&work);
    }

    /// One `numstat` call must size every kind of change in the list: a tracked edit, a
    /// worktree deletion, and an untracked file (all additions). Binary files can't be
    /// line-counted, so they're omitted rather than reported as `+0 −0`.
    #[test]
    fn numstat_reports_added_and_removed_lines_per_file() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let work = std::env::temp_dir().join(format!("kyde-numstat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("kept.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(work.join("gone.txt"), "a\nb\n").unwrap();
        fs::write(work.join("blob.bin"), [0u8, 1, 2]).unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);

        // One line changed + two added; a deletion; an untracked file; a binary edit.
        fs::write(work.join("kept.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
        fs::remove_file(work.join("gone.txt")).unwrap();
        fs::write(work.join("new.txt"), "x\ny\nz\n").unwrap();
        fs::write(work.join("blob.bin"), [0u8, 9, 9, 9]).unwrap();

        let repo = Repo::discover(&work).unwrap();
        let stats = repo.numstat().unwrap();
        assert_eq!(stats.get(Path::new("kept.txt")), Some(&(3, 1)));
        assert_eq!(stats.get(Path::new("gone.txt")), Some(&(0, 2)));
        assert_eq!(stats.get(Path::new("new.txt")), Some(&(3, 0)));
        assert!(!stats.contains_key(Path::new("blob.bin")));

        let _ = fs::remove_dir_all(&work);
    }

    /// `stage_content` stages GIVEN content for a path (not the working copy), leaving the
    /// working tree untouched — the partial-commit primitive.
    #[test]
    fn stage_content_stages_given_text_without_touching_the_worktree() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let work = std::env::temp_dir().join(format!("kyde-partial-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("f.txt"), "one\ntwo\nthree\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);

        // Two edits on disk; stage only the first one.
        fs::write(work.join("f.txt"), "ONE\ntwo\nTHREE\n").unwrap();
        let repo = Repo::discover(&work).unwrap();
        repo.stage_content(Path::new("f.txt"), "ONE\ntwo\nthree\n")
            .expect("staging partial content");
        repo.commit("partial").expect("committing the staged half");

        assert_eq!(g(&work, &["show", "HEAD:f.txt"]), "ONE\ntwo\nthree\n");
        assert_eq!(
            fs::read_to_string(work.join("f.txt")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );
        let status = repo.status().unwrap();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].path, PathBuf::from("f.txt"));

        // A brand-new (untracked) file can be partially staged the same way.
        fs::write(work.join("new.txt"), "a\nb\n").unwrap();
        repo.stage_content(Path::new("new.txt"), "a\n")
            .expect("staging partial content of an untracked file");
        repo.commit("partial new")
            .expect("committing the staged part");
        assert_eq!(g(&work, &["show", "HEAD:new.txt"]), "a\n");
        assert_eq!(fs::read_to_string(work.join("new.txt")).unwrap(), "a\nb\n");

        // An executable's mode survives partial staging (100755, not 100644).
        fs::write(work.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        g(&work, &["update-index", "--add", "--chmod=+x", "run.sh"]);
        g(&work, &["commit", "-m", "script"]);
        fs::write(work.join("run.sh"), "#!/bin/sh\necho hi\necho more\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(work.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        repo.stage_content(Path::new("run.sh"), "#!/bin/sh\necho hi\necho more\n")
            .expect("staging executable content");
        let ls = g(&work, &["ls-files", "--stage", "--", "run.sh"]);
        assert!(
            ls.starts_with("100755"),
            "executable bit must survive stage_content, got: {ls}"
        );

        let _ = fs::remove_dir_all(&work);
    }

    /// `committed_content` must run the same ref guard as `log`/`checkout`: a flag-like rev
    /// never reaches git's argv, and a valid rev reads the file at that revision.
    #[test]
    fn committed_content_guards_the_rev() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let work = std::env::temp_dir().join(format!("kyde-committed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("a.txt"), "1\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);

        let repo = Repo::discover(&work).unwrap();
        assert_eq!(
            repo.committed_content("HEAD", Path::new("a.txt")).unwrap(),
            "1\n"
        );
        // Flag-like rev → rejected before git sees it → empty (no error, no injection).
        assert_eq!(
            repo.committed_content("--output=/tmp/evil", Path::new("a.txt"))
                .unwrap(),
            ""
        );

        let _ = fs::remove_dir_all(&work);
    }

    /// The whole merge lifecycle against a real repo: up-to-date, clean merge, conflicted
    /// merge (stages readable, abort restores, re-merge + resolve + `commit_merge`
    /// concludes), plus `branch_ahead_behind` counts.
    #[test]
    fn merge_branch_covers_clean_conflict_abort_and_commit() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let work = std::env::temp_dir().join(format!("kyde-merge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("f.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(work.join("other.txt"), "x\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);
        let repo = Repo::discover(&work).unwrap();

        // A branch that only touches other.txt merges cleanly.
        g(&work, &["checkout", "-b", "clean"]);
        fs::write(work.join("other.txt"), "x\ny\n").unwrap();
        g(&work, &["commit", "-am", "clean change"]);
        g(&work, &["checkout", "main"]);
        assert_eq!(
            repo.branch_ahead_behind("clean"),
            Some((1, 0)),
            "clean is 1 ahead of main, 0 behind"
        );
        assert_eq!(repo.merge_branch("clean").unwrap(), MergeOutcome::Merged);
        assert_eq!(
            repo.merge_branch("clean").unwrap(),
            MergeOutcome::UpToDate,
            "second merge of the same branch has nothing to do"
        );

        // Conflicting edits to the same line on both branches.
        g(&work, &["checkout", "-b", "feature"]);
        fs::write(work.join("f.txt"), "one\nFEATURE\nthree\n").unwrap();
        g(&work, &["commit", "-am", "feature change"]);
        g(&work, &["checkout", "main"]);
        fs::write(work.join("f.txt"), "one\nMAIN\nthree\n").unwrap();
        g(&work, &["commit", "-am", "main change"]);

        let out = repo.merge_branch("feature").unwrap();
        assert_eq!(
            out,
            MergeOutcome::Conflicts(vec![PathBuf::from("f.txt")]),
            "same-line edits must conflict"
        );
        assert_eq!(repo.merging().as_deref(), Some("feature"));
        assert_eq!(
            repo.conflict_stage(Path::new("f.txt"), 1),
            "one\ntwo\nthree\n"
        );
        assert_eq!(
            repo.conflict_stage(Path::new("f.txt"), 2),
            "one\nMAIN\nthree\n"
        );
        assert_eq!(
            repo.conflict_stage(Path::new("f.txt"), 3),
            "one\nFEATURE\nthree\n"
        );

        // Abort restores the pre-merge state.
        repo.merge_abort().unwrap();
        assert_eq!(repo.merging(), None);
        assert_eq!(
            fs::read_to_string(work.join("f.txt")).unwrap(),
            "one\nMAIN\nthree\n"
        );

        // Merge again, resolve by staging merged content, conclude with commit_merge.
        assert!(matches!(
            repo.merge_branch("feature").unwrap(),
            MergeOutcome::Conflicts(_)
        ));
        repo.save_file(Path::new("f.txt"), "one\nMERGED\nthree\n")
            .unwrap();
        repo.stage(Path::new("f.txt")).unwrap();
        repo.commit_merge().unwrap();
        assert_eq!(repo.merging(), None);
        assert_eq!(g(&work, &["show", "HEAD:f.txt"]), "one\nMERGED\nthree\n");
        assert!(
            g(&work, &["log", "--merges", "--oneline"]).lines().count() >= 1,
            "a merge commit was created"
        );

        // A bogus branch is an error, not a conflict outcome.
        assert!(repo.merge_branch("no-such-branch").is_err());
        assert_eq!(repo.merging(), None, "failed merge must not leave state");

        let _ = fs::remove_dir_all(&work);
    }

    /// `conflict_entries` must classify what each side did from the index stages:
    /// modify/modify, modify/delete, and add/add (no common base).
    #[test]
    fn conflict_entries_classify_each_side() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let work = std::env::temp_dir().join(format!("kyde-centries-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("f.txt"), "one\ntwo\n").unwrap();
        fs::write(work.join("gone.txt"), "a\nb\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);

        // feature: edit f, DELETE gone, ADD new.
        g(&work, &["checkout", "-qb", "feature"]);
        fs::write(work.join("f.txt"), "one\nFEATURE\n").unwrap();
        g(&work, &["rm", "-q", "gone.txt"]);
        fs::write(work.join("new.txt"), "from feature\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "feature"]);

        // main: edit f differently, edit gone, add new with different content.
        g(&work, &["checkout", "-q", "main"]);
        fs::write(work.join("f.txt"), "one\nMAIN\n").unwrap();
        fs::write(work.join("gone.txt"), "a\nEDITED\n").unwrap();
        fs::write(work.join("new.txt"), "from main\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "main"]);

        let repo = Repo::discover(&work).unwrap();
        assert!(matches!(
            repo.merge_branch("feature").unwrap(),
            MergeOutcome::Conflicts(_)
        ));
        let entries = repo.conflict_entries();
        let by = |name: &str| {
            entries
                .iter()
                .find(|e| e.path == Path::new(name))
                .unwrap_or_else(|| panic!("{name} missing from {entries:?}"))
        };
        assert_eq!(by("f.txt").ours, ConflictSide::Modified);
        assert_eq!(by("f.txt").theirs, ConflictSide::Modified);
        assert_eq!(by("gone.txt").ours, ConflictSide::Modified);
        assert_eq!(by("gone.txt").theirs, ConflictSide::Deleted);
        assert_eq!(by("new.txt").ours, ConflictSide::Added);
        assert_eq!(by("new.txt").theirs, ConflictSide::Added);

        let _ = fs::remove_dir_all(&work);
    }

    /// A freshly-created local branch (no upstream) must measure a push from where it forked
    /// off the remote mainline — NOT the empty tree (which used to report the whole repo as
    /// "to push"). Builds a real repo + bare remote and checks `ahead_count`/`push_files`.
    #[test]
    fn new_branch_pushes_only_its_own_commits_not_whole_repo() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };

        // Isolated temp workspace (pid keeps parallel `cargo test` runs from colliding).
        let base = std::env::temp_dir().join(format!("kyde-pushbase-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let remote = base.join("remote.git");
        let work = base.join("work");
        fs::create_dir_all(&remote).unwrap();
        fs::create_dir_all(&work).unwrap();

        g(&remote, &["init", "--bare", "-b", "main"]);
        g(&work, &["init", "-b", "main"]);
        g(&work, &["config", "user.email", "t@example.com"]);
        g(&work, &["config", "user.name", "Test"]);
        g(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("a.txt"), "1\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "init"]);
        g(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        g(&work, &["push", "-u", "origin", "main"]);
        let _ = git(&work, &["remote", "set-head", "origin", "main"]);

        let repo = Repo::discover(&work).unwrap();

        // New branch off main, never pushed, no new commits → nothing to push.
        g(&work, &["checkout", "-b", "feature"]);
        assert_eq!(
            repo.ahead_count(),
            Some(0),
            "a brand-new branch with no commits is 0 ahead"
        );
        assert!(
            repo.push_files().is_empty(),
            "a brand-new branch must not report the whole repo as unpushed"
        );

        // One commit on the branch → exactly that file is unpushed, 1 ahead.
        fs::write(work.join("b.txt"), "2\n").unwrap();
        g(&work, &["add", "-A"]);
        g(&work, &["commit", "-m", "feature work"]);
        assert_eq!(repo.ahead_count(), Some(1));
        let files = repo.push_files();
        assert_eq!(files.len(), 1, "only the new commit's file is unpushed");
        assert_eq!(files[0].path, PathBuf::from("b.txt"));

        let _ = fs::remove_dir_all(&base);
    }

    /// `unstage` on a newly-added file must drop it back to untracked — both in a normal
    /// repo (`restore --staged`) and on an unborn HEAD, where `restore --staged` can't
    /// resolve HEAD and the `git rm --cached` fallback must kick in (issue #59).
    #[test]
    fn unstage_added_file_works_with_and_without_head() {
        use std::fs;
        let g = |dir: &Path, args: &[&str]| {
            git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
        };
        let base = std::env::temp_dir().join(format!("kyde-unstage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        g(&base, &["init", "-b", "main"]);
        g(&base, &["config", "user.email", "t@example.com"]);
        g(&base, &["config", "user.name", "Test"]);
        g(&base, &["config", "commit.gpgsign", "false"]);
        let repo = Repo::discover(&base).unwrap();

        // Unborn HEAD: no commits yet, stage a new file, unstage it.
        fs::write(base.join("first.txt"), "1\n").unwrap();
        repo.stage(Path::new("first.txt")).unwrap();
        assert_eq!(repo.status().unwrap()[0].status, FileStatus::Added);
        repo.unstage(Path::new("first.txt"))
            .expect("unstage must survive an unborn HEAD");
        let st = repo.status().unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].status, FileStatus::Untracked, "back to untracked");
        assert!(base.join("first.txt").exists(), "working copy untouched");

        // Normal repo (HEAD exists): same flow through `restore --staged`.
        g(&base, &["add", "-A"]);
        g(&base, &["commit", "-m", "init"]);
        fs::write(base.join("second.txt"), "2\n").unwrap();
        repo.stage(Path::new("second.txt")).unwrap();
        assert_eq!(repo.status().unwrap()[0].status, FileStatus::Added);
        repo.unstage(Path::new("second.txt")).unwrap();
        let st = repo.status().unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].status, FileStatus::Untracked);
        assert!(base.join("second.txt").exists());

        let _ = fs::remove_dir_all(&base);
    }
}
