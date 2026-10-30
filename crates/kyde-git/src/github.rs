//! GitHub issues and pull requests through the `gh` CLI — shelled out like `git`, so it uses
//! the user's existing `gh auth` (no tokens of our own). Network calls: run them off the UI
//! thread. `gh` never prompts here (`GH_PROMPT_DISABLED`), so a missing login fails fast.

use crate::{io_err, GitError, Result};
use serde::Deserialize;
use std::path::Path;
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

/// Run `gh <args>` in `dir`; its stdout, or its stderr as the error.
fn gh(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("gh")
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .map_err(io_err(format!("running gh {args:?}")))?;
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
    /// The conversation: `(author, created at, markdown body)`, oldest first.
    pub comments: Vec<(String, String, String)>,
    /// Assignees' logins.
    pub assignees: Vec<String>,
    /// When it was opened (ISO 8601).
    pub created_at: String,
}

/// What a pull request changes.
#[derive(Debug, Clone, PartialEq, Eq)]
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
struct RawComment {
    author: Option<RawAuthor>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
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
    comments: Vec<RawComment>,
}

/// Issue `number` in full (`gh issue view`).
pub fn issue_detail(dir: &Path, number: u64) -> Result<ItemDetail> {
    let raw = gh(
        dir,
        &[
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "number,title,body,author,state,url,labels,comments,assignees,createdAt",
        ],
    )?;
    parse_detail(&raw, "gh issue view")
}

/// Pull request `number` in full (`gh pr view`), incl. its branches and changed files.
pub fn pr_detail(dir: &Path, number: u64) -> Result<ItemDetail> {
    let raw = gh(
        dir,
        &[
            "pr", "view", &number.to_string(), "--json",
            "number,title,body,author,state,url,labels,headRefName,baseRefName,additions,deletions,isDraft,files,comments,assignees,createdAt,commits,statusCheckRollup,reviewRequests",
        ],
    )?;
    parse_detail(&raw, "gh pr view")
}

/// Parse `gh issue view` / `gh pr view --json …` (a PR has `headRefName`).
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
        }),
        comments: r
            .comments
            .into_iter()
            .map(|c| {
                (
                    c.author.map(|a| a.login).unwrap_or_default(),
                    c.created_at,
                    c.body,
                )
            })
            .collect(),
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
    /// `(author, at, markdown body)`, oldest first.
    pub comments: Vec<(String, String, String)>,
}

#[derive(Deserialize)]
struct RawUser {
    login: String,
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
    user: Option<RawUser>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
}

/// Pull request `number`'s reviews and inline review threads (`gh api …/pulls/N/reviews`
/// and `…/comments`), oldest first.
pub fn reviews(dir: &Path, number: u64) -> Result<(Vec<Review>, Vec<ReviewThread>)> {
    let reviews: Vec<RawReview> = api_items(dir, &format!("pulls/{number}/reviews"))?;
    let comments: Vec<RawReviewComment> = api_items(dir, &format!("pulls/{number}/comments"))?;
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
                comments: vec![entry],
            }),
        }
    }
    Ok((reviews, threads))
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
