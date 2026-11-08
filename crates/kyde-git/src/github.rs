//! GitHub issues and pull requests through the `gh` CLI — shelled out like `git`, so it uses
//! the user's existing `gh auth` (no tokens of our own). Network calls: run them off the UI
//! thread. `gh` never prompts here (`GH_PROMPT_DISABLED`), so a missing login fails fast.

use crate::{io_err, GitError, Result};
use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// An open issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Issue number.
    pub number: u64,
    /// Title.
    pub title: String,
    /// Web URL.
    pub url: String,
    /// Label names.
    pub labels: Vec<String>,
    /// Assignees' logins.
    pub assignees: Vec<String>,
}

/// Whose issues the Issues tab lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IssueFilter {
    /// Assigned to you.
    #[default]
    Mine,
    /// Everyone's.
    All,
    /// Nobody's yet.
    Unassigned,
}

/// Which pull requests the Pull Requests tab lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrFilter {
    /// Ones waiting on your review.
    #[default]
    ReviewRequested,
    /// Ones you opened.
    Mine,
    /// Every open PR.
    All,
}

/// An open pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// PR number.
    pub number: u64,
    /// Title.
    pub title: String,
    /// Web URL.
    pub url: String,
    /// Author's login.
    pub author: String,
    /// Head branch name.
    pub head: String,
    /// Still a draft.
    pub draft: bool,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}
#[derive(Deserialize)]
struct RawIssue {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    assignees: Vec<RawAuthor>,
}
#[derive(Deserialize)]
struct RawAuthor {
    login: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    number: u64,
    title: String,
    url: String,
    author: Option<RawAuthor>,
    head_ref_name: String,
    #[serde(default)]
    is_draft: bool,
}

/// The `gh` CLI: on `PATH`, else the usual install spots — a GUI-launched app gets a bare
/// `PATH` (`/usr/bin:/bin:…`) that misses Homebrew. Bare `gh` if none has it.
fn gh_bin() -> PathBuf {
    find_bin(
        "gh",
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// `name` in the `path` dirs, then Homebrew, `/usr/local`, `/opt/local` and `home`'s
/// `.local/bin`; bare `name` (left to the OS) if none has it.
fn find_bin(name: &str, path: Option<&OsStr>, home: Option<&OsStr>) -> PathBuf {
    path.map(std::env::split_paths)
        .into_iter()
        .flatten()
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from))
        .chain(home.map(|h| Path::new(h).join(".local/bin")))
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| name.into())
}

/// Run `gh <args>` in `dir`; its stdout, or its stderr as the error.
fn gh(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(gh_bin())
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => GitError::GhMissing,
            _ => io_err(format!("running gh {args:?}"))(source),
        })?;
    if !out.status.success() {
        return Err(GitError::Command {
            command: format!("gh {args:?}"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Open issues in `dir`'s GitHub repo — whose per `filter`, narrowed by `search` (GitHub's
/// search syntax: words, `assignee:x`, `label:bug`, …).
pub fn issues(dir: &Path, filter: IssueFilter, search: &str) -> Result<Vec<Issue>> {
    let mut query = search.trim().to_string();
    let mut args = vec![
        "issue",
        "list",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,title,url,labels,assignees",
    ];
    match filter {
        IssueFilter::Mine => args.extend(["--assignee", "@me"]),
        IssueFilter::All => {}
        IssueFilter::Unassigned => query = format!("{query} no:assignee").trim().to_string(),
    }
    if !query.is_empty() {
        args.extend(["--search", &query]);
    }
    parse_issues(&gh(dir, &args)?)
}

/// Parse `gh issue list --json number,title,url,labels`.
///
/// ```
/// let v = kyde_git::github::parse_issues(
///     r#"[{"number":7,"title":"Fix x","url":"u","labels":[{"name":"bug"}]}]"#,
/// ).unwrap();
/// assert_eq!((v[0].number, v[0].labels[0].as_str()), (7, "bug"));
/// ```
pub fn parse_issues(json: &str) -> Result<Vec<Issue>> {
    let raw: Vec<RawIssue> = serde_json::from_str(json).map_err(|source| GitError::Json {
        what: "gh issue list".into(),
        source,
    })?;
    Ok(raw
        .into_iter()
        .map(|r| Issue {
            number: r.number,
            title: r.title,
            url: r.url,
            labels: r.labels.into_iter().map(|l| l.name).collect(),
            assignees: r.assignees.into_iter().map(|a| a.login).collect(),
        })
        .collect())
}

/// Open pull requests in `dir`'s GitHub repo — which per `filter`, narrowed by `search`.
pub fn prs(dir: &Path, filter: PrFilter, search: &str) -> Result<Vec<PullRequest>> {
    let mut query = search.trim().to_string();
    let mut args = vec![
        "pr",
        "list",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,title,url,author,headRefName,isDraft",
    ];
    match filter {
        PrFilter::All => {}
        PrFilter::Mine => args.extend(["--author", "@me"]),
        PrFilter::ReviewRequested => {
            query = format!("{query} review-requested:@me").trim().to_string();
        }
    }
    if !query.is_empty() {
        args.extend(["--search", &query]);
    }
    let raw = gh(dir, &args)?;
    let raw: Vec<RawPr> = serde_json::from_str(&raw).map_err(|source| GitError::Json {
        what: "gh pr list".into(),
        source,
    })?;
    Ok(raw
        .into_iter()
        .map(|r| PullRequest {
            number: r.number,
            title: r.title,
            url: r.url,
            author: r.author.map(|a| a.login).unwrap_or_default(),
            head: r.head_ref_name,
            draft: r.is_draft,
        })
        .collect())
}

/// Check out pull request `number`'s branch in the worktree at `dir` (`gh pr checkout`, which
/// handles forks and sets the branch up to push back to the PR).
pub fn pr_checkout(dir: &Path, number: u64) -> Result<()> {
    gh(dir, &["pr", "checkout", &number.to_string()]).map(|_| ())
}

/// An issue or pull request in full, for the detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDetail {
    /// Number.
    pub number: u64,
    /// Title.
    pub title: String,
    /// Markdown body.
    pub body: String,
    /// Author's login.
    pub author: String,
    /// `OPEN` / `CLOSED` / `MERGED`.
    pub state: String,
    /// Web URL.
    pub url: String,
    /// Label names.
    pub labels: Vec<String>,
    /// Pull-request specifics (`None` for an issue).
    pub pr: Option<PrDetail>,
    /// The conversation: `(author, created at, markdown body, by a bot)`, oldest first.
    pub comments: Vec<(String, String, String, bool)>,
    /// Assignees' logins.
    pub assignees: Vec<String>,
    /// When it was opened (ISO 8601).
    pub created_at: String,
}

/// What a pull request changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrDetail {
    /// Head branch.
    pub head: String,
    /// Base branch.
    pub base: String,
    /// Lines added / deleted.
    pub additions: u64,
    /// Lines deleted.
    pub deletions: u64,
    /// Still a draft.
    pub draft: bool,
    /// Changed files: `(path, additions, deletions)`.
    pub files: Vec<(String, u64, u64)>,
    /// Its commits, oldest first.
    pub commits: Vec<PrCommit>,
    /// CI checks: `(name, status-or-conclusion, details url)`.
    pub checks: Vec<(String, String, String)>,
    /// Reviewers asked for a review.
    pub review_requests: Vec<String>,
    /// The head commit's hash (a merge is pinned to it).
    pub head_oid: String,
    /// `MERGEABLE` / `CONFLICTING` / `UNKNOWN` (still computing).
    pub mergeable: String,
    /// GitHub's `mergeStateStatus`: `CLEAN`, `BLOCKED` (branch protection), `BEHIND`,
    /// `DIRTY` (conflicts), `DRAFT`, `UNSTABLE` (failing checks), `UNKNOWN`, …
    pub merge_state: String,
    /// `APPROVED` / `CHANGES_REQUESTED` / `REVIEW_REQUIRED`, or empty (no review needed).
    pub review_decision: String,
}

impl PrDetail {
    /// Why GitHub won't merge it as it stands, or `None` when it would.
    ///
    /// ```
    /// let pr = |mergeable: &str, state: &str| kyde_git::github::PrDetail {
    ///     mergeable: mergeable.into(),
    ///     merge_state: state.into(),
    ///     ..Default::default()
    /// };
    /// assert!(pr("CONFLICTING", "DIRTY").merge_block().is_some());
    /// assert!(pr("MERGEABLE", "BLOCKED").merge_block().is_some());
    /// assert_eq!(pr("MERGEABLE", "CLEAN").merge_block(), None);
    /// assert_eq!(pr("UNKNOWN", "UNKNOWN").merge_block(), None); // still computing: let gh try
    /// ```
    #[must_use]
    pub fn merge_block(&self) -> Option<&'static str> {
        if self.draft || self.merge_state == "DRAFT" {
            Some("This pull request is still a draft")
        } else if self.mergeable == "CONFLICTING" || self.merge_state == "DIRTY" {
            Some("This branch has conflicts that must be resolved")
        } else if self.merge_state == "BLOCKED" {
            Some("Merging is blocked by the base branch's rules")
        } else {
            None
        }
    }
}

/// One commit of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCommit {
    /// Full hash.
    pub oid: String,
    /// First line of its message.
    pub headline: String,
    /// Author's login (or name).
    pub author: String,
    /// When committed (ISO 8601).
    pub date: String,
}

