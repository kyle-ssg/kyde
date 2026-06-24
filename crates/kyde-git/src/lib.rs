//! Git layer. Shells out to the `git` binary on a background-friendly API,
//! exactly like Zed's `crates/git` (no libgit2). Pure Rust — compiles standalone.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub status: FileStatus,
}

/// One entry in the git log (history view).
#[derive(Debug, Clone)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Relative date string (`git log --date=relative`), e.g. "2 hours ago".
    pub date: String,
    pub subject: String,
    /// Decoration refs (`%D`): e.g. "HEAD -> main, origin/main, tag: v1". Empty if none.
    pub refs: String,
}

pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Open repo containing `path` (walks up to the git root).
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let out = git(path.as_ref(), &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(out.trim());
        if root.as_os_str().is_empty() {
            return Err(anyhow!("not a git repository"));
        }
        Ok(Self { root })
    }

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
        let child = Command::new("git")
            .current_dir(&self.root)
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
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
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

    /// HEAD version of a file = the "before" side of the commit diff. We compare the working
    /// tree against HEAD (not the index) so the diff always shows the *full* change that will
    /// be committed — `commit_now` re-stages each checked file's working copy before
    /// committing, so working-vs-HEAD is exactly the committed delta. Using the index instead
    /// would render an empty diff for any file that's already fully staged. A file absent from
    /// HEAD (newly added) has no base → empty string (all-added).
    pub fn base_content(&self, rel: &Path) -> Result<String> {
        let p = rel.to_string_lossy();
        git(&self.root, &["show", &format!("HEAD:{}", p)]).or(Ok(String::new()))
    }

    /// Current on-disk content = the "after" side. Errors if the file is binary
    /// (contains a NUL byte) or not valid UTF-8, so callers treat it as
    /// not-diffable/not-editable rather than silently truncating it to empty
    /// (which, fed through the diff editor's autosave, would erase the file).
    pub fn working_content(&self, rel: &Path) -> Result<String> {
        let full = self.root.join(rel);
        let bytes = std::fs::read(&full).with_context(|| format!("reading {:?}", full))?;
        if bytes.contains(&0) {
            return Err(anyhow!("binary file: {:?}", rel));
        }
        String::from_utf8(bytes).map_err(|_| anyhow!("not valid UTF-8: {:?}", rel))
    }

    pub fn stage(&self, rel: &Path) -> Result<()> {
        git(&self.root, &["add", "--", &rel.to_string_lossy()]).map(|_| ())
    }

    pub fn unstage(&self, rel: &Path) -> Result<()> {
        git(
            &self.root,
            &["restore", "--staged", "--", &rel.to_string_lossy()],
        )
        .map(|_| ())
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
        std::fs::remove_file(self.root.join(rel)).map_err(Into::into)
    }

    /// Rename/move a working-tree file. Plain `fs::rename` (git picks up the
    /// rename on its next status), creating the destination's parent dirs.
    /// Refuses to clobber an existing destination.
    pub fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let (src, dst) = (self.root.join(from), self.root.join(to));
        if dst.exists() {
            return Err(anyhow!("{:?} already exists", to));
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::rename(&src, &dst).with_context(|| format!("renaming {:?} -> {:?}", src, dst))?;
        Ok(())
    }

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
                self.pull_rebase().map_err(|pe| {
                    anyhow!(
                        "remote has changes that conflict with yours — pull and resolve \
                         them manually ({pe})"
                    )
                })?;
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
        git(&self.root, &["rev-parse", "@{u}"]).ok()?;
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
        let n = format!("-n{}", limit);
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

    /// Write new content to a working-tree file (used by the editor on save).
    pub fn save_file(&self, rel: &Path, content: &str) -> Result<()> {
        let full = self.root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&full, content).with_context(|| format!("writing {:?}", full))?;
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
        return Err(anyhow!("empty branch name"));
    }
    if n.starts_with('-') {
        return Err(anyhow!("invalid branch name: {n:?}"));
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

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {:?}", args))?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_stdin(dir: &Path, args: &[&str], stdin: &str) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning git {:?}", args))?;
    child
        .stdin
        .take()
        .expect("stdin is piped (configured above)")
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
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
    fn valid_ref_rejects_flags_and_empty() {
        assert!(valid_ref("-f").is_err());
        assert!(valid_ref("--track").is_err());
        assert!(valid_ref("   ").is_err());
        assert!(valid_ref("").is_err());
        assert_eq!(valid_ref("feature/x").unwrap(), "feature/x");
        assert_eq!(valid_ref("  main  ").unwrap(), "main"); // trims
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
}
