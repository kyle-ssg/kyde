//! File operations — new file, rename, delete, scratch files, reveal in Finder/terminal.
//! Crate-root child module.

use crate::*;

impl Kyde {
    pub(crate) fn render_delete_modal(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let Some((path, is_dir)) = self.delete_target.clone() else {
            return div().into_any_element();
        };
        let name = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let kind = if is_dir { "folder" } else { "file" };

        let cancel = btn_secondary("delete-cancel", "Cancel")
            .px_3()
            .py_1p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.delete_target = None;
                    cx.notify();
                }),
            );
        let confirm = div()
            .id("delete-confirm")
            .px_3()
            .py_1p5()
            .rounded_md()
            .bg(t.status_deleted)
            .text_color(t.primary_text)
            .cursor_pointer()
            .hover(|s| s.opacity(0.9))
            .child("Delete")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.do_delete(cx)),
            );

        let panel = div()
            .w(px(420.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(t.frame_bg)
            .border_1()
            .border_color(t.divider)
            .rounded(px(theme::ISLAND_RADIUS))
            .shadow_lg()
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .occlude()
            .child(div().text_color(t.text).child(format!("Delete {kind}?")))
            .child(
                div()
                    .text_color(t.secondary_text)
                    .text_size(px(theme::get().ui_font_size))
                    .child(format!(
                        "“{name}” will be permanently deleted from disk. This can't be undone."
                    )),
            )
            .child(ui::modal_footer().child(cancel).child(confirm));
        overlay(cx, true).child(panel).into_any_element()
    }

    /// New-file / rename modal: a single-line name input + Create/Rename & Cancel.
    /// Enter confirms / Esc cancels via the "`FileFinder`" key context (the input is
    /// single-line, so those keys bubble up to this wrapper).
    pub(crate) fn render_name_prompt(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let Some(prompt) = self.name_prompt.clone() else {
            return div().into_any_element();
        };
        let (title, action) = match &prompt {
            NamePrompt::NewFile(_) => ("New file", "Create"),
            NamePrompt::NewFolder(_) => ("New folder", "Create"),
            NamePrompt::Rename(_) => ("Rename", "Rename"),
        };

        let cancel = btn_secondary("name-cancel", "Cancel")
            .px_3()
            .py_1p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| this.cancel_name_prompt(window, cx)),
            );
        let confirm = btn_primary("name-confirm", action)
            .px_3()
            .py_1p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| this.confirm_name_prompt(window, cx)),
            );

        let panel = div()
            .key_context("FileFinder")
            .on_action(cx.listener(|this, _: &FinderConfirm, window, cx| {
                this.confirm_name_prompt(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FinderClose, window, cx| {
                this.cancel_name_prompt(window, cx);
            }))
            .w(px(420.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(t.frame_bg)
            .border_1()
            .border_color(t.divider)
            .rounded(px(theme::ISLAND_RADIUS))
            .shadow_lg()
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .occlude()
            .child(div().text_color(t.text).child(title))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(t.main_bg)
                    .border_1()
                    .border_color(t.divider)
                    .child(self.name_input.clone()),
            )
            .child(ui::modal_footer().child(cancel).child(confirm));
        overlay(cx, true).child(panel).into_any_element()
    }

    /// Open the "new file" prompt, creating in `dir` (rel path; `""` = repo root).
    pub(crate) fn start_new_file(
        &mut self,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_name_prompt(NamePrompt::NewFile(dir), window, cx);
    }

    /// Open the "new folder" prompt, creating in `dir` (rel path; `""` = repo root).
    pub(crate) fn start_new_folder(
        &mut self,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_name_prompt(NamePrompt::NewFolder(dir), window, cx);
    }

    fn start_name_prompt(
        &mut self,
        prompt: NamePrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.name_prompt = Some(prompt);
        self.name_input.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx);
        });
        let handle = self.name_input.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    /// Open the "rename" prompt for `path` (rel), pre-filled with its current name.
    pub(crate) fn start_rename(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let cur = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.name_prompt = Some(NamePrompt::Rename(path));
        self.name_input
            .update(cx, |e, cx| e.set_content(cur, Lang::PlainText, cx));
        let handle = self.name_input.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    pub(crate) fn cancel_name_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.name_prompt = None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Apply the name prompt: create the new file (and open it) or rename, then
    /// refresh. A blank name just cancels.
    pub(crate) fn confirm_name_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.name_prompt.take() else {
            return;
        };
        let name = self.name_input.read(cx).text().trim().to_string();
        window.focus(&self.focus_handle);
        if name.is_empty() {
            cx.notify();
            return;
        }
        // Pure filesystem ops rooted at the project — they must work in plain
        // (non-git) folders too, so none of them go through `Repo`.
        match prompt {
            NamePrompt::NewFile(dir) => {
                let rel = if dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    dir.join(&name)
                };
                match self.fs_create_file(&rel) {
                    Ok(()) => {
                        self.refresh(cx);
                        self.open_file(rel, cx);
                    }
                    Err(e) => self.fail("Creating file", e),
                }
            }
            NamePrompt::NewFolder(dir) => {
                let rel = if dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    dir.join(&name)
                };
                match self.fs_create_folder(&rel) {
                    Ok(()) => {
                        // Keep the (still empty, so invisible-to-git) folder in the
                        // tree, expanded down to it, and selected.
                        if !self.browse.extra_dirs.contains(&rel) {
                            self.browse.extra_dirs.push(rel.clone());
                        }
                        for a in rel.ancestors().skip(1) {
                            self.browse.expanded.insert(a.to_path_buf());
                        }
                        self.browse.expanded.insert(PathBuf::new());
                        self.browse.selected_path = Some(rel);
                        self.refresh(cx);
                    }
                    Err(e) => self.fail("Creating folder", e),
                }
            }
            NamePrompt::Rename(path) => {
                let dst = path
                    .parent()
                    .map_or_else(|| PathBuf::from(&name), |d| d.join(&name));
                match self.fs_rename(&path, &dst) {
                    Ok(()) => {
                        // Repoint any open tab / selection from the old path to the new one.
                        for t in &mut self.browse.open_tabs {
                            if *t == path {
                                *t = dst.clone();
                            }
                        }
                        let was_open = self.browse.open_path.as_ref() == Some(&path);
                        if self.browse.selected_path.as_ref() == Some(&path) {
                            self.browse.selected_path = Some(dst.clone());
                        }
                        self.refresh(cx);
                        if was_open {
                            self.open_file(dst, cx);
                        }
                    }
                    Err(e) => self.fail("Renaming", e),
                }
            }
        }
        cx.notify();
    }

    /// `rel` resolved against the open project's root.
    fn project_abs(&self, rel: &std::path::Path) -> anyhow::Result<PathBuf> {
        let root = self
            .repo_root
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no project open"))?;
        Ok(root.join(rel))
    }

    /// Create an empty file at `rel`. Refuses to touch an existing path (a "new" file
    /// must never truncate something already there).
    fn fs_create_file(&self, rel: &std::path::Path) -> anyhow::Result<()> {
        let full = self.project_abs(rel)?;
        if full.exists() {
            anyhow::bail!("{} already exists", rel.display());
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, "")?;
        Ok(())
    }

    /// Create the folder `rel` (and any missing parents).
    fn fs_create_folder(&self, rel: &std::path::Path) -> anyhow::Result<()> {
        let full = self.project_abs(rel)?;
        if full.exists() {
            anyhow::bail!("{} already exists", rel.display());
        }
        std::fs::create_dir_all(&full)?;
        Ok(())
    }

    /// Move `from` to `to` within the project. Refuses to clobber an existing target.
    fn fs_rename(&self, from: &std::path::Path, to: &std::path::Path) -> anyhow::Result<()> {
        let (src, dst) = (self.project_abs(from)?, self.project_abs(to)?);
        if dst.exists() {
            anyhow::bail!("{} already exists", to.display());
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&src, &dst)?;
        Ok(())
    }

    pub(crate) fn open_delete(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            self.repo_root
                .as_ref()
                .map_or_else(|| path.clone(), |r| r.join(&path))
        };
        let is_dir = abs.is_dir();
        self.context_menu = None;
        self.delete_target = Some((path, is_dir));
        cx.notify();
    }

    /// Delete the pending file/folder from disk, then refresh the trees.
    pub(crate) fn do_delete(&mut self, cx: &mut Context<Self>) {
        let Some((path, is_dir)) = self.delete_target.take() else {
            return;
        };
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            self.repo_root
                .as_ref()
                .map_or_else(|| path.clone(), |r| r.join(&path))
        };
        // Local history: a deleted file's content is recoverable from its timeline.
        // (Folders are skipped — snapshotting a whole subtree could be huge.)
        if !is_dir {
            self.lh_snapshot_now(vec![path.clone()], "Before delete", cx);
        }
        let r = if is_dir {
            std::fs::remove_dir_all(&abs)
        } else {
            std::fs::remove_file(&abs)
        };
        if let Err(e) = r {
            // The file is still on disk — surface the error and leave tabs/selection
            // pointing at it, rather than silently pretending the delete happened.
            self.fail("Deleting", e);
            cx.notify();
            return;
        }
        // Drop any open tab / selection pointing at the deleted path.
        self.browse.open_tabs.retain(|t| t != &path);
        if self.browse.open_path.as_ref() == Some(&path) {
            self.browse.open_path = self.browse.open_tabs.last().cloned();
        }
        if self.browse.selected_path.as_ref() == Some(&path) {
            self.browse.selected_path = None;
        }
        self.refresh(cx);
        cx.notify();
    }

    /// Reveal a repo-relative path in the OS file manager (macOS Finder via `open -R`).
    pub(crate) fn reveal_in_os(&mut self, rel: &std::path::Path, cx: &mut Context<Self>) {
        if let Some(root) = &self.repo_root {
            let full = root.join(rel);
            std::process::Command::new("open")
                .arg("-R")
                .arg(&full)
                .spawn()
                .ok();
        }
        self.close_menu(cx);
    }

    /// Open the system terminal in the folder containing a repo-relative path
    /// (macOS: `open -a Terminal <dir>`). Files open their parent dir; dirs open
    /// themselves.
    pub(crate) fn reveal_in_terminal(&mut self, rel: &std::path::Path, cx: &mut Context<Self>) {
        if let Some(root) = &self.repo_root {
            let full = root.join(rel);
            let dir = if full.is_dir() {
                full.clone()
            } else {
                full.parent()
                    .map_or_else(|| root.clone(), std::path::Path::to_path_buf)
            };
            std::process::Command::new("open")
                .arg("-a")
                .arg("Terminal")
                .arg(&dir)
                .spawn()
                .ok();
        }
        self.close_menu(cx);
    }

    /// Backspace: delete the selected Browse-tree file/folder — identical to the
    /// right-click "Delete…" menu item (both route through `open_delete`, which pops the
    /// confirm modal). Browse mode only; no-op when nothing is selected.
    pub(crate) fn act_delete_file(
        &mut self,
        _: &DeleteFile,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Browse {
            return;
        }
        if let Some(path) = self.browse.selected_path.clone() {
            self.open_delete(path, cx);
        }
    }

    pub(crate) fn act_new_scratch(
        &mut self,
        _: &NewScratch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_finder(FinderMode::Scratch, window, cx);
    }

    /// Create a scratch file of the given extension and open it.
    pub(crate) fn create_scratch(&mut self, ext: &str, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        match scratch::create(&root, ext) {
            Ok(path) => {
                self.refresh(cx);
                self.mode = Mode::Browse;
                self.open_file(path, cx);
            }
            Err(e) => self.fail("Creating scratch file", e),
        }
        cx.notify();
    }
}