#[derive(Deserialize)]
struct RawFile {
    path: String,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommit {
    oid: String,
    #[serde(default)]
    message_headline: String,
    #[serde(default)]
    committed_date: String,
    #[serde(default)]
    authors: Vec<RawCommitAuthor>,
}
#[derive(Deserialize)]
struct RawCommitAuthor {
    #[serde(default)]
    login: String,
    #[serde(default)]
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheck {
    /// `CheckRun` → `name`; `StatusContext` → `context`.
    #[serde(default)]
    name: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    status: String,
    /// `CheckRun` → `conclusion`; `StatusContext` → `state`.
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    details_url: String,
    #[serde(default)]
    target_url: String,
}
#[derive(Deserialize)]
struct RawRequested {
    /// A user → `login`; a team → `name`.
    #[serde(default)]
    login: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetail {
    number: u64,
    title: String,
    #[serde(default)]
    assignees: Vec<RawAuthor>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    body: String,
    author: Option<RawAuthor>,
    #[serde(default)]
    state: String,
    url: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    head_ref_name: Option<String>,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    commits: Vec<RawCommit>,
    #[serde(default)]
    status_check_rollup: Vec<RawCheck>,
    #[serde(default)]
    review_requests: Vec<RawRequested>,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    mergeable: String,
    #[serde(default)]
    merge_state_status: String,
    #[serde(default)]
    review_decision: String,
}

/// Issue `number` in full (`gh issue view`).
pub fn issue_detail(dir: &Path, number: u64) -> Result<ItemDetail> {
    detail(
        dir,
        number,
        &[
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "number,title,body,author,state,url,labels,assignees,createdAt",
        ],
        "gh issue view",
    )
}

/// Pull request `number` in full (`gh pr view`), incl. its branches and changed files.
pub fn pr_detail(dir: &Path, number: u64) -> Result<ItemDetail> {
    detail(
        dir,
        number,
        &[
            "pr", "view", &number.to_string(), "--json",
            "number,title,body,author,state,url,labels,headRefName,baseRefName,additions,deletions,isDraft,files,assignees,createdAt,commits,statusCheckRollup,reviewRequests,headRefOid,mergeable,mergeStateStatus,reviewDecision",
        ],
        "gh pr view",
    )
}

/// `gh <args>` parsed as `what`, with its [`comments`] fetched side by side.
fn detail(dir: &Path, number: u64, args: &[&str], what: &str) -> Result<ItemDetail> {
    let (raw, comments) = std::thread::scope(|s| {
        let comments = s.spawn(|| comments(dir, number));
        (gh(dir, args), join(comments))
    });
    let mut d = parse_detail(&raw?, what)?;
    d.comments = comments?;
    Ok(d)
}

/// Issue or pull request `number`'s conversation, oldest first (`gh api …/issues/N/comments`:
/// unlike `gh … view`, REST says which authors are bots).
pub fn comments(dir: &Path, number: u64) -> Result<Vec<(String, String, String, bool)>> {
    Ok(
        api_items::<RawIssueComment>(dir, &format!("issues/{number}/comments"))?
            .into_iter()
            .map(RawIssueComment::entry)
            .collect(),
    )
}

/// Parse `gh issue view` / `gh pr view --json …` (a PR has `headRefName`). Its `comments`
/// are left empty — they come from [`comments`].
///
/// ```
/// let d = kyde_git::github::parse_detail(
///     r#"{"number":3,"title":"T","body":"b","author":{"login":"k"},"state":"OPEN","url":"u",
///        "labels":[],"headRefName":"feat/x","baseRefName":"main","additions":2,"deletions":1,
///        "isDraft":false,"files":[{"path":"a.rs","additions":2,"deletions":1}]}"#,
///     "pr",
/// ).unwrap();
/// let pr = d.pr.unwrap();
/// assert_eq!((pr.head.as_str(), pr.files[0].0.as_str()), ("feat/x", "a.rs"));
/// ```
pub fn parse_detail(json: &str, what: &str) -> Result<ItemDetail> {
    let r: RawDetail = serde_json::from_str(json).map_err(|source| GitError::Json {
        what: what.into(),
        source,
    })?;
    Ok(ItemDetail {
        number: r.number,
        title: r.title,
        body: r.body,
        author: r.author.map(|a| a.login).unwrap_or_default(),
        state: r.state,
        url: r.url,
        labels: r.labels.into_iter().map(|l| l.name).collect(),
        pr: r.head_ref_name.map(|head| PrDetail {
            head,
            base: r.base_ref_name,
            additions: r.additions,
            deletions: r.deletions,
            draft: r.is_draft,
            files: r
                .files
                .into_iter()
                .map(|f| (f.path, f.additions, f.deletions))
                .collect(),
            commits: r
                .commits
                .into_iter()
                .map(|c| PrCommit {
                    oid: c.oid,
                    headline: c.message_headline,
                    author: c
                        .authors
                        .into_iter()
                        .next()
                        .map(|a| if a.login.is_empty() { a.name } else { a.login })
                        .unwrap_or_default(),
                    date: c.committed_date,
                })
                .collect(),
            checks: r
                .status_check_rollup
                .into_iter()
                .map(|c| {
                    let name = if c.name.is_empty() { c.context } else { c.name };
                    let result = [c.conclusion, c.state, c.status]
                        .into_iter()
                        .find(|s| !s.is_empty())
                        .unwrap_or_default();
                    let url = if c.details_url.is_empty() {
                        c.target_url
                    } else {
                        c.details_url
                    };
                    (name, result, url)
                })
                .collect(),
            review_requests: r
                .review_requests
                .into_iter()
                .map(|q| if q.login.is_empty() { q.name } else { q.login })
                .collect(),
            head_oid: r.head_ref_oid,
            mergeable: r.mergeable,
            merge_state: r.merge_state_status,
            review_decision: r.review_decision,
        }),
        comments: Vec::new(),
        assignees: r.assignees.into_iter().map(|a| a.login).collect(),
        created_at: r.created_at,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHead {
    head_ref_name: String,
}

/// Head branches of the merged pull requests in `dir`'s repo (the most recent 200) — a
/// worktree on one of them is finished.
pub fn merged_heads(dir: &Path) -> Result<std::collections::HashSet<String>> {
    let raw = gh(
        dir,
        &[
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "200",
            "--json",
            "headRefName",
        ],
    )?;
    let heads: Vec<RawHead> = serde_json::from_str(&raw).map_err(|source| GitError::Json {
        what: "gh pr list --state merged".into(),
        source,
    })?;
    Ok(heads.into_iter().map(|h| h.head_ref_name).collect())
}

/// Pull request `number`'s changes as a unified diff (`gh pr diff`, no checkout needed).
pub fn pr_diff(dir: &Path, number: u64) -> Result<String> {
    gh(
        dir,
        &["pr", "diff", &number.to_string(), "--color", "never"],
    )
}

/// One row of a pull request's diff, for a line-by-line view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRow {
    /// A changed file: its path and how many lines it adds / deletes.
    File {
        /// The file's path (its new path when renamed).
        path: String,
        /// Lines added.
        additions: usize,
        /// Lines deleted.
        deletions: usize,
    },
    /// A hunk header (`@@ -a,b +c,d @@ context`).
    Hunk(String),
    /// An added line (without its `+`).
    Add(String),
    /// A deleted line (without its `-`).
    Del(String),
    /// An unchanged context line.
    Ctx(String),
}

/// Split a unified diff into [`DiffRow`]s: a `File` row per file (with its +/− counts), then
/// its hunks' lines. Git's per-file header lines are dropped.
///
/// ```
/// use kyde_git::github::{parse_diff, DiffRow};
/// let rows = parse_diff("diff --git a/x.rs b/x.rs\nindex 1..2\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-old\n+new\n same\n");
/// assert_eq!(rows[0], DiffRow::File { path: "x.rs".into(), additions: 1, deletions: 1 });
/// assert_eq!(rows[2], DiffRow::Del("old".into()));
/// ```
#[must_use]
pub fn parse_diff(diff: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut file: Option<usize> = None;
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).to_string();
            file = Some(rows.len());
            rows.push(DiffRow::File {
                path,
                additions: 0,
                deletions: 0,
            });
            in_hunk = false;
            continue;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            rows.push(DiffRow::Hunk(line.to_string()));
            continue;
        }
        if !in_hunk {
            continue; // index / --- / +++ / mode / rename / binary headers
        }
        let (add, del) = match line.as_bytes().first() {
            Some(b'+') => {
                rows.push(DiffRow::Add(line[1..].to_string()));
                (1, 0)
            }
            Some(b'-') => {
                rows.push(DiffRow::Del(line[1..].to_string()));
                (0, 1)
            }
            Some(b'\\') => (0, 0), // "\ No newline at end of file"
            _ => {
                rows.push(DiffRow::Ctx(line.get(1..).unwrap_or("").to_string()));
                (0, 0)
            }
        };
        if let Some(DiffRow::File {
            additions,
            deletions,
            ..
        }) = file.and_then(|i| rows.get_mut(i))
        {
            *additions += add;
            *deletions += del;
        }
    }
    rows
}

/// Post `body` as a comment on issue / pull request `number` (`gh issue|pr comment`).
pub fn comment(dir: &Path, pr: bool, number: u64, body: &str) -> Result<()> {
    let kind = if pr { "pr" } else { "issue" };
    gh(dir, &[kind, "comment", &number.to_string(), "--body", body]).map(|_| ())
}

/// Close issue `number` (`gh issue close`).
pub fn close_issue(dir: &Path, number: u64) -> Result<()> {
    gh(dir, &["issue", "close", &number.to_string()]).map(|_| ())
}

/// Reopen issue `number` (`gh issue reopen`).
pub fn reopen_issue(dir: &Path, number: u64) -> Result<()> {
    gh(dir, &["issue", "reopen", &number.to_string()]).map(|_| ())
}

/// Approve pull request `number`, with `body` as the review's comment when it isn't empty
/// (`gh pr review --approve`).
pub fn approve(dir: &Path, number: u64, body: &str) -> Result<()> {
    let n = number.to_string();
    let mut args = vec!["pr", "review", &n, "--approve"];
    if !body.is_empty() {
        args.extend(["--body", body]);
    }
    gh(dir, &args).map(|_| ())
}

/// How a pull request goes into its base branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    /// A merge commit.
    Merge,
    /// One squashed commit.
    Squash,
    /// Its commits, rebased onto the base.
    Rebase,
}

/// What `dir`'s repo lets you merge with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMerge {
    /// The methods the repo allows, in GitHub's order (merge, squash, rebase).
    pub methods: Vec<MergeMethod>,
    /// The one GitHub picks for you (your last used).
    pub default: Option<MergeMethod>,
    /// You have write access (merging needs it).
    pub can_merge: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepoMerge {
    #[serde(default)]
    merge_commit_allowed: bool,
    #[serde(default)]
    squash_merge_allowed: bool,
    #[serde(default)]
    rebase_merge_allowed: bool,
    #[serde(default)]
    viewer_default_merge_method: String,
    #[serde(default)]
    viewer_permission: String,
}

/// `dir`'s repo's merge settings, as they apply to you (`gh repo view`).
pub fn repo_merge(dir: &Path) -> Result<RepoMerge> {
    let raw = gh(
        dir,
        &[
            "repo",
            "view",
            "--json",
            "mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,viewerDefaultMergeMethod,viewerPermission",
        ],
    )?;
    parse_repo_merge(&raw)
}

/// Parse `gh repo view --json mergeCommitAllowed,…,viewerPermission`.
///
/// ```
/// use kyde_git::github::{parse_repo_merge, MergeMethod};
/// let m = parse_repo_merge(
///     r#"{"mergeCommitAllowed":false,"squashMergeAllowed":true,"rebaseMergeAllowed":true,
///        "viewerDefaultMergeMethod":"REBASE","viewerPermission":"WRITE"}"#,
/// ).unwrap();
/// assert_eq!(m.methods, [MergeMethod::Squash, MergeMethod::Rebase]);
/// assert_eq!((m.default, m.can_merge), (Some(MergeMethod::Rebase), true));
/// let read = parse_repo_merge(r#"{"squashMergeAllowed":true,"viewerPermission":"READ"}"#);
/// assert!(!read.unwrap().can_merge);
/// ```
pub fn parse_repo_merge(json: &str) -> Result<RepoMerge> {
    let r: RawRepoMerge = serde_json::from_str(json).map_err(|source| GitError::Json {
        what: "gh repo view".into(),
        source,
    })?;
    let methods: Vec<MergeMethod> = [
        (r.merge_commit_allowed, MergeMethod::Merge),
        (r.squash_merge_allowed, MergeMethod::Squash),
        (r.rebase_merge_allowed, MergeMethod::Rebase),
    ]
    .into_iter()
    .filter_map(|(on, m)| on.then_some(m))
    .collect();
    let default = match r.viewer_default_merge_method.as_str() {
        "MERGE" => Some(MergeMethod::Merge),
        "SQUASH" => Some(MergeMethod::Squash),
        "REBASE" => Some(MergeMethod::Rebase),
        _ => None,
    }
    .filter(|m| methods.contains(m));
    Ok(RepoMerge {
        methods,
        default,
        can_merge: matches!(r.viewer_permission.as_str(), "ADMIN" | "MAINTAIN" | "WRITE"),
    })
}

/// Merge pull request `number` by `method` — only if its head is still `head` (the commit
/// you looked at), so a push made meanwhile isn't merged unseen (`gh pr merge`).
pub fn merge(dir: &Path, number: u64, method: MergeMethod, head: &str) -> Result<()> {
    let flag = match method {
        MergeMethod::Merge => "--merge",
        MergeMethod::Squash => "--squash",
        MergeMethod::Rebase => "--rebase",
    };
    let n = number.to_string();
    let mut args = vec!["pr", "merge", &n, flag];
    if !head.is_empty() {
        args.extend(["--match-head-commit", head]);
    }
    gh(dir, &args).map(|_| ())
}

/// A submitted pull-request review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// REST id (what its inline comments point back to).
    pub id: u64,
    /// Reviewer's login.
    pub author: String,
    /// `APPROVED` / `CHANGES_REQUESTED` / `COMMENTED` / `DISMISSED`.
    pub state: String,
    /// Its summary comment (often empty).
    pub body: String,
    /// When submitted (ISO 8601).
    pub at: String,
}

