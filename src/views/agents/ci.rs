//! The Agents sidebar's CI tab: each project's latest GitHub Actions runs, filtered to the
//! ones in progress / failed / completed, with a one-click retry on a failed one — and, in
//! the main panel, the selected run's jobs, GitHub's reasons for the failed ones and their
//! failed steps' log. Everything goes through `gh` off the UI thread (`kyde_git::github`).

use super::*;
use kyde_git::github::{LogLevel, LogRow, RunState, WorkflowRun};

/// How often the CI lists re-fetch while a run is under way (vs `GH_EVERY_TICKS`).
#[cfg(feature = "terminal")]
pub(super) const CI_EVERY_TICKS: u32 = 15;

/// The CI filter dropdown: its labels, and the runs each shows (`None` = all).
pub(super) const CI_FILTER_LABELS: [&str; 4] = ["All", "In progress", "Failed", "Completed"];
const CI_FILTERS: [Option<RunState>; 4] = [
    None,
    Some(RunState::Running),
    Some(RunState::Failed),
    Some(RunState::Passed),
];

/// The dropdown position of `filter`.
pub(super) fn filter_index(filter: Option<RunState>) -> usize {
    CI_FILTERS.iter().position(|f| *f == filter).unwrap_or(0)
}

/// The filter at dropdown position `i`.
pub(super) fn filter_at(i: usize) -> Option<RunState> {
    CI_FILTERS.get(i).copied().flatten()
}

/// A run's (or job's) status glyph and colour — the PR Checks tab's.
fn state_glyph(state: RunState) -> (&'static str, kyde_color::Color) {
    match state {
        RunState::Running => CheckOutcome::Pending,
        RunState::Failed => CheckOutcome::Fail,
        RunState::Passed => CheckOutcome::Pass,
    }
    .glyph()
}

/// What a run's status pill says.
fn run_label(status: &str, conclusion: &str) -> &'static str {
    match RunState::of(status, conclusion) {
        RunState::Running => match status {
            "queued" | "requested" | "pending" => "Queued",
            "waiting" => "Waiting",
            _ => "In progress",
        },
        _ => match conclusion {
            "success" => "Succeeded",
            "skipped" => "Skipped",
            "neutral" => "Neutral",
            "cancelled" => "Cancelled",
            "timed_out" => "Timed out",
            "startup_failure" => "Startup failure",
            "action_required" => "Action required",
            _ => "Failed",
        },
    }
}

