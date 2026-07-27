//! File operations — new file, rename, delete, scratch files, reveal in Finder/terminal.
//! Crate-root child module.

use crate::*;

/// If `p` is `old` or lives under it, return `p` with the `old` prefix replaced by `new`
/// (so renaming a folder repoints every path beneath it); `None` when `p` is unrelated.
pub(crate) fn remap_under(
    p: &std::path::Path,
    old: &std::path::Path,
    new: &std::path::Path,
) -> Option<PathBuf> {
    if p == old {
        return Some(new.to_path_buf());
    }
    p.strip_prefix(old).ok().map(|rest| new.join(rest))
}

/// Recursively copy `src` → `dst` (a single file, or a whole directory tree). Parents of
/// `dst` are created as needed. Used by paste/duplicate (issue #67).
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> anyhow::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Split a filename into `(stem, ext_with_dot)` at the LAST dot — but only when the dot
/// isn't the leading char (so a dotfile like `.gitignore` keeps its whole name). Pure.
fn split_name(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// File extension for a clipboard image's format (issue #67 — pasted image → a file).
fn image_ext(format: gpui::ImageFormat) -> &'static str {
    match format {
        gpui::ImageFormat::Png => "png",
        gpui::ImageFormat::Jpeg => "jpg",
        gpui::ImageFormat::Gif => "gif",
        gpui::ImageFormat::Webp => "webp",
        gpui::ImageFormat::Bmp => "bmp",
        gpui::ImageFormat::Tiff => "tiff",
        gpui::ImageFormat::Svg => "svg",
    }
}

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

        let panel = ui::modal_panel(420.0, ui)
            .child(ui::dialog_title(format!("Delete {kind}?")))
            .child(ui::dialog_body(format!(
                "“{name}” will be permanently deleted from disk. This can't be undone."
            )))
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

        let panel = ui::modal_panel(420.0, ui)
            .key_context("FileFinder")
            .on_action(cx.listener(|this, _: &FinderConfirm, window, cx| {
                this.confirm_name_prompt(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FinderClose, window, cx| {
                this.cancel_name_prompt(window, cx);
            }))
            .child(ui::dialog_title(title))
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
                        // Stamp the creation in local history (inline, before the
                        // open records a bare baseline) — the timeline row reads
                        // "Created" and the pre-creation state is one revert away.
                        self.lh_label_sync(std::slice::from_ref(&rel), "Created");
                        self.refresh(cx);
                        self.open_file(rel, cx);
                        // Focus the editor so the user can type into the new file at once.
                        self.focus_editor(window, cx);
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
                self.move_path(&path, &dst, "Renaming", cx);
            }
        }
        cx.notify();
    }

    /// `rel` resolved against the open project's root.
    pub(crate) fn project_abs(&self, rel: &std::path::Path) -> anyhow::Result<PathBuf> {
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

    /// Move/rename `from` → `to` on disk, then repoint every piece of Browse UI state that
    /// pointed at or under `from` — open tabs, the active/selected path, expansion, and
    /// `extra_dirs` — so a folder move carries its whole subtree. Reopens the active file at
    /// its new path. No-op when `to == from`. `ctx` labels any error banner.
    pub(crate) fn move_path(
        &mut self,
        from: &std::path::Path,
        to: &std::path::Path,
        ctx: &str,
        cx: &mut Context<Self>,
    ) {
        if to == from {
            cx.notify();
            return;
        }
        if let Err(e) = self.fs_rename(from, to) {
            self.fail(ctx, e);
            cx.notify();
            return;
        }
        // Log it in local history so a rename/move reads clearly (issue #67 QA): old path
        // tombstoned, new path labeled. Must run AFTER the fs move (reads the new location).
        let lh_label = if ctx == "Renaming" {
            "Renamed"
        } else {
            "Moved here"
        };
        self.lh_record_rename(from, to, lh_label, cx);
        for t in &mut self.browse.open_tabs {
            if let Some(n) = remap_under(t, from, to) {
                *t = n;
            }
        }
        let reopen = self
            .browse
            .open_path
            .as_ref()
            .and_then(|o| remap_under(o, from, to));
        if let Some(sel) = &self.browse.selected_path {
            if let Some(n) = remap_under(sel, from, to) {
                self.browse.selected_path = Some(n);
            }
        }
        self.browse.expanded = std::mem::take(&mut self.browse.expanded)
            .into_iter()
            .map(|e| remap_under(&e, from, to).unwrap_or(e))
            .collect();
        for d in &mut self.browse.extra_dirs {
            if let Some(n) = remap_under(d, from, to) {
                *d = n;
            }
        }
        self.refresh(cx);
        if let Some(o) = reopen {
            self.open_file(o, cx);
        }
        cx.notify();
    }

    /// Drop the dragged path `src` into the directory `dest_dir` (rel; `""` = repo root) —
    /// the tree drag & drop (issue #65). Ignores no-op drops (already in that folder) and
    /// refuses to move a folder into itself or one of its own descendants.
    pub(crate) fn drop_onto_dir(
        &mut self,
        src: PathBuf,
        dest_dir: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = src.file_name() else {
            return;
        };
        // Already living directly in the destination → nothing to do.
        let src_parent = src
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        if src_parent == dest_dir {
            return;
        }
        // Can't drop a folder onto itself or into its own subtree.
        if dest_dir == src || dest_dir.starts_with(&src) {
            return;
        }
        let to = dest_dir.join(name);
        // Reveal the moved item: make sure its new home is expanded.
        self.browse.expanded.insert(dest_dir);
        self.move_path(&src, &to, "Moving", cx);
    }

    // ── Cut / Copy / Paste (issue #67) ────────────────────────────────────────────────

    /// The tree paths a clipboard/keyboard op should act on: the cmd-click multi-selection
    /// if any, else the single selected row. Scratch + absolute + the repo-root row are
    /// dropped (they aren't movable project files).
    pub(crate) fn tree_selection(&self) -> Vec<PathBuf> {
        let raw = if self.browse.multi_selected.is_empty() {
            self.browse.selected_path.clone().into_iter().collect()
        } else {
            self.browse.multi_selected.clone()
        };
        raw.into_iter()
            .filter(|p| !p.as_os_str().is_empty() && !p.is_absolute())
            .collect()
    }

    /// Put `paths` on the tree clipboard (issue #67). `cut` → a later paste moves them;
    /// otherwise it copies. Empty selection is a no-op.
    pub(crate) fn clip_paths(&mut self, paths: Vec<PathBuf>, cut: bool, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| !p.as_os_str().is_empty() && !p.is_absolute())
            .collect();
        if paths.is_empty() {
            self.close_menu(cx);
            return;
        }
        self.browse.clipboard = Some(TreeClip { paths, cut });
        self.close_menu(cx);
        cx.notify();
    }

    /// Paste into the directory `dest_dir` (rel; `""` = repo root). Priority: the internal
    /// tree clipboard (cut = move, copy = duplicate), then files copied in Finder, then
    /// image data on the clipboard (written as `image.<ext>` in the folder). Issue #67.
    pub(crate) fn paste_into(&mut self, dest_dir: PathBuf, cx: &mut Context<Self>) {
        self.close_menu(cx);
        // Normalize the target to a real directory (a file → its parent), creating it if
        // needed — so a write never hits "not a directory" (issue #67 QA).
        let dest_dir = self.ensure_dest_dir(dest_dir);
        // 1) Internal clipboard — kyde's own cut/copy.
        if let Some(clip) = self.browse.clipboard.clone() {
            let mut last = None;
            for src in &clip.paths {
                let Some(name) = src.file_name() else {
                    continue;
                };
                if clip.cut {
                    // A move: skip a no-op (already here) or a move into its own subtree.
                    let parent = src
                        .parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default();
                    if parent == dest_dir || dest_dir == *src || dest_dir.starts_with(src) {
                        continue;
                    }
                    let to = dest_dir.join(name);
                    self.move_path(src, &to, "Moving", cx);
                    last = Some(to);
                } else {
                    let to = self.unique_dest(&dest_dir, name);
                    if let Err(e) = self.copy_path(src, &to) {
                        self.fail("Pasting", e);
                    } else {
                        last = Some(to);
                    }
                }
            }
            if clip.cut {
                self.browse.clipboard = None;
            }
            self.finish_paste(dest_dir, last, cx);
            return;
        }
        // 2) Files copied in Finder (absolute source paths).
        let files = os_clipboard::file_paths();
        if !files.is_empty() {
            let mut last = None;
            for src_abs in &files {
                if let Some(to) = self.copy_external_into(src_abs, &dest_dir) {
                    last = Some(to);
                }
            }
            self.finish_paste(dest_dir, last, cx);
            return;
        }
        // 3) Raw image data on the clipboard → a new image file in the folder.
        if let Some(item) = cx.read_from_clipboard() {
            for entry in item.entries() {
                if let gpui::ClipboardEntry::Image(img) = entry {
                    let name = format!("image.{}", image_ext(img.format));
                    let to = self.unique_dest(&dest_dir, std::ffi::OsStr::new(&name));
                    match self.project_abs(&to) {
                        Ok(abs) => {
                            if let Some(p) = abs.parent() {
                                let _ = std::fs::create_dir_all(p);
                            }
                            if let Err(e) = std::fs::write(&abs, &img.bytes) {
                                self.fail("Pasting image", e);
                            } else {
                                self.finish_paste(dest_dir, Some(to), cx);
                                return;
                            }
                        }
                        Err(e) => self.fail("Pasting image", e),
                    }
                }
            }
        }
    }

    /// Files dragged in from Finder (issue #67): copy each into `dest_dir` (rel; `""` = repo
    /// root). Dropping onto a file row targets its folder (the row wiring passes the parent).
    /// `paths` are absolute OS paths.
    pub(crate) fn drop_external_paths(
        &mut self,
        paths: Vec<PathBuf>,
        dest_dir: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let dest = self.ensure_dest_dir(dest_dir);
        let mut last = None;
        for src in &paths {
            if let Some(to) = self.copy_external_into(src, &dest) {
                last = Some(to);
            }
        }
        self.finish_paste(dest, last, cx);
    }

    /// Resolve a paste destination to a real, existing directory (rel; `""` = repo root).
    /// A file path becomes its parent folder; a missing directory is created. Guarantees the
    /// caller can write `<dest>/<name>` without hitting a "not a directory" error.
    fn ensure_dest_dir(&self, dest_dir: PathBuf) -> PathBuf {
        let Some(root) = self.repo_root.clone() else {
            return dest_dir;
        };
        let rel = if root.join(&dest_dir).is_file() {
            dest_dir
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default()
        } else {
            dest_dir
        };
        let _ = std::fs::create_dir_all(root.join(&rel));
        rel
    }

    /// Shared tail of a paste: reveal the destination, select the last pasted item, refresh.
    fn finish_paste(&mut self, dest_dir: PathBuf, last: Option<PathBuf>, cx: &mut Context<Self>) {
        self.browse.expanded.insert(dest_dir);
        if let Some(l) = last {
            self.browse.selected_path = Some(l);
        }
        self.refresh(cx);
        cx.notify();
    }

    /// Copy `from_rel` → `to_rel` within the project (recursive for a directory).
    fn copy_path(
        &self,
        from_rel: &std::path::Path,
        to_rel: &std::path::Path,
    ) -> anyhow::Result<()> {
        let (src, dst) = (self.project_abs(from_rel)?, self.project_abs(to_rel)?);
        copy_tree(&src, &dst)
    }

    /// Copy an EXTERNAL (absolute) source path into `dest_dir` (rel), auto-renaming to dodge
    /// a name clash. Returns the rel destination on success. Used for Finder-copied files.
    fn copy_external_into(
        &mut self,
        src_abs: &std::path::Path,
        dest_dir: &std::path::Path,
    ) -> Option<PathBuf> {
        let name = src_abs.file_name()?;
        let to = self.unique_dest(dest_dir, name);
        let dst = self.project_abs(&to).ok()?;
        match copy_tree(src_abs, &dst) {
            Ok(()) => Some(to),
            Err(e) => {
                self.fail("Pasting", e);
                None
            }
        }
    }

    /// A rel path inside `dir_rel` for `name` that doesn't already exist, appending
    /// " copy", " copy 2", … before the extension (Finder-style) on a clash.
    fn unique_dest(&self, dir_rel: &std::path::Path, name: &std::ffi::OsStr) -> PathBuf {
        let join = |n: &str| {
            if dir_rel.as_os_str().is_empty() {
                PathBuf::from(n)
            } else {
                dir_rel.join(n)
            }
        };
        let taken = |p: &std::path::Path| self.project_abs(p).is_ok_and(|a| a.exists());
        let name = name.to_string_lossy();
        let first = join(&name);
        if !taken(&first) {
            return first;
        }
        let (stem, ext) = split_name(&name);
        for i in 1..10_000 {
            let cand = if i == 1 {
                format!("{stem} copy{ext}")
            } else {
                format!("{stem} copy {i}{ext}")
            };
            let p = join(&cand);
            if !taken(&p) {
                return p;
            }
        }
        join(&format!("{stem} copy{ext}"))
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
        // Local history: a deleted file's content is recoverable from its timeline, and a
        // deletion tombstone marks it gone (so a later re-creation reads as "added back").
        // (Folders are skipped — snapshotting a whole subtree could be huge.)
        if !is_dir {
            self.lh_note_delete(&path, cx);
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

    /// ⌘C — copy the tree selection (issue #67). Browse mode only.
    pub(crate) fn act_copy_files(&mut self, _: &CopyFiles, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode != Mode::Browse {
            return;
        }
        let sel = self.tree_selection();
        self.clip_paths(sel, false, cx);
    }

    /// ⌘X — cut the tree selection (paste moves it). Browse mode only.
    pub(crate) fn act_cut_files(&mut self, _: &CutFiles, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode != Mode::Browse {
            return;
        }
        let sel = self.tree_selection();
        self.clip_paths(sel, true, cx);
    }

    /// ⌘V — paste into the selected folder (or the selected file's folder, else the repo
    /// root). Browse mode only.
    pub(crate) fn act_paste_files(
        &mut self,
        _: &PasteFiles,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Browse {
            return;
        }
        let dest = self.paste_target();
        self.paste_into(dest, cx);
    }

    /// Where a keyboard ⌘V pastes: a selected directory → into it; a selected file → its
    /// parent folder; nothing selected → the repo root.
    fn paste_target(&self) -> PathBuf {
        match self.browse.selected_path.clone() {
            Some(p) if !p.is_absolute() && !p.as_os_str().is_empty() => {
                let is_dir = self.repo_root.as_ref().is_some_and(|r| r.join(&p).is_dir());
                if is_dir {
                    p
                } else {
                    p.parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default()
                }
            }
            _ => PathBuf::new(),
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

    /// Create a scratch file of the given extension, open it, and focus the editor so you
    /// can type immediately.
    pub(crate) fn create_scratch(
        &mut self,
        ext: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        match scratch::create(&root, ext) {
            Ok(path) => {
                self.refresh(cx);
                self.mode = Mode::Browse;
                self.open_file(path, cx);
                self.focus_editor(window, cx);
            }
            Err(e) => self.fail("Creating scratch file", e),
        }
        cx.notify();
    }
}