/// An inline review thread: the code it's on and its comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    /// The review the thread started in.
    pub review: u64,
    /// The first comment's id — replies go to it.
    pub root: u64,
    /// The file it's on.
    pub path: String,
    /// The diff hunk it's anchored to (git's `@@ … @@` + lines).
    pub hunk: String,
    /// The line it's on — in the code it was left on when that has changed since (outdated).
    pub line: Option<u64>,
    /// Marked resolved on GitHub (`None`: its state couldn't be read).
    pub resolved: Option<bool>,
    /// `(author, at, markdown body)`, oldest first.
    pub comments: Vec<(String, String, String)>,
}

#[derive(Deserialize)]
struct RawUser {
    login: String,
    /// `User` / `Bot` / `Organization`.
    #[serde(default, rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct RawIssueComment {
    user: Option<RawUser>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
}

impl RawIssueComment {
    /// `(author, at, body, by a bot)`; a bot's login as GitHub shows it, without REST's `[bot]`.
    fn entry(self) -> (String, String, String, bool) {
        let (author, bot) = match self.user {
            Some(u) if u.kind == "Bot" => {
                let login = u.login.strip_suffix("[bot]").unwrap_or(&u.login);
                (login.to_string(), true)
            }
            Some(u) => (u.login, false),
            None => (String::new(), false),
        };
        (author, self.created_at, self.body, bot)
    }
}
#[derive(Deserialize)]
struct RawReview {
    id: u64,
    user: Option<RawUser>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    submitted_at: String,
}
#[derive(Deserialize)]
struct RawReviewComment {
    id: u64,
    #[serde(default)]
    pull_request_review_id: Option<u64>,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    diff_hunk: String,
    /// `null` once the code it's on has changed (outdated) — then `original_line` has it.
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    original_line: Option<u64>,
    user: Option<RawUser>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
}