/// "45s" / "3m 12s" / "1h 4m" for a span of seconds.
fn span(secs: i64) -> String {
    match secs.max(0) {
        s @ 0..60 => format!("{s}s"),
        s @ 60..3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// Seconds since the Unix epoch, now.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl Kyde {
    /// (Re)load `space`'s latest runs with `gh`, in the background. `quiet`: keep showing the
    /// current list meanwhile, and keep it if the fetch fails.
    pub(super) fn agents_fetch_runs(
        &mut self,
        space: PathBuf,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        let shown = matches!(self.agents.ci.runs.get(&space), Some(GhList::Ready(_)));
        if !(quiet && shown) {
            self.agents.ci.runs.insert(space.clone(), GhList::Loading);
        }
        cx.spawn(async move |this, cx| {
            let dir = space.clone();
            let r = cx
                .background_executor()
                .spawn(async move { kyde_git::github::runs(&dir) })
                .await;
            this.update(cx, |this, cx| {
                if quiet && r.is_err() && shown {
                    return;
                }
                let list = r.map_or_else(
                    |e| GhList::Failed(crate::agents::git_reason(&e.to_string()).to_string()),
                    GhList::Ready,
                );
                this.agents.ci.runs.insert(space, list);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether the list shows `run`: it passes the filter and matches the search (`query`,
    /// lowercased) on its title, workflow or branch.
    fn ci_shows(&self, run: &WorkflowRun, query: &str) -> bool {
        self.agents.ci.filter.is_none_or(|f| run.state() == f)
            && (query.is_empty()
                || [&run.title, &run.workflow, &run.branch]
                    .iter()
                    .any(|s| s.to_lowercase().contains(query)))
    }

    /// `space`'s runs as the tree counts them: (shown, still loading, why it failed).
    pub(super) fn agents_ci_status(
        &self,
        space: &Path,
        query: &str,
    ) -> (usize, bool, Option<&str>) {
        match self.agents.ci.runs.get(space) {
            Some(GhList::Ready(v)) => (
                v.iter().filter(|r| self.ci_shows(r, query)).count(),
                false,
                None,
            ),
            other => gh_status(other),
        }
    }

    /// `space`'s listed run `id` — the list knows whether a newer run has superseded it.
    fn ci_listed(&self, space: &Path, id: u64) -> Option<&WorkflowRun> {
        match self.agents.ci.runs.get(space) {
            Some(GhList::Ready(v)) => v.iter().find(|r| r.id == id),
            _ => None,
        }
    }

    /// How many of `space`'s runs are under way (the CI tab's badge).
    pub(super) fn agents_ci_running(&self, space: &Path) -> usize {
        match self.agents.ci.runs.get(space) {
            Some(GhList::Ready(v)) => v.iter().filter(|r| r.state() == RunState::Running).count(),
            _ => 0,
        }
    }

    /// A run is under way somewhere — the CI tab then refreshes faster.
    #[cfg(feature = "terminal")]
    pub(super) fn agents_ci_busy(&self) -> bool {
        self.agents
            .cfg
            .spaces
            .iter()
            .any(|s| self.agents_ci_running(&s.path) > 0)
    }

    /// The CI tab's rows for `space`: its runs the filter + search let through, newest first
    /// — status glyph, title over workflow · branch · age, and Retry on a failed one. A run a
    /// newer one of its workflow + branch has superseded is dimmed, with no Retry: re-running
    /// an old deploy would roll the newer one back.
    pub(super) fn agents_ci_rows(
        &self,
        si: usize,
        space: &Path,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let Some(GhList::Ready(runs)) = self.agents.ci.runs.get(space) else {
            return Vec::new();
        };
        let fs = theme::get().ui_font_size;
        let shown: Vec<&WorkflowRun> = runs.iter().filter(|r| self.ci_shows(r, query)).collect();
        let len = shown.len();
        shown
            .into_iter()
            .enumerate()
            .map(|(i, run)| {
                let state = run.state();
                let (glyph, color) = state_glyph(state);
                let lead = div()
                    .flex_none()
                    .text_size(px(fs - 1.0))
                    .text_color(color)
                    .child(glyph)
                    .into_any_element();
                let mut sub = format!("{} #{} · {}", run.workflow, run.number, run.branch);
                if state == RunState::Running {
                    sub.push_str(" · ");
                    sub.push_str(&run_label(&run.status, &run.conclusion).to_lowercase());
                } else {
                    let ago = crate::agents::iso_ago(&run.created_at);
                    if !ago.is_empty() {
                        sub.push_str(" · ");
                        sub.push_str(&ago);
                    }
                }
                if let Some(newer) = run.superseded_by {
                    sub.push_str(" · superseded");
                    if let Some(n) = self.ci_listed(space, newer) {
                        sub.push_str(&format!(" by #{}", n.number));
                    }
                }
                let action = match state {
                    _ if run.superseded_by.is_some() => RowAct::Hidden,
                    RunState::Failed if self.agents.ci.rerunning.contains(&run.id) => {
                        RowAct::Busy("Retrying…")
                    }
                    RunState::Failed => RowAct::Go("Retry"),
                    _ => RowAct::Hidden,
                };
                let (sp, id) = (space.to_path_buf(), run.id);
                let sel_space = sp.clone();
                let row = self.agents_gh_row(
                    ("agents-run", si * 10_000 + i),
                    i + 1 == len,
                    self.agents.gh_selected.as_ref()
                        == Some(&(space.to_path_buf(), SideTab::Ci, id)),
                    Some(lead),
                    run.title.clone(),
                    sub,
                    action,
                    move |this, cx| this.agents_ci_rerun(sp.clone(), id, true, cx),
                    move |this, cx| this.agents_select_run(sel_space.clone(), id, cx),
                    cx,
                );
                if run.superseded_by.is_some() {
                    div().opacity(0.5).child(row).into_any_element()
                } else {
                    row
                }
            })
            .collect()
    }

    /// Show run `id` of `space` in the main panel, loading its jobs (and, once it has failed,
    /// its failed steps' log) with `gh`.
    pub(super) fn agents_select_run(&mut self, space: PathBuf, id: u64, cx: &mut Context<Self>) {
        let key = (space, SideTab::Ci, id);
        if self.agents.gh_selected.as_ref() == Some(&key) {
            return;
        }
        self.agents.gh_selected = Some(key);
        self.agents.ci.detail = None;
        self.agents.ci.log = None;
        self.agents_fetch_run(false, cx);
        cx.notify();
    }

    /// Reload the selected run — when `force`d (the Refresh button), or while it's under way.
    pub(super) fn agents_reload_run(&mut self, force: bool, cx: &mut Context<Self>) {
        let running = matches!(
            &self.agents.ci.detail,
            Some(Ok(d)) if d.run.state() == RunState::Running
        );
        if force || running {
            self.agents_fetch_run(true, cx);
        }
    }

    /// (Re)load the selected run's jobs in the background — `quiet`: keep the current ones up
    /// meanwhile, and if it fails. A failed run's log follows once it's in.
    fn agents_fetch_run(&mut self, quiet: bool, cx: &mut Context<Self>) {
        let Some(key @ (_, SideTab::Ci, _)) = self.agents.gh_selected.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (dir, id) = (key.0.clone(), key.2);
            let r = cx
                .background_executor()
                .spawn(async move { kyde_git::github::run_detail(&dir, id) })
                .await;
            this.update(cx, |this, cx| {
                // A newer pick supersedes this one.
                if this.agents.gh_selected.as_ref() != Some(&key) {
                    return;
                }
                match r {
                    Ok(d) => {
                        // The list's copy follows (a re-run queued, a run finished).
                        if let Some(GhList::Ready(v)) = this.agents.ci.runs.get_mut(&key.0) {
                            if let Some(listed) = v.iter_mut().find(|r| r.id == d.run.id) {
                                *listed = WorkflowRun {
                                    superseded_by: listed.superseded_by,
                                    ..d.run.clone()
                                };
                            }
                        }
                        // No jobs = it never started (a workflow file problem): no log.
                        let wants_log = d.run.state() == RunState::Failed
                            && !d.jobs.is_empty()
                            && this.agents.ci.log.is_none();
                        this.agents.ci.detail = Some(Ok(d));
                        if wants_log {
                            this.agents_fetch_run_log(key.0.clone(), key.2, cx);
                        }
                    }
                    Err(_) if quiet && matches!(this.agents.ci.detail, Some(Ok(_))) => {}
                    Err(e) => {
                        this.agents.ci.detail =
                            Some(Err(crate::agents::git_reason(&e.to_string()).to_string()));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Load run `id`'s failure log, and open it at the first step the runner failed, with the
    /// output that led there above it (not the runner's setup at the top).
    fn agents_fetch_run_log(&mut self, space: PathBuf, id: u64, cx: &mut Context<Self>) {
        let key = (space.clone(), SideTab::Ci, id);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { kyde_git::github::run_failed_log(&space, id) })
                .await;
            this.update(cx, |this, cx| {
                if this.agents.gh_selected.as_ref() != Some(&key) {
                    return;
                }
                let log = r
                    .map(std::sync::Arc::new)
                    .map_err(|e| crate::agents::git_reason(&e.to_string()).to_string());
                if let Ok(rows) = &log {
                    let failure = rows.iter().position(|r| {
                        matches!(
                            r,
                            LogRow::Line {
                                level: LogLevel::Failure,
                                ..
                            }
                        )
                    });
                    if let Some(i) = failure {
                        this.agents
                            .ci
                            .log_scroll
                            .scroll_to_item(i, gpui::ScrollStrategy::Bottom);
                    }
                }
                this.agents.ci.log = Some(log);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-run run `id` of `space` (only its failed jobs, or all of them): it shows as queued
    /// at once, and its new attempt once GitHub has it.
    pub(super) fn agents_ci_rerun(
        &mut self,
        space: PathBuf,
        id: u64,
        failed_only: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.agents.ci.rerunning.insert(id) {
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dir = space.clone();
            let r = cx
                .background_executor()
                .spawn(async move { kyde_git::github::rerun(&dir, id, failed_only) })
                .await;
            this.update(cx, |this, cx| {
                this.agents.ci.rerunning.remove(&id);
                if let Err(e) = r {
                    this.agents.note = Some(format!(
                        "Couldn't retry the run: {}",
                        crate::agents::git_reason(&e.to_string())
                    ));
                    cx.notify();
                    return;
                }
                let queue = |r: &mut WorkflowRun| {
                    r.status = "queued".into();
                    r.conclusion.clear();
                };
                if let Some(GhList::Ready(v)) = this.agents.ci.runs.get_mut(&space) {
                    v.iter_mut().filter(|r| r.id == id).for_each(queue);
                }
                if this.agents.gh_selected.as_ref() == Some(&(space.clone(), SideTab::Ci, id)) {
                    if let Some(Ok(d)) = &mut this.agents.ci.detail {
                        queue(&mut d.run);
                    }
                    // The new attempt logs afresh.
                    this.agents.ci.log = None;
                }
                cx.notify();
                // GitHub takes a moment to list the new attempt.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    this.update(cx, |this, cx| {
                        this.agents_fetch_runs(space, true, cx);
                        this.agents_reload_run(false, cx);
                    })
                    .ok();
                })
                .detach();
            })
            .ok();
        })
        .detach();
    }

    /// The CI tab's main panel: the run's title, a status pill and what started it, Retry /
    /// Re-run / Open on GitHub — then its jobs (failed ones with their failed steps and
    /// GitHub's reasons) and, for a failed run, its failed steps' log. A superseded run says
    /// so and links to the latest one instead of offering a re-run.
    pub(super) fn render_agents_ci_detail(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        let Some((space, _, id)) = self
            .agents
            .gh_selected
            .clone()
            .filter(|(_, k, _)| *k == SideTab::Ci)
        else {
            return ui::empty_state("Select a workflow run", ui).into_any_element();
        };
        let d = match &self.agents.ci.detail {
            None => return ui::empty_state("Loading…", ui).into_any_element(),
            Some(Err(why)) => return ui::empty_state(why.clone(), ui).into_any_element(),
            Some(Ok(d)) => d,
        };
        let run = &d.run;
        let state = run.state();
        let retrying = self.agents.ci.rerunning.contains(&id);
        // Superseded: (the latest run's id, its number when listed).
        let newer = self
            .ci_listed(&space, id)
            .and_then(|r| r.superseded_by)
            .map(|n| (n, self.ci_listed(&space, n).map(|r| r.number)));
        let pill_color = match state {
            RunState::Running => t.status_modified,
            RunState::Failed => t.status_conflict,
            RunState::Passed => t.status_added,
        };
        let pill = div()
            .flex_none()
            .px_2p5()
            .py_0p5()
            .rounded_full()
            .bg(pill_color)
            .text_color(t.primary_text)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(run_label(&run.status, &run.conclusion));
        let mut what = format!(
            "{} #{} · {} on {} · {}",
            run.workflow,
            run.number,
            run.event,
            run.branch,
            crate::agents::iso_ago(&run.created_at)
        );
        if run.attempt > 1 {
            what.push_str(&format!(" · attempt {}", run.attempt));
        }
        let mut actions: Vec<gpui::AnyElement> = Vec::new();
        if let Some((latest, number)) = newer {
            let sp = space.clone();
            let label = number.map_or_else(
                || "View the latest run".to_string(),
                |n| format!("View the latest run #{n}"),
            );
            actions.push(
                btn_primary("agents-ci-latest", label)
                    .debug_selector(|| "agents-ci-latest".into())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_select_run(sp.clone(), latest, cx);
                        }),
                    )
                    .into_any_element(),
            );
        } else if state == RunState::Failed {
            let sp = space.clone();
            actions.push(if retrying {
                btn_primary_state("agents-ci-retry", "Retrying…", true).into_any_element()
            } else {
                btn_primary("agents-ci-retry", "Retry failed jobs")
                    .debug_selector(|| "agents-ci-retry".into())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_ci_rerun(sp.clone(), id, true, cx);
                        }),
                    )
                    .into_any_element()
            });
        }
        if state != RunState::Running && newer.is_none() {
            let sp = space.clone();
            actions.push(
                btn_secondary_state("agents-ci-rerun-all", "Re-run all jobs", retrying)
                    .when(!retrying, |b| {
                        b.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.agents_ci_rerun(sp.clone(), id, false, cx);
                            }),
                        )
                    })
                    .into_any_element(),
            );
        }
        let url = run.url.clone();
        actions.push(
            btn_secondary("agents-ci-web", "Open on GitHub")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_this, _e, _w, cx| cx.open_url(&url)),
                )
                .into_any_element(),
        );
        let header = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .flex_none()
            .border_b_1()
            .border_color(t.divider)
            .child(
                div()
                    .text_size(px(fs + 7.0))
                    .child(SharedString::from(run.title.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(pill)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(t.secondary_text)
                            .child(SharedString::from(what)),
                    ),
            )
            .when(newer.is_some(), |d| {
                d.child(
                    div()
                        .text_size(px(fs - 1.0))
                        .text_color(t.secondary_text)
                        .child(SharedString::from(format!(
                            "Superseded by a newer {} run on {} — re-running this one could undo \
                             it (an old deploy would roll back the new one).",
                            run.workflow, run.branch
                        ))),
                )
            })
            .child(div().flex().flex_row().gap_2().pt_1().children(actions));
        // The log takes the room when there is one; the jobs scroll above it.
        let has_log = state == RunState::Failed && !d.jobs.is_empty();
        let jobs = div()
            .id("agents-ci-jobs")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .when(has_log, |d| d.flex_none().max_h(px(240.0)))
            .when(!has_log, |d| d.flex_1().min_h_0())
            .children(self.ci_job_rows(d, cx));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .font_family(ui)
            .text_size(px(fs))
            .text_color(t.text)
            .child(header)
            .child(jobs)
            .when(has_log, |d| {
                d.child(self.ci_log_header(cx)).child(self.render_ci_log())
            })
            .into_any_element()
    }

    /// One row per job — its glyph, name and time (or the step it's on) — with a failed
    /// job's failed steps and GitHub's reasons beneath. A click opens the job on GitHub.
    fn ci_job_rows(
        &self,
        d: &kyde_git::github::RunDetail,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let t = theme::get();
        let fs = t.ui_font_size;
        if d.jobs.is_empty() {
            return vec![gh_note(match d.run.state() {
                RunState::Failed => {
                    "No job ran — this run likely failed because of a problem in its workflow file."
                }
                RunState::Running => "Waiting for its jobs to start…",
                RunState::Passed => "No jobs ran.",
            })];
        }
        let now = now_secs();
        d.jobs
            .iter()
            .enumerate()
            .map(|(i, job)| {
                let state = job.state();
                let (glyph, color) = state_glyph(state);
                let secs = |iso: &str| crate::agents::iso_secs(iso);
                let time = match (secs(&job.started_at), secs(&job.completed_at)) {
                    (Some(a), Some(b)) if state != RunState::Running => span(b - a),
                    (Some(a), _) => {
                        // Under way: the step it's on, and for how long.
                        let step = job
                            .steps
                            .iter()
                            .find(|(_, s)| *s == RunState::Running)
                            .map(|(n, _)| format!("{n} · "))
                            .unwrap_or_default();
                        format!("{step}{}", span(now - a))
                    }
                    _ => run_label(&job.status, &job.conclusion).to_lowercase(),
                };
                let failed_steps: Vec<&str> = job
                    .steps
                    .iter()
                    .filter(|(_, s)| *s == RunState::Failed)
                    .map(|(n, _)| n.as_str())
                    .collect();
                let url = job.url.clone();
                div()
                    .id(("agents-ci-job", i))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(t.divider)
                    .cursor_pointer()
                    .hover(|d| d.bg(t.bg_mid))
                    .tooltip(ui::tip("Open this job on GitHub"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _e, _w, cx| {
                            if !url.is_empty() {
                                cx.open_url(&url);
                            }
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(div().flex_none().text_color(color).child(glyph))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(job.name.clone())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(fs - 1.0))
                                    .text_color(t.line_number)
                                    .child(SharedString::from(time)),
                            ),
                    )
                    .children(failed_steps.into_iter().map(|step| {
                        div()
                            .pl(px(20.0))
                            .text_size(px(fs - 1.0))
                            .text_color(t.secondary_text)
                            .child(SharedString::from(format!("✗ {step}")))
                    }))
                    .children(job.errors.iter().map(|a| {
                        let at = match (a.path.as_str(), a.line) {
                            ("", _) => String::new(),
                            (p, Some(l)) => format!("{p}:{l} — "),
                            (p, None) => format!("{p} — "),
                        };
                        div()
                            .ml(px(20.0))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(t.diff_conflict_bg)
                            .text_size(px(fs - 1.0))
                            .text_color(t.text)
                            .child(SharedString::from(format!("{at}{}", a.message)))
                    }))
                    .into_any_element()
            })
            .collect()
    }

    /// The bar over the log: what it is, and Copy log.
    fn ci_log_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let rows = match &self.agents.ci.log {
            Some(Ok(rows)) if !rows.is_empty() => Some(rows.clone()),
            _ => None,
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_1p5()
            .flex_none()
            .border_b_1()
            .border_color(t.divider)
            .bg(t.bg_mid)
            .child(
                div()
                    .flex_1()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Failure log"),
            )
            .when_some(rows, |d, rows| {
                d.child(
                    btn_secondary("agents-ci-copy-log", "Copy log")
                        .px_2()
                        .py_0()
                        .text_size(px(t.ui_font_size - 2.0))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _e, _w, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(log_text(
                                    &rows,
                                )));
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    /// The failed steps' output, line by line: a header where each step starts, the runner's
    /// failures on red, error lines in red, warnings brighter than the rest.
    fn render_ci_log(&self) -> gpui::AnyElement {
        let t = theme::get();
        let rows = match &self.agents.ci.log {
            None => return gh_note("Loading the log…"),
            Some(Err(why)) => return gh_note(why.clone()),
            Some(Ok(rows)) => rows.clone(),
        };
        if rows.is_empty() {
            return gh_note(
                "The failed steps logged nothing — the reasons above are all GitHub has.",
            );
        }
        let count = rows.len();
        let fs = t.ui_font_size - 1.0;
        gpui::uniform_list("agents-ci-log", count, move |range, _window, _cx| {
            let t = theme::get();
            range
                .map(|i| {
                    let row = div()
                        .w_full()
                        .h(px(fs * 1.6))
                        .flex()
                        .flex_row()
                        .items_center()
                        .px_4()
                        .text_size(px(fs))
                        .font_family(theme::font::family());
                    match &rows[i] {
                        LogRow::Step { job, step } => row
                            .bg(t.bg_mid)
                            .font_family(theme::font::ui_family())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(div().truncate().child(SharedString::from(
                                if step.is_empty() {
                                    job.clone()
                                } else {
                                    format!("{job} › {step}")
                                },
                            ))),
                        LogRow::Line { text, level } => {
                            let text = SharedString::from(text.replace('\t', "    "));
                            match level {
                                LogLevel::Failure => row
                                    .bg(t.diff_conflict_bg)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(div().truncate().child(text)),
                                LogLevel::Error => row
                                    .text_color(t.status_conflict)
                                    .child(div().truncate().child(text)),
                                LogLevel::Warning => {
                                    row.text_color(t.text).child(div().truncate().child(text))
                                }
                                LogLevel::Group => row.text_color(t.line_number).child(
                                    div()
                                        .truncate()
                                        .child(SharedString::from(format!("▸ {text}"))),
                                ),
                                LogLevel::Plain => row
                                    .text_color(t.secondary_text)
                                    .child(div().truncate().child(text)),
                            }
                        }
                    }
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(self.agents.ci.log_scroll.clone())
        .flex_1()
        .min_h_0()
        .into_any_element()
    }
}

/// The log as plain text, for the clipboard (each step under a `── job › step` line).
fn log_text(rows: &[LogRow]) -> String {
    let mut out = String::new();
    for r in rows {
        match r {
            LogRow::Step { job, step } if step.is_empty() => out.push_str(&format!("── {job}\n")),
            LogRow::Step { job, step } => out.push_str(&format!("── {job} › {step}\n")),
            LogRow::Line { text, .. } => {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out
}