/// Pull request `number`'s reviews and inline review threads (`gh api …/pulls/N/reviews`
/// and `…/comments`, plus which threads are resolved — GraphQL only), oldest first. The
/// three calls run side by side. A thread's `resolved` is `None` when that last one fails.
pub fn reviews(dir: &Path, number: u64) -> Result<(Vec<Review>, Vec<ReviewThread>)> {
    let (reviews, comments, states) = std::thread::scope(|s| {
        let comments =
            s.spawn(|| api_items::<RawReviewComment>(dir, &format!("pulls/{number}/comments")));
        let states = s.spawn(|| thread_states(dir, number));
        let reviews = api_items::<RawReview>(dir, &format!("pulls/{number}/reviews"));
        (reviews, join(comments), join(states))
    });
    let (reviews, comments, states) = (reviews?, comments?, states.ok());
    let login = |u: Option<RawUser>| u.map(|u| u.login).unwrap_or_default();
    let reviews = reviews
        .into_iter()
        .map(|r| Review {
            id: r.id,
            author: login(r.user),
            state: r.state,
            body: r.body,
            at: r.submitted_at,
        })
        .collect();
    // Thread = a root comment + the replies pointing at it.
    let mut threads: Vec<ReviewThread> = Vec::new();
    for c in comments {
        let entry = (login(c.user), c.created_at, c.body);
        match c
            .in_reply_to_id
            .and_then(|root| threads.iter_mut().find(|t| t.root == root))
        {
            Some(t) => t.comments.push(entry),
            None => threads.push(ReviewThread {
                review: c.pull_request_review_id.unwrap_or(0),
                root: c.id,
                path: c.path,
                hunk: c.diff_hunk,
                line: c.line.or(c.original_line),
                resolved: states.as_ref().and_then(|s| s.get(&c.id).copied()),
                comments: vec![entry],
            }),
        }
    }
    Ok((reviews, threads))
}

/// A scoped thread's result, its panic passed on.
fn join<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
    h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// Whether each of pull request `number`'s review threads is resolved, keyed by its first
/// comment's REST id (GraphQL's `fullDatabaseId`) — REST has no resolved state.
pub fn thread_states(dir: &Path, number: u64) -> Result<std::collections::HashMap<u64, bool>> {
    const QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!,$endCursor:String){\
        repository(owner:$owner,name:$repo){pullRequest(number:$number){\
        reviewThreads(first:100,after:$endCursor){pageInfo{hasNextPage endCursor} \
        nodes{isResolved comments(first:1){nodes{fullDatabaseId}}}}}}}";
    let raw = gh(
        dir,
        &[
            "api",
            "graphql",
            "--paginate",
            "-F",
            "owner={owner}",
            "-F",
            "repo={repo}",
            "-F",
            &format!("number={number}"),
            "-f",
            &format!("query={QUERY}"),
            "--jq",
            ".data.repository.pullRequest.reviewThreads.nodes[]",
        ],
    )?;
    parse_thread_states(&raw)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreadState {
    #[serde(default)]
    is_resolved: bool,
    comments: RawNodes,
}
#[derive(Deserialize)]
struct RawNodes {
    #[serde(default)]
    nodes: Vec<RawFirstComment>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFirstComment {
    /// A GraphQL `BigInt` — a JSON string.
    #[serde(default)]
    full_database_id: Option<serde_json::Value>,
}

/// Parse [`thread_states`]' output: one review-thread JSON object per line.
///
/// ```
/// let s = kyde_git::github::parse_thread_states(concat!(
///     r#"{"isResolved":true,"comments":{"nodes":[{"fullDatabaseId":"4312305778"}]}}"#, "\n",
///     r#"{"isResolved":false,"comments":{"nodes":[{"fullDatabaseId":"17"}]}}"#, "\n",
/// )).unwrap();
/// assert_eq!((s[&4312305778], s[&17]), (true, false));
/// ```
pub fn parse_thread_states(text: &str) -> Result<std::collections::HashMap<u64, bool>> {
    let mut out = std::collections::HashMap::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        let t: RawThreadState = serde_json::from_str(l).map_err(|source| GitError::Json {
            what: "gh api graphql reviewThreads".into(),
            source,
        })?;
        let id = t
            .comments
            .nodes
            .into_iter()
            .next()
            .and_then(|c| c.full_database_id)
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
        if let Some(id) = id {
            out.insert(id, t.is_resolved);
        }
    }
    Ok(out)
}

/// Where a GitHub Actions run (or job, or step) stands, as the CI tab groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Queued, waiting or running.
    Running,
    /// Finished and failed, timed out, was cancelled or couldn't start.
    Failed,
    /// Finished fine (or was skipped).
    Passed,
}

impl RunState {
    /// The state GitHub's `status` + `conclusion` pair means.
    #[must_use]
    pub fn of(status: &str, conclusion: &str) -> Self {
        if !status.eq_ignore_ascii_case("completed") {
            return Self::Running;
        }
        match conclusion.to_ascii_lowercase().as_str() {
            "success" | "neutral" | "skipped" => Self::Passed,
            _ => Self::Failed,
        }
    }
}

/// A GitHub Actions workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    /// Run id (what `gh run view` / `gh run rerun` take).
    pub id: u64,
    /// The workflow's run number (`#373`).
    pub number: u64,
    /// The workflow's name (`build`).
    pub workflow: String,
    /// What it ran for: the commit's or PR's title.
    pub title: String,
    /// The branch it ran on.
    pub branch: String,
    /// What started it (`push`, `pull_request`, …).
    pub event: String,
    /// `queued` / `in_progress` / `completed` / …
    pub status: String,
    /// Once completed: `success` / `failure` / `cancelled` / …
    pub conclusion: String,
    /// Web URL.
    pub url: String,
    /// When it was created (ISO 8601).
    pub created_at: String,
    /// Which attempt this is (each re-run counts up from 1).
    pub attempt: u64,
    /// The id of the latest run of the same workflow on the same branch, when that's a newer
    /// one — this run is then out of date (re-running an old deploy would roll it back). Set
    /// by [`parse_runs`] from the runs listed with it; `None` from [`run_detail`].
    pub superseded_by: Option<u64>,
}

impl WorkflowRun {
    /// Where it stands.
    #[must_use]
    pub fn state(&self) -> RunState {
        RunState::of(&self.status, &self.conclusion)
    }
}

/// One job of a workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Job id (also its check run's id).
    pub id: u64,
    /// Its name (`test (ubuntu-latest)`).
    pub name: String,
    /// `queued` / `in_progress` / `completed` / …
    pub status: String,
    /// Once completed: `success` / `failure` / `cancelled` / …
    pub conclusion: String,
    /// When it started / finished (ISO 8601; empty until then).
    pub started_at: String,
    /// When it finished.
    pub completed_at: String,
    /// Web URL (its log on GitHub).
    pub url: String,
    /// Its steps, in order: `(name, state)`.
    pub steps: Vec<(String, RunState)>,
    /// Why it failed in GitHub's words: its failure annotations (empty unless it failed).
    pub errors: Vec<Annotation>,
}

impl Job {
    /// Where it stands.
    #[must_use]
    pub fn state(&self) -> RunState {
        RunState::of(&self.status, &self.conclusion)
    }
}

/// A failure annotation on a job: a problem matcher's error (a file and line), or the
/// runner's own reason ("The job was not acquired by Runner…").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    /// The file it's on — empty for one about the job itself.
    pub path: String,
    /// Its line in that file.
    pub line: Option<u64>,
    /// What went wrong (its title first, when it has one).
    pub message: String,
}

/// A workflow run in full, for the detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDetail {
    /// The run itself (its latest attempt).
    pub run: WorkflowRun,
    /// Its jobs, in GitHub's order.
    pub jobs: Vec<Job>,
}

/// The `--json` fields of a run, for `gh run list` / `gh run view`.
const RUN_FIELDS: &str =
    "databaseId,number,attempt,workflowName,displayTitle,headBranch,event,status,conclusion,url,createdAt";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRun {
    database_id: u64,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    attempt: u64,
    #[serde(default)]
    workflow_name: String,
    #[serde(default)]
    display_title: String,
    #[serde(default)]
    head_branch: String,
    #[serde(default)]
    event: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    created_at: String,
}

impl From<RawRun> for WorkflowRun {
    fn from(r: RawRun) -> Self {
        Self {
            id: r.database_id,
            number: r.number,
            workflow: r.workflow_name,
            title: r.display_title,
            branch: r.head_branch,
            event: r.event,
            status: r.status,
            conclusion: r.conclusion,
            url: r.url,
            created_at: r.created_at,
            attempt: r.attempt,
            superseded_by: None,
        }
    }
}

#[derive(Deserialize)]
struct RawRunView {
    #[serde(flatten)]
    run: RawRun,
    #[serde(default)]
    jobs: Vec<RawJob>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawJob {
    database_id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: String,
    #[serde(default)]
    started_at: String,
    #[serde(default)]
    completed_at: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    steps: Vec<RawStep>,
}
#[derive(Deserialize)]
struct RawStep {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: String,
}
#[derive(Deserialize)]
struct RawAnnotation {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start_line: Option<u64>,
    #[serde(default)]
    annotation_level: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    message: String,
}

/// The latest workflow runs in `dir`'s GitHub repo (all workflows, newest first).
pub fn runs(dir: &Path) -> Result<Vec<WorkflowRun>> {
    parse_runs(&gh(
        dir,
        &["run", "list", "--limit", "50", "--json", RUN_FIELDS],
    )?)
}

/// Parse `gh run list --json databaseId,number,…` (see `RUN_FIELDS`; newest first), marking
/// each run a newer one of the same workflow on the same branch has superseded.
///
/// ```
/// use kyde_git::github::{parse_runs, RunState};
/// let v = parse_runs(
///     r#"[{"databaseId":42,"number":8,"attempt":1,"workflowName":"build","displayTitle":"Fix x",
///         "headBranch":"main","event":"push","status":"in_progress","conclusion":""},
///        {"databaseId":41,"number":7,"workflowName":"build","headBranch":"main",
///         "status":"completed","conclusion":"timed_out"},
///        {"databaseId":40,"number":3,"workflowName":"deploy","headBranch":"main",
///         "status":"completed","conclusion":"skipped"}]"#,
/// ).unwrap();
/// assert_eq!((v[0].id, v[0].title.as_str()), (42, "Fix x"));
/// let states: Vec<RunState> = v.iter().map(|r| r.state()).collect();
/// assert_eq!(states, [RunState::Running, RunState::Failed, RunState::Passed]);
/// let superseded: Vec<Option<u64>> = v.iter().map(|r| r.superseded_by).collect();
/// assert_eq!(superseded, [None, Some(42), None]); // the older `build` on main is out of date
/// ```
pub fn parse_runs(json: &str) -> Result<Vec<WorkflowRun>> {
    let raw: Vec<RawRun> = serde_json::from_str(json).map_err(|source| GitError::Json {
        what: "gh run list".into(),
        source,
    })?;
    let mut latest: std::collections::HashMap<(String, String), u64> =
        std::collections::HashMap::new();
    Ok(raw
        .into_iter()
        .map(|r| {
            let mut run = WorkflowRun::from(r);
            let key = (run.workflow.clone(), run.branch.clone());
            match latest.get(&key) {
                Some(&newer) => run.superseded_by = Some(newer),
                None => {
                    latest.insert(key, run.id);
                }
            }
            run
        })
        .collect())
}

/// Run `id` in full (`gh run view`): its latest attempt and jobs, each failed job with its
/// failure annotations (fetched side by side; a job whose annotations can't be read just
/// has none — its log still says why).
pub fn run_detail(dir: &Path, id: u64) -> Result<RunDetail> {
    let fields = format!("{RUN_FIELDS},jobs");
    let raw = gh(dir, &["run", "view", &id.to_string(), "--json", &fields])?;
    let raw: RawRunView = serde_json::from_str(&raw).map_err(|source| GitError::Json {
        what: "gh run view".into(),
        source,
    })?;
    let mut jobs: Vec<Job> = raw
        .jobs
        .into_iter()
        .map(|j| Job {
            id: j.database_id,
            name: j.name,
            status: j.status,
            conclusion: j.conclusion,
            started_at: j.started_at,
            completed_at: j.completed_at,
            url: j.url,
            steps: j
                .steps
                .into_iter()
                .map(|s| {
                    let state = RunState::of(&s.status, &s.conclusion);
                    (s.name, state)
                })
                .collect(),
            errors: Vec::new(),
        })
        .collect();
    std::thread::scope(|s| {
        let fetches: Vec<_> = jobs
            .iter_mut()
            .filter(|j| j.state() == RunState::Failed)
            .map(|j| {
                let id = j.id;
                (s.spawn(move || job_errors(dir, id)), j)
            })
            .collect();
        for (h, j) in fetches {
            j.errors = join(h).unwrap_or_default();
        }
    });
    Ok(RunDetail {
        run: raw.run.into(),
        jobs,
    })
}

/// Job `id`'s failure annotations, each once (`gh api …/check-runs/N/annotations`). One
/// about the job itself, not a file, comes with an empty path.
fn job_errors(dir: &Path, id: u64) -> Result<Vec<Annotation>> {
    let mut out: Vec<Annotation> = Vec::new();
    for a in api_items::<RawAnnotation>(dir, &format!("check-runs/{id}/annotations"))? {
        if a.annotation_level != "failure" {
            continue;
        }
        // GitHub files the runner's own errors under `.github`, line 1.
        let about_job = a.path == ".github";
        let message = if a.title.is_empty() || a.message.starts_with(&a.title) {
            a.message
        } else {
            format!("{}: {}", a.title, a.message)
        };
        let annotation = Annotation {
            path: if about_job { String::new() } else { a.path },
            line: a.start_line.filter(|_| !about_job),
            message,
        };
        if !out.contains(&annotation) {
            out.push(annotation);
        }
    }
    Ok(out)
}

/// One row of a run's failed-steps log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogRow {
    /// A step's output starts: its job and step names.
    Step {
        /// The job's name.
        job: String,
        /// The step's name — empty when `gh` couldn't tell (it then logs the whole job).
        step: String,
    },
    /// A line of output, without the runner's timestamp or ANSI colours.
    Line {
        /// The text.
        text: String,
        /// How it reads.
        level: LogLevel,
    },
}

/// How a log line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Ordinary output.
    Plain,
    /// The runner's verdict on a step that failed (`##[error]…`) — the cause is just above.
    Failure,
    /// An error in the output (a line starting `error:` / `error[` / `Error:`).
    Error,
    /// A warning (`##[warning]…` / `warning:`).
    Warning,
    /// A group's title (`##[group]…`) — usually the command a step ran.
    Group,
}

/// The output of run `id`'s failed steps (`gh run view --log-failed`), row by row. Only a
/// finished run has its logs.
pub fn run_failed_log(dir: &Path, id: u64) -> Result<Vec<LogRow>> {
    Ok(parse_failed_log(&gh(
        dir,
        &["run", "view", &id.to_string(), "--log-failed"],
    )?))
}

/// Split `gh run view --log-failed` output (`job⇥step⇥<timestamp> text` per line) into
/// [`LogRow`]s: a `Step` row where each step's output starts, then its lines — timestamps,
/// ANSI colours and the runner's `##[group]` / `##[error]` markers taken off. `gh` names a
/// step it can't place `UNKNOWN STEP`; that comes back as an empty step name.
///
/// ```
/// use kyde_git::github::{parse_failed_log, LogLevel, LogRow};
/// let rows = parse_failed_log(concat!(
///     "test\tRun cargo test\t\u{feff}2026-11-04T17:10:15.1234567Z ##[group]Run cargo test\n",
///     "test\tRun cargo test\t2026-11-04T17:10:16.1234567Z \x1b[31merror[E0425]\x1b[0m: x\n",
///     "test\tRun cargo test\t2026-11-04T17:10:17.1234567Z ##[endgroup]\n",
///     "test\tRun cargo test\t2026-11-04T17:10:18.1234567Z ##[error]Process completed with exit code 101.\n",
///     "lint\tUNKNOWN STEP\t2026-11-04T17:10:19.1234567Z ok\n",
/// ));
/// assert_eq!(rows[0], LogRow::Step { job: "test".into(), step: "Run cargo test".into() });
/// assert_eq!(rows[1], LogRow::Line { text: "Run cargo test".into(), level: LogLevel::Group });
/// assert_eq!(rows[2], LogRow::Line { text: "error[E0425]: x".into(), level: LogLevel::Error });
/// assert!(matches!(rows[3], LogRow::Line { level: LogLevel::Failure, .. })); // `##[endgroup]` dropped
/// assert_eq!(rows[4], LogRow::Step { job: "lint".into(), step: String::new() });
/// ```
#[must_use]
pub fn parse_failed_log(log: &str) -> Vec<LogRow> {
    let mut rows = Vec::new();
    let mut at: Option<(&str, &str)> = None;
    for raw in log.lines() {
        let mut parts = raw.splitn(3, '\t');
        let (job, step, line) = match (parts.next(), parts.next(), parts.next()) {
            (Some(job), Some(step), Some(line)) => (job, step, line),
            _ => ("", "", raw),
        };
        if !job.is_empty() && at != Some((job, step)) {
            rows.push(LogRow::Step {
                job: job.to_string(),
                step: if step == "UNKNOWN STEP" { "" } else { step }.to_string(),
            });
            at = Some((job, step));
        }
        let text = strip_ansi(strip_stamp(line.trim_start_matches('\u{feff}')));
        let marked = |m: &str| text.strip_prefix(m).map(str::to_string);
        let (level, text) = if let Some(t) = marked("##[error]") {
            (LogLevel::Failure, t)
        } else if let Some(t) = marked("##[warning]") {
            (LogLevel::Warning, t)
        } else if let Some(t) = marked("##[group]") {
            (LogLevel::Group, t)
        } else if text.starts_with("##[endgroup]") {
            continue;
        } else if ["error:", "error[", "Error:"]
            .iter()
            .any(|p| text.trim_start().starts_with(p))
        {
            (LogLevel::Error, text)
        } else if text.trim_start().starts_with("warning:") {
            (LogLevel::Warning, text)
        } else {
            (LogLevel::Plain, text)
        };
        rows.push(LogRow::Line { text, level });
    }
    rows
}

/// `line` without the runner's leading `2026-11-04T17:10:15.1234567Z ` timestamp.
fn strip_stamp(line: &str) -> &str {
    let (stamp, rest) = line.split_once(' ').unwrap_or((line, ""));
    let b = stamp.as_bytes();
    let is_stamp = b.len() >= 20
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
        && stamp.ends_with('Z');
    if is_stamp {
        rest
    } else {
        line
    }
}

/// `s` without ANSI escape sequences (colours, cursor moves).
fn strip_ansi(s: &str) -> String {
    if !s.contains('\u{1b}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
        } else if chars.next_if_eq(&'[').is_some() {
            // CSI: parameters up to a final byte in `@`..=`~`.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// Re-run run `id` (`gh run rerun`): only its failed (and cancelled) jobs, or all of them.
pub fn rerun(dir: &Path, id: u64, failed_only: bool) -> Result<()> {
    let id = id.to_string();
    let mut args = vec!["run", "rerun", &id];
    if failed_only {
        args.push("--failed");
    }
    gh(dir, &args).map(|_| ())
}

/// Every item of a paginated REST list under the current repo (`gh api --paginate --jq
/// '.[]'`: one JSON object per line, whatever the page count).
fn api_items<T: serde::de::DeserializeOwned>(dir: &Path, path: &str) -> Result<Vec<T>> {
    let raw = gh(
        dir,
        &[
            "api",
            "--paginate",
            "--jq",
            ".[]",
            &format!("repos/{{owner}}/{{repo}}/{path}"),
        ],
    )?;
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).map_err(|source| GitError::Json {
                what: format!("gh api {path}"),
                source,
            })
        })
        .collect()
}

/// Reply to an inline review thread (its root comment `root`) on pull request `number`.
pub fn reply_to_thread(dir: &Path, number: u64, root: u64, body: &str) -> Result<()> {
    gh(
        dir,
        &[
            "api",
            "-X",
            "POST",
            &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments/{root}/replies"),
            "-f",
            &format!("body={body}"),
        ],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bin_reaches_past_a_bare_gui_path() {
        let home = std::env::temp_dir().join(format!("kyde-gh-home-{}", std::process::id()));
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("kyde-test-gh"), "").unwrap();
        // The `PATH` a Dock-launched app gets: no Homebrew, no ~/.local/bin.
        let bare = OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        let found = find_bin("kyde-test-gh", Some(bare), Some(home.as_os_str()));
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(found, bin.join("kyde-test-gh"));
    }

    #[test]
    fn a_bots_comment_is_marked_as_one() {
        let entry = |l| serde_json::from_str::<RawIssueComment>(l).unwrap().entry();
        assert_eq!(
            entry(
                r#"{"user":{"login":"github-actions[bot]","type":"Bot"},"body":"b","created_at":"t"}"#
            ),
            ("github-actions".into(), "t".into(), "b".into(), true)
        );
        assert!(!entry(r#"{"user":{"login":"kyle","type":"User"},"body":"b","created_at":"t"}"#).3);
    }

    /// Perf guard (see CLAUDE.md "Performance regression tests"): a failed run's log is
    /// parsed once per selection, and big test suites log hundreds of thousands of lines.
    #[test]
    fn perf_parse_failed_log() {
        let line = "test (ubuntu)\tRun cargo test\t2026-11-04T17:10:16.1234567Z \x1b[32mtest a::b ... ok\x1b[0m\n";
        let log = line.repeat(200_000);
        let start = std::time::Instant::now();
        let rows = parse_failed_log(&log);
        let took = start.elapsed();
        assert_eq!(rows.len(), 200_001);
        assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
    }
}
