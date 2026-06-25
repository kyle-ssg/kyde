//! A real multi-line code editor, modeled on gpui's `examples/input.rs` but
//! extended to multiple lines with tree-sitter syntax highlighting.
//!
//! Architecture (same as gpui's text input):
//!   - `CodeEditor` is an Entity holding the text + selection + layout caches.
//!   - `EditorElement` is a custom `Element` that shapes each line, paints the
//!     caret/selection, and wires the OS input handler via `window.handle_input`.
//!   - Typed text arrives through `EntityInputHandler::replace_text_in_range`
//!     (so IME / dead keys / emoji all work); control keys come through `actions!`.
//!
//! Offsets everywhere are UTF-8 byte offsets into `content` (newlines included).
use crate::highlight::{self, Lang};
use crate::theme;
use gpui::{
    actions, div, fill, point, prelude::*, px, relative, App, Bounds, Context, CursorStyle,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hsla, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollHandle, ShapedLine, SharedString, Style, TextRun,
    UTF16Selection, Window,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

mod element;
use element::EditorElement;

/// Width of the fold gutter (chevron column) drawn left of the text, when the
/// buffer has any foldable regions. Zero otherwise (single-line inputs, plain
/// text with no grammar) so those editors look exactly as before.
const GUTTER_W: Pixels = px(20.0);

actions!(
    kyde_editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        Home,
        End,
        Enter,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Paste,
        Copy,
        Cut,
        Undo,
        Redo,
        Indent,
        Outdent,
        DeleteToHome,
    ]
);

/// Key context names. Multi-line editors use `CONTEXT`; single-line inputs use
/// `CONTEXT_SINGLE` (no enter/up/down bindings, so those bubble to a surrounding
/// context such as the file finder).
pub const CONTEXT: &str = "CodeEditor";
pub const CONTEXT_SINGLE: &str = "CodeInput";

/// Editor line height in px (font size × multiplier, plus 2px breathing room). Shared by
/// the editor render and the diff center gutter so their rows stay in lockstep.
///
/// Rounded to a whole pixel on purpose: the editor positions text via the
/// device-pixel-snapped `window.line_height()`, while the diff center gutter places its
/// `»` chevrons at `row * this`. A fractional value diverges by sub-pixels per line and
/// accumulates into a visible ~1-row drift far down a large diff; an integral, snap-free
/// height keeps the two in exact lockstep at any scroll depth.
pub fn line_height_px() -> f32 {
    (theme::get().editor_font_size * theme::font::LINE_HEIGHT + 2.0).round()
}

/// Key bindings for the editor. Call once at startup (and after a keymap change).
pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    for ctx in [CONTEXT, CONTEXT_SINGLE] {
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some(ctx)),
            // Holding shift while pressing backspace must still delete.
            KeyBinding::new("shift-backspace", Backspace, Some(ctx)),
            // macOS: cmd-backspace deletes from the caret to the start of the line.
            KeyBinding::new("cmd-backspace", DeleteToHome, Some(ctx)),
            KeyBinding::new("delete", Delete, Some(ctx)),
            KeyBinding::new("left", Left, Some(ctx)),
            KeyBinding::new("right", Right, Some(ctx)),
            KeyBinding::new("home", Home, Some(ctx)),
            KeyBinding::new("end", End, Some(ctx)),
            // macOS line nav: Cmd+←/→ jump to line start/end.
            KeyBinding::new("cmd-left", Home, Some(ctx)),
            KeyBinding::new("cmd-right", End, Some(ctx)),
            KeyBinding::new("shift-left", SelectLeft, Some(ctx)),
            KeyBinding::new("shift-right", SelectRight, Some(ctx)),
            KeyBinding::new("cmd-a", SelectAll, Some(ctx)),
            KeyBinding::new("cmd-v", Paste, Some(ctx)),
            KeyBinding::new("cmd-c", Copy, Some(ctx)),
            KeyBinding::new("cmd-x", Cut, Some(ctx)),
            KeyBinding::new("cmd-z", Undo, Some(ctx)),
            KeyBinding::new("cmd-shift-z", Redo, Some(ctx)),
            KeyBinding::new("tab", Indent, Some(ctx)),
            KeyBinding::new("shift-tab", Outdent, Some(ctx)),
        ]);
    }
    // Multi-line only.
    cx.bind_keys([
        KeyBinding::new("up", Up, Some(CONTEXT)),
        KeyBinding::new("down", Down, Some(CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("enter", Enter, Some(CONTEXT)),
    ]);
}

/// Emitted whenever the text changes (used by the file finder to re-query live).
pub enum EditorEvent {
    Changed,
}

/// A point-in-time editor state for the undo/redo stacks.
#[derive(Clone)]
struct Snapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

/// Classifies an edit so consecutive single-character typing coalesces into one
/// undo step (IntelliJ-style) instead of one step per keystroke.
#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Type,
    Other,
}

pub struct CodeEditor {
    pub focus_handle: FocusHandle,
    pub content: String,
    pub lang: Lang,
    pub placeholder: SharedString,
    pub dirty: bool,
    /// Single-line inputs (e.g. the file-finder query) don't bind enter/up/down,
    /// letting those keys bubble to the surrounding key context.
    pub single_line: bool,
    /// Optional key-context override (e.g. the find bar's inputs use "FindBar" so its
    /// enter/escape bindings fire). `None` → the default single/multi-line context.
    pub ctx_override: Option<&'static str>,
    /// Fill the parent's height (clickable across the whole box, e.g. the commit message)
    /// instead of sizing to content. Content-sized editors (file editor, diff panes) scroll;
    /// this one fills.
    pub fill_height: bool,
    /// Soft-wrap long lines to the element width instead of overflowing horizontally.
    /// Implemented by emitting one *display row* per wrapped visual segment (each a normal
    /// uniform-height `ShapedLine`), so the existing caret/click/selection math — which keys
    /// off `line_starts[dr]` + a uniform `line_height` — works unchanged. Used by the commit
    /// message box; the file editor stays unwrapped (horizontal scroll).
    pub soft_wrap: bool,
    /// Read-only panes (e.g. the diff "before" side) render + select + scroll but
    /// reject all mutations.
    pub read_only: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,

    // Undo/redo. Snapshot-based (whole-buffer); fine until the rope buffer lands.
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Kind of the most recent edit, for coalescing runs of typing.
    last_edit: EditKind,

    // ── code folding ──────────────────────────────────────────────
    /// Buffer line indices currently collapsed (each a foldable region start).
    folded: HashSet<usize>,
    /// Foldable regions `(start_line, end_line)`, recomputed on every content
    /// change; folding `start_line` hides `start_line+1 ..= end_line`.
    foldable: Vec<(usize, usize)>,
    /// Set of fold-start lines (= `foldable[*].0`), for O(1) `is_foldable_start`.
    foldable_starts: HashSet<usize>,
    /// Bumped each time content changes; an async highlight/fold job only applies if its
    /// captured generation still matches (so stale big-file results are dropped).
    compute_gen: u64,
    /// Cached syntax spans for the whole buffer, recomputed only on content/lang
    /// change (in `recompute_folds`) — NOT per paint. This is the big-file win:
    /// scrolling/caret-moving no longer re-highlights the entire file each frame.
    spans: Vec<highlight::Span>,
    /// Longest line length in bytes (recomputed on content change) — drives the element's
    /// content width so long lines scroll horizontally instead of clipping.
    max_line_len: usize,

    // Layout caches (populated each paint; used for mouse + vertical movement).
    // `line_layouts` is sparse — only on-screen display rows are shaped (virtualization),
    // keyed by display row. `line_starts`/`visible_lines` stay dense (cheap `usize`s) so the
    // display-row ↔ buffer-line mapping and click→offset math cover the whole file.
    line_layouts: HashMap<usize, ShapedLine>,
    /// Total display rows (folds/filler included) — the dense length, since `line_layouts`
    /// is now sparse and its `.len()` is just the on-screen count.
    display_rows: usize,
    line_starts: Vec<usize>,
    /// Display row → buffer line index (folds skip hidden buffer lines).
    visible_lines: Vec<usize>,
    gutter_w: Pixels,
    /// Width of the line-number sub-column (left part of `gutter_w`); the fold chevron
    /// column sits to its right, so a click in `[num_w, gutter_w)` toggles a fold.
    num_w: Pixels,
    /// Render the gutter (line numbers + fold column) on the RIGHT of the text instead of
    /// the left. Used by the diff's left/base pane so its line numbers sit toward the center
    /// gutter (IntelliJ/GitHub side-by-side style); the right pane keeps numbers on the left.
    pub gutter_right: bool,
    bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    /// The parent scroll container's handle (set by the embedding view via
    /// `set_scroll_handle` whenever it changes, e.g. `file_scroll` in Browse vs
    /// `md_editor_scroll` in the Markdown split). Lets the editor scroll itself to keep the
    /// caret visible on keyboard selection and to auto-scroll while drag-selecting past an
    /// edge. `None` → the editor never scrolls its parent (e.g. single-line inputs).
    pub scroll: Option<ScrollHandle>,
    /// A caret move (keyboard/selection) asked to be revealed; honored on the next paint,
    /// once pixel layout + viewport are known.
    reveal_pending: bool,
    /// Last painted viewport (the parent scroll clip) in window coords — drives drag
    /// auto-scroll edge detection (`on_mouse_move` has no access to the clip otherwise).
    viewport: Option<Bounds<Pixels>>,
    /// Current pointer position during a drag-select, so the auto-scroll loop can keep
    /// extending the selection while the pointer is held past an edge (no mouse-move events).
    drag_pos: Option<Point<Pixels>>,
    /// True while the drag auto-scroll loop is running, so we never spawn a second one.
    autoscroll_active: bool,
    /// Held vertical-nav key as `(direction, extend-selection)` — `Some` while ↑/↓ (or
    /// Shift+↑/↓) is held, driving the accelerating auto-repeat loop. `None` = released.
    nav: Option<(i32, bool)>,
    /// True while the vertical-nav auto-repeat loop is running (one at a time).
    nav_active: bool,
    /// Ticks elapsed in the current hold, for the acceleration curve.
    nav_ticks: u32,
    /// Fractional carry of lines-to-move, so a sub-1-line-per-tick speed still advances.
    nav_accum: f32,
    /// True only while THIS editor owns an in-progress drag-select, so a drag that began
    /// elsewhere (e.g. the diff divider) sweeping over it doesn't select text.
    is_selecting: bool,
    /// Caret blink: visible this half-cycle. `blink_epoch` cancels a stale blink loop after
    /// activity restarts it (so the caret is solid right after typing/moving, then blinks).
    blink_on: bool,
    blink_epoch: usize,
    blink_started: bool,
    /// Show a line-number gutter (diff panes + future editor option).
    pub line_numbers: bool,
    /// Per buffer-line background tint (diff hunk colors). Empty = none.
    pub line_bg: std::collections::HashMap<usize, gpui::Rgba>,
    /// Per buffer-line word-level highlight: byte ranges within the line that actually
    /// changed (inline word diff), painted in `word_bg_color` over the line tint.
    pub word_bg: std::collections::HashMap<usize, Vec<std::ops::Range<usize>>>,
    pub word_bg_color: gpui::Rgba,
    /// Diff alignment: insert N blank display rows BEFORE buffer line `b` (`filler[b]=N`),
    /// so this pane lines up row-for-row with the other side. `filler_end` = trailing blanks.
    pub filler: std::collections::HashMap<usize, usize>,
    pub filler_end: usize,
}

impl CodeEditor {
    pub fn new(cx: &mut Context<Self>, content: String, lang: Lang, placeholder: &str) -> Self {
        Self::with_options(cx, content, lang, placeholder, false)
    }

    pub fn single_line(cx: &mut Context<Self>, placeholder: &str) -> Self {
        Self::with_options(cx, String::new(), Lang::PlainText, placeholder, true)
    }

    /// A read-only editor (selectable, scrollable, highlighted — but not editable).
    pub fn read_only(cx: &mut Context<Self>, content: String, lang: Lang) -> Self {
        let mut e = Self::with_options(cx, content, lang, "", false);
        e.read_only = true;
        e
    }

    pub fn with_options(
        cx: &mut Context<Self>,
        content: String,
        lang: Lang,
        placeholder: &str,
        single_line: bool,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content,
            lang,
            placeholder: placeholder.to_string().into(),
            dirty: false,
            single_line,
            ctx_override: None,
            fill_height: false,
            soft_wrap: false,
            read_only: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Other,
            folded: HashSet::new(),
            foldable: Vec::new(),
            foldable_starts: HashSet::new(),
            compute_gen: 0,
            spans: Vec::new(),
            max_line_len: 0,
            line_layouts: HashMap::new(),
            display_rows: 0,
            line_starts: Vec::new(),
            visible_lines: Vec::new(),
            gutter_w: px(0.0),
            num_w: px(0.0),
            gutter_right: false,
            bounds: None,
            line_height: px(16.0),
            scroll: None,
            reveal_pending: false,
            viewport: None,
            drag_pos: None,
            autoscroll_active: false,
            nav: None,
            nav_active: false,
            nav_ticks: 0,
            nav_accum: 0.0,
            is_selecting: false,
            blink_on: true,
            blink_epoch: 0,
            blink_started: false,
            line_numbers: false,
            line_bg: std::collections::HashMap::new(),
            word_bg: std::collections::HashMap::new(),
            word_bg_color: gpui::rgba(0x00000000),
            filler: std::collections::HashMap::new(),
            filler_end: 0,
        }
    }

    /// Number of display rows the last frame *shaped* (the sparse `line_layouts` size).
    /// With virtualization this stays ≈ the on-screen window even for huge files; a
    /// regression to shaping every line would blow it up to the file's row count. Used by
    /// the big-file virtualization perf guard.
    #[cfg(test)]
    pub fn shaped_row_count(&self) -> usize {
        self.line_layouts.len()
    }
    /// Total display rows of the last frame (dense) — the file's row count (± folds/filler).
    #[cfg(test)]
    pub fn display_row_count(&self) -> usize {
        self.display_rows
    }

    pub fn set_content(&mut self, content: String, lang: Lang, cx: &mut Context<Self>) {
        self.content = content;
        self.lang = lang;
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.dirty = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = EditKind::Other;
        self.folded.clear();
        self.recompute_folds(cx);
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Tell the editor which scroll container currently holds it, so caret-follow and
    /// drag auto-scroll can move it. Idempotent; call whenever the active container may
    /// change (e.g. a file opens in Browse vs the Markdown split). Cheap — `ScrollHandle`
    /// is a shared handle, so this just stores a clone.
    pub fn set_scroll_handle(&mut self, scroll: ScrollHandle) {
        self.scroll = Some(scroll);
    }

    /// Pixel width of the widest line (monospace estimate) plus the gutter — the width an
    /// explicit-width wrapper must give this editor so long lines overflow the scroll viewport
    /// and the horizontal scrollbar appears. Mirrors the `content_w` used in `request_layout`.
    pub fn content_width(&self) -> f32 {
        let char_w = theme::get().editor_font_size * 0.6;
        64.0 + self.max_line_len as f32 * char_w + 24.0
    }

    /// Switch the language and re-highlight the buffer in place, keeping the caret,
    /// selection, and scroll. Used when a syntax pack is installed for the open file so
    /// its colors apply immediately instead of only after reopening it.
    pub fn set_lang(&mut self, lang: Lang, cx: &mut Context<Self>) {
        self.lang = lang;
        self.recompute_folds(cx); // rebuilds `spans` from the new grammar
        cx.notify();
    }

    /// Select a byte range (used by find/replace to highlight the current match).
    pub fn select_range(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        let len = self.content.len();
        self.selected_range = range.start.min(len)..range.end.min(len);
        self.selection_reversed = false;
        // Scroll the match into view (Find jump-to-match relies on this).
        self.reveal_pending = true;
        cx.notify();
    }

    /// Replace a byte range with `text` (find/replace), recording one undo step.
    pub fn replace_range_text(
        &mut self,
        range: std::ops::Range<usize>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let len = self.content.len();
        let r = range.start.min(len)..range.end.min(len);
        self.record_undo(EditKind::Other);
        self.content.replace_range(r.clone(), text);
        let caret = r.start + text.len();
        self.selected_range = caret..caret;
        self.dirty = true;
        self.recompute_folds(cx);
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }

    // ── code folding ──────────────────────────────────────────────
    /// Re-derive foldable regions from the current text + language, dropping any
    /// folded entry whose start line is no longer a foldable region (after edits).
    fn recompute_folds(&mut self, cx: &mut Context<Self>) {
        if self.single_line || self.content.is_empty() {
            self.spans = Vec::new();
            self.foldable = Vec::new();
            self.foldable_starts.clear();
            self.max_line_len = 0;
            return;
        }
        // Longest line (bytes) → element content width for horizontal scroll. Cheap byte
        // scan, only on content change (not per frame).
        self.max_line_len = self.content.split('\n').map(|l| l.len()).max().unwrap_or(0);
        // Small files: highlight + find folds inline (correct on the very first frame, no
        // flicker). Big files: clear now so the file opens instantly as plain text, then
        // compute spans + folds on a background thread and swap them in when ready — the
        // whole-file tree-sitter parse never blocks the UI (Zed-style async highlighting).
        let lines = self.content.bytes().filter(|&b| b == b'\n').count();
        const ASYNC_THRESHOLD: usize = 4000;
        if lines < ASYNC_THRESHOLD {
            self.spans = highlight::highlight(&self.content, self.lang);
            self.foldable = highlight::fold_regions(&self.content, self.lang);
            self.foldable_starts = self.foldable.iter().map(|r| r.0).collect();
            self.folded.retain(|l| self.foldable_starts.contains(l));
            return;
        }
        self.spans = Vec::new();
        self.foldable = Vec::new();
        self.foldable_starts.clear();
        self.compute_gen = self.compute_gen.wrapping_add(1);
        let gen = self.compute_gen;
        let content = self.content.clone();
        let lang = self.lang;
        cx.spawn(async move |this, cx| {
            let (spans, foldable) = cx
                .background_executor()
                .spawn(async move {
                    (
                        highlight::highlight(&content, lang),
                        highlight::fold_regions(&content, lang),
                    )
                })
                .await;
            this.update(cx, |ed, cx| {
                if ed.compute_gen == gen {
                    // Still the current content → apply the highlight + folds.
                    ed.spans = spans;
                    ed.foldable_starts = foldable.iter().map(|r| r.0).collect();
                    ed.foldable = foldable;
                    ed.folded.retain(|l| ed.foldable_starts.contains(l));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// (Re)start the caret blink: show it now and bump the epoch so any prior blink loop exits,
    /// then drive a 530ms toggle. Called on focus-start and after activity so the caret is solid
    /// immediately after typing/moving, then resumes blinking.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_on = true;
        self.blink_epoch = self.blink_epoch.wrapping_add(1);
        let epoch = self.blink_epoch;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(530))
                .await;
            let alive = this
                .update(cx, |ed, cx| {
                    if ed.blink_epoch != epoch {
                        return false;
                    }
                    ed.blink_on = !ed.blink_on;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    // The fold *mapping* (which lines are hidden / visible) lives in pure
    // module-level functions below (`is_hidden`, `visible_line_indices`,
    // `caret_after_fold`, …) so it's unit-testable without a gpui Context. These
    // methods are thin delegators over the editor's `content`/`folded`/`foldable`.
    fn is_foldable_start(&self, line: usize) -> bool {
        self.foldable_starts.contains(&line)
    }

    /// Buffer line indices that are visible, in order (display row → buffer line).
    fn visible_line_indices(&self) -> Vec<usize> {
        visible_line_indices(&self.content, &self.folded, &self.foldable)
    }

    /// If the caret sits on a now-hidden line (after a fold toggle or an undo that
    /// re-collapsed a region), pull it to the end of the enclosing fold's start
    /// line so it stays visible. No-op otherwise.
    fn ensure_caret_visible(&mut self) {
        if let Some(at) =
            caret_after_fold(&self.content, &self.folded, &self.foldable, self.cursor())
        {
            self.selected_range = at..at;
            self.selection_reversed = false;
        }
    }

    /// Toggle the fold at buffer `line` (a foldable region start), keeping the
    /// caret visible.
    fn toggle_fold(&mut self, line: usize, cx: &mut Context<Self>) {
        if !self.is_foldable_start(line) {
            return;
        }
        if !self.folded.remove(&line) {
            self.folded.insert(line);
        }
        self.ensure_caret_visible();
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, off: usize, cx: &mut Context<Self>) {
        self.selected_range = off..off;
        // Navigating ends the current typing run, so the next character starts a
        // fresh undo step rather than coalescing across the cursor jump.
        self.last_edit = EditKind::Other;
        self.reveal_pending = true; // keep the caret in view (scrolls on next paint)
        cx.notify();
    }

    // ── undo / redo ───────────────────────────────────────────────
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    /// Record the pre-edit state on the undo stack before a mutation. Consecutive
    /// single-character typing (`EditKind::Type`) coalesces into the existing step.
    fn record_undo(&mut self, kind: EditKind) {
        self.redo_stack.clear();
        if kind == EditKind::Type && self.last_edit == EditKind::Type {
            self.last_edit = kind;
            return; // merge into the run already on the stack
        }
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 1000 {
            self.undo_stack.remove(0);
        }
        self.last_edit = kind;
    }

    fn restore(&mut self, s: Snapshot, cx: &mut Context<Self>) {
        self.content = s.content;
        self.selected_range = s.selected_range;
        self.selection_reversed = s.selection_reversed;
        self.marked_range = None;
        self.last_edit = EditKind::Other;
        self.dirty = true;
        self.recompute_folds(cx);
        // The restored caret may land inside a region that's still folded — don't
        // strand it on a hidden line (next Up/Down would otherwise jump to line 0).
        self.ensure_caret_visible();
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if let Some(prev) = self.undo_stack.pop() {
            let now = self.snapshot();
            self.restore(prev, cx);
            self.redo_stack.push(now);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if let Some(next) = self.redo_stack.pop() {
            let now = self.snapshot();
            self.restore(next, cx);
            self.undo_stack.push(now);
        }
    }

    fn select_to(&mut self, off: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = off;
        } else {
            self.selected_range.end = off;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        // Shift+arrows etc. move the active selection end — reveal it so extending the
        // selection past the viewport scrolls to follow the caret.
        self.reveal_pending = true;
        cx.notify();
    }

    // ── grapheme-aware horizontal boundaries ──────────────────────
    fn prev_boundary(&self, off: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < off).then_some(i))
            .unwrap_or(0)
    }
    fn next_boundary(&self, off: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > off).then_some(i))
            .unwrap_or(self.content.len())
    }

    // ── line geometry ─────────────────────────────────────────────
    /// (line_index, byte offset of line start) for a content offset.
    fn locate(&self, off: usize) -> (usize, usize) {
        let mut start = 0usize;
        for (i, line) in self.content.split('\n').enumerate() {
            let end = start + line.len();
            if off <= end {
                return (i, start);
            }
            start = end + 1; // skip '\n'
        }
        let lines = self.content.split('\n').count().saturating_sub(1);
        (lines, start)
    }

    // ── actions ───────────────────────────────────────────────────
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.prev_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.prev_boundary(self.cursor()), cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let (_, start) = self.locate(self.cursor());
        self.move_to(start, cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let (li, start) = self.locate(self.cursor());
        let line_len = self
            .content
            .split('\n')
            .nth(li)
            .map(|l| l.len())
            .unwrap_or(0);
        self.move_to(start + line_len, cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, false, cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, false, cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, true, cx);
    }
    /// Move the caret one display row in `dir` (skipping folds). `extend` keeps
    /// the selection anchor (Shift+↑/↓) instead of collapsing it.
    fn vertical(&mut self, dir: i32, extend: bool, cx: &mut Context<Self>) {
        if self.line_starts.is_empty() {
            return;
        }
        let (li, start) = self.locate(self.cursor());
        // Move by *display* rows so folds are skipped (line_starts/line_layouts
        // are display-indexed; visible_lines maps display row → buffer line).
        let dr = self
            .visible_lines
            .iter()
            .position(|&b| b == li)
            .unwrap_or(0);
        let target = dr as i32 + dir;
        if target < 0 || target as usize >= self.display_rows {
            return;
        }
        let target = target as usize;
        let col = self.cursor() - start;
        // `line_layouts` is sparse (visible rows only); the current + adjacent rows are within
        // the overscan band, so they're shaped. Fall back to x=0 if somehow off-screen.
        let x = self
            .line_layouts
            .get(&dr)
            .map(|l| l.x_for_index(col))
            .unwrap_or(px(0.0));
        let Some(&new_start) = self.line_starts.get(target) else {
            return;
        };
        let new_col = self
            .line_layouts
            .get(&target)
            .map(|l| l.closest_index_for_x(x))
            .unwrap_or(0);
        let off = new_start + new_col;
        if extend {
            self.select_to(off, cx);
        } else {
            self.move_to(off, cx);
        }
    }
    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.prev_boundary(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }
    /// cmd-backspace: delete from the caret back to the start of the current line
    /// (or just delete the selection if there is one).
    fn delete_to_home(&mut self, _: &DeleteToHome, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let caret = self.cursor();
            let line_start = self.content[..caret]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            self.select_to(line_start, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.selected_range.start == self.selected_range.end {
            // No selection: insert one indent unit at the caret.
            let c = self.cursor();
            self.record_undo(EditKind::Other);
            self.content.insert_str(c, "  ");
            self.selected_range = (c + 2)..(c + 2);
            self.finish_indent(cx);
        } else {
            self.shift_lines(true, cx);
        }
    }
    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.read_only {
            self.shift_lines(false, cx);
        }
    }
    /// Indent/outdent every line the selection touches by one unit (2 spaces).
    fn shift_lines(&mut self, indent: bool, cx: &mut Context<Self>) {
        let unit = "  ";
        let (s, e) = (self.selected_range.start, self.selected_range.end);
        let region_start = self.content[..s].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let region_end = self.content[e..]
            .find('\n')
            .map(|i| e + i)
            .unwrap_or(self.content.len());
        let region = self.content[region_start..region_end].to_string();
        let mut out = String::new();
        for (i, line) in region.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            if indent {
                out.push_str(unit);
                out.push_str(line);
            } else {
                let strip = if line.starts_with(unit) {
                    2
                } else if line.starts_with(' ') || line.starts_with('\t') {
                    1
                } else {
                    0
                };
                out.push_str(&line[strip..]);
            }
        }
        self.record_undo(EditKind::Other);
        self.content = format!(
            "{}{}{}",
            &self.content[..region_start],
            out,
            &self.content[region_end..]
        );
        self.selected_range = region_start..(region_start + out.len());
        self.selection_reversed = false;
        self.finish_indent(cx);
    }
    fn finish_indent(&mut self, cx: &mut Context<Self>) {
        self.marked_range = None;
        self.last_edit = EditKind::Other;
        self.dirty = true;
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    // ── mouse ─────────────────────────────────────────────────────
    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Take keyboard focus on click so this editor is the action target — without it,
        // IME text can route here incidentally while actions (cmd-z, etc.) don't dispatch.
        self.focus_handle.focus(window);
        self.restart_blink(cx);
        // A click in the fold gutter toggles the fold on that display row instead
        // of moving the caret.
        if self.gutter_w > px(0.0) {
            if let Some(bounds) = self.bounds {
                // The chevron column is the part of the gutter just past the line numbers.
                // With the gutter on the right (diff base pane) it sits at the far right.
                let gutter_left = if self.gutter_right {
                    bounds.size.width - self.gutter_w
                } else {
                    px(0.0)
                };
                let rel_x = ev.position.x - bounds.left() - gutter_left;
                if rel_x >= self.num_w && rel_x < self.gutter_w && !self.line_layouts.is_empty() {
                    let rel_y = (ev.position.y - bounds.top()).max(px(0.0));
                    let dr = ((f32::from(rel_y) / f32::from(self.line_height)).floor() as usize)
                        .min(self.visible_lines.len().saturating_sub(1));
                    if let Some(&line) = self.visible_lines.get(dr) {
                        if self.is_foldable_start(line) {
                            self.toggle_fold(line, cx);
                            return;
                        }
                    }
                }
            }
        }
        // Double-click selects the word; triple-click (+) selects the whole line. Keep
        // `is_selecting` on so a drag started from the same press still extends the selection.
        if ev.click_count >= 2 {
            self.focus_handle.focus(window);
            let off = self.offset_for_position(ev.position);
            if ev.click_count == 2 {
                self.select_word_at(off, cx);
            } else {
                self.select_line_at(off, cx);
            }
            self.is_selecting = true;
            cx.stop_propagation();
            return;
        }
        self.click_at(ev.position, ev.modifiers.shift, window, cx);
        // Consume so a click on the editor doesn't ALSO bubble to a surrounding scroll
        // container that forwards clicks (see `click_at`) — that would double-handle.
        cx.stop_propagation();
    }

    /// Select the word (run of alphanumeric/`_`) containing byte offset `off`. If `off` isn't
    /// on a word character, leaves a collapsed caret there.
    fn select_word_at(&mut self, off: usize, cx: &mut Context<Self>) {
        let bytes = self.content.as_bytes();
        let off = off.min(bytes.len());
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut start = off;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = off;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        self.selected_range = start..end;
        self.selection_reversed = false;
        cx.notify();
    }

    /// Select the whole line (its text, excluding the trailing newline) containing `off`.
    fn select_line_at(&mut self, off: usize, cx: &mut Context<Self>) {
        let off = off.min(self.content.len());
        let start = self.content[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = self.content[off..]
            .find('\n')
            .map(|i| off + i)
            .unwrap_or(self.content.len());
        self.selected_range = start..end;
        self.selection_reversed = false;
        cx.notify();
    }
    /// Place the caret at window position `pos` (extending the selection if `extend`). Public
    /// so a surrounding scroll container can forward clicks: when a file is shorter than the
    /// viewport, a click in the empty area below the text still lands here, and
    /// `offset_for_position` snaps it to the end of the last line.
    pub fn click_at(
        &mut self,
        pos: Point<Pixels>,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window);
        let off = self.offset_for_position(pos);
        self.is_selecting = true;
        if extend {
            self.select_to(off, cx);
        } else {
            self.move_to(off, cx);
        }
    }
    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Only extend the selection for a drag this editor itself started.
        if self.is_selecting && ev.dragging() {
            // `select_to` sets `reveal_pending`, which would yank the view back to the
            // caret and fight the edge auto-scroll — drop it; the drag drives scrolling.
            let off = self.offset_for_position(ev.position);
            self.select_to(off, cx);
            self.reveal_pending = false;
            self.drag_pos = Some(ev.position);
            // Pointer past an edge → start the auto-scroll loop (it self-stops when the
            // pointer comes back inside or the drag ends).
            if self.autoscroll_step(ev.position) != Point::default() {
                self.start_autoscroll(cx);
            }
        }
    }
    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.drag_pos = None; // ends the auto-scroll loop on its next tick
    }

    /// How far (px) the view should scroll this tick given the pointer position during a
    /// drag-select: nonzero on each axis only when the pointer is within `MARGIN` of (or
    /// past) the corresponding viewport edge. Speed ramps with how far past the edge it is.
    fn autoscroll_step(&self, pos: Point<Pixels>) -> Point<Pixels> {
        let Some(vp) = self.viewport else {
            return Point::default();
        };
        const MARGIN: f32 = 24.0;
        const MAX: f32 = 48.0;
        let axis = |p: f32, lo: f32, hi: f32| -> f32 {
            if p > hi - MARGIN {
                -((p - (hi - MARGIN)).min(MAX)) // past bottom/right → scroll content up/left
            } else if p < lo + MARGIN {
                ((lo + MARGIN) - p).min(MAX) // past top/left → scroll content down/right
            } else {
                0.0
            }
        };
        point(
            px(axis(
                f32::from(pos.x),
                f32::from(vp.left()),
                f32::from(vp.right()),
            )),
            px(axis(
                f32::from(pos.y),
                f32::from(vp.top()),
                f32::from(vp.bottom()),
            )),
        )
    }

    /// Apply one auto-scroll step to the parent handle (clamped to the scroll range) and
    /// return whether it actually moved.
    fn apply_autoscroll(&self, step: Point<Pixels>) -> bool {
        let Some(scroll) = self.scroll.as_ref() else {
            return false;
        };
        let max = scroll.max_offset();
        let cur = scroll.offset();
        let nx = (cur.x + step.x).clamp(-max.width, px(0.0));
        let ny = (cur.y + step.y).clamp(-max.height, px(0.0));
        if nx == cur.x && ny == cur.y {
            return false;
        }
        scroll.set_offset(point(nx, ny));
        true
    }

    /// Drive continuous auto-scroll while the pointer is held past a viewport edge mid-drag.
    /// Mouse-move events stop firing when the pointer is still, so a timer loop keeps the
    /// view scrolling (and the selection extending) until the pointer returns inside or the
    /// drag ends. Guarded by `autoscroll_active` so only one loop runs.
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscroll_active {
            return;
        }
        self.autoscroll_active = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
            let keep = this
                .update(cx, |ed, cx| {
                    let Some(pos) = ed.drag_pos.filter(|_| ed.is_selecting) else {
                        ed.autoscroll_active = false;
                        return false;
                    };
                    let step = ed.autoscroll_step(pos);
                    if step == Point::default() || !ed.apply_autoscroll(step) {
                        ed.autoscroll_active = false;
                        return false;
                    }
                    // Re-extend the selection to the (now differently-mapped) pointer.
                    let off = ed.offset_for_position(pos);
                    ed.select_to(off, cx);
                    ed.reveal_pending = false;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
    }

    /// Held ↑/↓ (and Shift+↑/↓) start an accelerating auto-repeat. The bound action
    /// (`Up`/`Down`/`SelectUp`/`SelectDown`) still does the first line on the initial press;
    /// this just notes the held key and kicks off the repeat loop. OS key-repeat is
    /// unreliable here (it wasn't moving while held at all), so we self-pace with a timer.
    fn on_key_down_nav(&mut self, ev: &gpui::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.single_line {
            return;
        }
        let dir = match ev.keystroke.key.as_str() {
            "down" => 1,
            "up" => -1,
            _ => return,
        };
        // OS repeats (is_held) are ignored — our own loop drives the repetition.
        if ev.is_held {
            return;
        }
        self.nav = Some((dir, ev.keystroke.modifiers.shift));
        self.nav_ticks = 0;
        self.nav_accum = 0.0;
        self.start_nav_repeat(cx);
    }

    fn on_key_up_nav(&mut self, ev: &gpui::KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let released = match ev.keystroke.key.as_str() {
            "down" => 1,
            "up" => -1,
            _ => return,
        };
        // Stop only if the released arrow matches the held direction (ignore Shift release).
        if matches!(self.nav, Some((dir, _)) if dir == released) {
            self.nav = None;
        }
    }

    /// Auto-repeat the held vertical move, accelerating so a long file is reachable without
    /// frantic tapping — but capped so it never feels runaway. A short initial delay lets a
    /// quick tap stay a single line (the bound action already moved it).
    fn start_nav_repeat(&mut self, cx: &mut Context<Self>) {
        if self.nav_active {
            return;
        }
        self.nav_active = true;
        cx.spawn(async move |this, cx| {
            // Initial delay (typematic-style): a tap releases before this and never repeats.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(260))
                .await;
            loop {
                let keep = this
                    .update(cx, |ed, cx| {
                        let Some((dir, extend)) = ed.nav else {
                            ed.nav_active = false;
                            return false;
                        };
                        ed.nav_ticks += 1;
                        // Lines/sec ramps from a gentle start up to a firm cap (~1.5s to peak).
                        let held_ms = ed.nav_ticks as f32 * 16.0;
                        let lps = (24.0 + held_ms * 0.16).min(170.0);
                        ed.nav_accum += lps * 0.016;
                        let n = ed.nav_accum.floor() as i32;
                        if n >= 1 {
                            ed.nav_accum -= n as f32;
                            for _ in 0..n {
                                let before = ed.cursor();
                                ed.vertical(dir, extend, cx);
                                if ed.cursor() == before {
                                    break; // reached the top/bottom of the file
                                }
                            }
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
    }

    fn offset_for_position(&self, pos: Point<Pixels>) -> usize {
        let Some(bounds) = self.bounds else { return 0 };
        // When empty, the shaped line is the (non-selectable) placeholder — clicking it
        // must not produce an offset into placeholder text.
        if self.line_starts.is_empty() || self.content.is_empty() {
            return 0;
        }
        let rel_y = (pos.y - bounds.top()).max(px(0.0));
        let raw_dr = (f32::from(rel_y) / f32::from(self.line_height)).floor() as usize;
        let last = self.display_rows.saturating_sub(1);
        // Click below the last line → caret at the END of the last line (standard editor
        // behaviour), regardless of the x position.
        if raw_dr > last {
            let line_len = self.content[self.line_starts[last]..]
                .split('\n')
                .next()
                .map(|l| l.len())
                .unwrap_or(0);
            return (self.line_starts[last] + line_len).min(self.content.len());
        }
        let dr = raw_dr.min(last);
        // Map the click x into text-local space. Left gutter shifts text right by `gutter_w`;
        // a right gutter leaves text flush at the left edge. `line_layouts` is sparse but the
        // clicked row is on-screen, so it's present; fall back to col 0.
        let text_left = if self.gutter_right {
            px(0.0)
        } else {
            self.gutter_w
        };
        let x = pos.x - bounds.left() - text_left;
        let col = self
            .line_layouts
            .get(&dr)
            .map(|l| l.closest_index_for_x(x))
            .unwrap_or(0);
        // Clamp to real content length so a click past the last char (or into a
        // placeholder/filler) never lands beyond the buffer.
        (self.line_starts[dr] + col).min(self.content.len())
    }

    // ── utf16 helpers (for the OS input handler) ──────────────────
    fn off_from_utf16(&self, o: usize) -> usize {
        let (mut u8o, mut u16o) = (0, 0);
        for ch in self.content.chars() {
            if u16o >= o {
                break;
            }
            u16o += ch.len_utf16();
            u8o += ch.len_utf8();
        }
        u8o
    }
    fn off_to_utf16(&self, o: usize) -> usize {
        let (mut u16o, mut u8o) = (0, 0);
        for ch in self.content.chars() {
            if u8o >= o {
                break;
            }
            u8o += ch.len_utf8();
            u16o += ch.len_utf16();
        }
        u16o
    }
    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.off_to_utf16(r.start)..self.off_to_utf16(r.end)
    }
    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.off_from_utf16(r.start)..self.off_from_utf16(r.end)
    }
}

impl Focusable for CodeEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for CodeEditor {}

impl EntityInputHandler for CodeEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.range_from_utf16(&range_utf16);
        actual.replace(self.range_to_utf16(&r));
        Some(self.content[r].to_string())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        // A single character typed over a collapsed caret = a "Type" run (coalesces);
        // everything else (paste, delete, newline, replacing a selection) is its own step.
        let kind = if range.start == range.end && new_text != "\n" && new_text.chars().count() == 1
        {
            EditKind::Type
        } else {
            EditKind::Other
        };
        self.record_undo(kind);
        self.content =
            self.content[..range.start].to_owned() + new_text + &self.content[range.end..];
        let caret = range.start + new_text.len();
        self.selected_range = caret..caret;
        self.marked_range = None;
        self.dirty = true;
        self.recompute_folds(cx);
        self.restart_blink(cx);
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_sel_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        // Snapshot once at the start of an IME composition, not on every marked update.
        if self.marked_range.is_none() {
            self.record_undo(EditKind::Other);
        }
        self.content =
            self.content[..range.start].to_owned() + new_text + &self.content[range.end..];
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_sel_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.end)
            .unwrap_or_else(|| {
                let c = range.start + new_text.len();
                c..c
            });
        self.dirty = true;
        self.recompute_folds(cx);
        cx.notify();
        cx.emit(EditorEvent::Changed);
    }
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let r = self.range_from_utf16(&range_utf16);
        let (li, start) = self.locate(r.start);
        let dr = self
            .visible_lines
            .iter()
            .position(|&b| b == li)
            .unwrap_or(0);
        let line = self.line_layouts.get(&dr)?;
        let x = line.x_for_index(r.start - start);
        let y = bounds.top() + self.line_height * (dr as f32);
        Some(Bounds::from_corners(
            point(bounds.left() + x, y),
            point(bounds.left() + x, y + self.line_height),
        ))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.off_to_utf16(self.offset_for_position(point)))
    }
}

impl Render for CodeEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Kick off the caret blink loop once (the editor entity now exists, so spawning is safe).
        if !self.blink_started {
            self.blink_started = true;
            self.restart_blink(cx);
        }
        // The base editor context (so cmd-a / cmd-backspace / arrows / cmd-c etc. always
        // work), PLUS any override as an ADDITIONAL context — not a replacement. gpui matches
        // a binding if its context appears anywhere in the stack, so "CodeInput FindBar" keeps
        // the editing keys while also firing the find bar's enter/escape. (Replacing the
        // context with just "FindBar" silently dropped every editor key — the cmd-a/
        // cmd-backspace-in-the-find-bar bug.)
        let base = if self.single_line {
            CONTEXT_SINGLE
        } else {
            CONTEXT
        };
        let ctx: String = match self.ctx_override {
            Some(o) => format!("{base} {o}"),
            None => base.to_string(),
        };
        // Fill the parent (clickable full-box) for single-line inputs and the commit box;
        // content-size (so a tall file overflows + scrolls) for the file editor + diff panes.
        let fill = self.single_line || self.fill_height;
        div()
            .key_context(ctx.as_str())
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .when(!fill, |d| d.w_full().min_h(relative(1.0)))
            .when(fill, |d| d.size_full())
            // Transparent: the editor adopts its container's background (the rounded editor
            // island, the commit box, the finder) so it never paints square corners over a
            // rounded parent (gpui clips to a rectangle, not the rounded shape).
            .text_color(theme::get().text)
            .font_family(theme::font::FAMILY)
            .text_size(px(theme::get().editor_font_size))
            .line_height(px(line_height_px()))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_to_home))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            // Enter / Up / Down only for multi-line editors; single-line inputs let
            // these bubble to the surrounding context (e.g. the file finder).
            .when(!self.single_line, |d| {
                d.on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::select_up))
                    .on_action(cx.listener(Self::select_down))
                    .on_action(cx.listener(Self::enter))
                    .on_action(cx.listener(Self::indent))
                    .on_action(cx.listener(Self::outdent))
                    // Held ↑/↓ → accelerating auto-repeat (OS key-repeat wasn't moving).
                    .on_key_down(cx.listener(Self::on_key_down_nav))
                    .on_key_up(cx.listener(Self::on_key_up_nav))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(EditorElement {
                editor: cx.entity(),
            })
    }
}

/// (line_index, line_start_offset) for an offset, given precomputed line starts.
fn locate_in(line_starts: &[usize], content: &str, off: usize) -> (usize, usize) {
    let mut idx = 0;
    for (i, &s) in line_starts.iter().enumerate() {
        if s <= off {
            idx = i;
        } else {
            break;
        }
    }
    let _ = content;
    // Guard: never index an empty slice (e.g. a frame where no rows were laid out).
    (idx, line_starts.get(idx).copied().unwrap_or(0))
}

/// Build TextRuns covering one line, colored by the highlight spans that fall in it.
/// Public so the side-by-side diff can color its lines with the same logic.
pub fn line_runs(
    line: &str,
    line_start: usize,
    spans: &[highlight::Span],
    font: &gpui::Font,
    default: Hsla,
) -> Vec<TextRun> {
    let line_end = line_start + line.len();
    let mut runs = Vec::new();
    let mut cursor = line_start;
    let mk = |len: usize, color: Hsla| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    // `spans` is sorted by position and non-overlapping, so binary-search the first one
    // that reaches this line and stop once we pass its end — O(log n + spans-in-line)
    // instead of scanning every span in the file for each visible line (the big-file killer).
    let first = spans.partition_point(|s| s.end <= line_start);
    for s in &spans[first..] {
        if s.start >= line_end {
            break;
        }
        let a = s.start.max(line_start);
        let b = s.end.min(line_end);
        if a > cursor {
            runs.push(mk(a - cursor, default));
        }
        if b > a {
            runs.push(mk(b - a, s.color.into()));
            cursor = b;
        }
    }
    if cursor < line_end {
        runs.push(mk(line_end - cursor, default));
    }
    if runs.is_empty() {
        runs.push(mk(0, default));
    }
    runs
}

// ── fold mapping (pure, unit-testable — no gpui) ───────────────────
// These operate only on the editor's data (`content` + `folded` set + `foldable`
// regions), so the index arithmetic that drives the fold display can be tested
// directly. `CodeEditor`'s methods are thin wrappers over these.

/// End line of the foldable region starting at `line`, if any.
fn fold_end(foldable: &[(usize, usize)], line: usize) -> Option<usize> {
    foldable.iter().find(|r| r.0 == line).map(|r| r.1)
}

/// Is `line` collapsed inside some currently-folded region?
fn is_hidden(folded: &HashSet<usize>, foldable: &[(usize, usize)], line: usize) -> bool {
    folded.iter().any(|&s| match fold_end(foldable, s) {
        Some(e) => line > s && line <= e,
        None => false,
    })
}

/// Buffer line indices that are visible, in order (display row → buffer line).
fn visible_line_indices(
    content: &str,
    folded: &HashSet<usize>,
    foldable: &[(usize, usize)],
) -> Vec<usize> {
    let total = content.split('\n').count().max(1);
    (0..total)
        .filter(|&l| !is_hidden(folded, foldable, l))
        .collect()
}

/// Byte offset of the start of buffer `line`.
fn line_byte_start(content: &str, line: usize) -> usize {
    content.split('\n').take(line).map(|l| l.len() + 1).sum()
}

/// 0-based buffer line containing byte offset `off` (mirrors `CodeEditor::locate`).
fn line_of_offset(content: &str, off: usize) -> usize {
    let mut start = 0usize;
    for (i, line) in content.split('\n').enumerate() {
        let end = start + line.len();
        if off <= end {
            return i;
        }
        start = end + 1;
    }
    content.split('\n').count().saturating_sub(1)
}

/// If a caret at byte offset `caret` lands on a hidden (folded-away) line, return
/// the offset it should move to — the end of the enclosing fold's start line —
/// so it stays visible. `None` when the caret is already visible.
fn caret_after_fold(
    content: &str,
    folded: &HashSet<usize>,
    foldable: &[(usize, usize)],
    caret: usize,
) -> Option<usize> {
    let cli = line_of_offset(content, caret);
    if !is_hidden(folded, foldable, cli) {
        return None;
    }
    let start = folded
        .iter()
        .copied()
        .find(|&s| fold_end(foldable, s).is_some_and(|e| cli > s && cli <= e))?;
    let line_len = content.split('\n').nth(start).map(|l| l.len()).unwrap_or(0);
    Some(line_byte_start(content, start) + line_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn set(items: &[usize]) -> HashSet<usize> {
        items.iter().copied().collect()
    }

    /// Drag auto-scroll: a step is produced only when the pointer is within the edge margin
    /// (or past it), in the direction that brings more content into view, speed-clamped.
    #[gpui::test]
    fn drag_autoscroll_step_only_past_an_edge(cx: &mut gpui::TestAppContext) {
        let editor = cx.new(|cx| CodeEditor::new(cx, "x".into(), Lang::PlainText, ""));
        editor.update(cx, |ed, _| {
            // Viewport x∈[100,500], y∈[100,400]; MARGIN=24, MAX=48.
            ed.viewport = Some(Bounds {
                origin: point(px(100.0), px(100.0)),
                size: gpui::size(px(400.0), px(300.0)),
            });
            // Comfortably inside → no scroll on either axis.
            assert_eq!(
                ed.autoscroll_step(point(px(250.0), px(250.0))),
                Point::default()
            );
            // Past the bottom → content scrolls up (negative y); past the top → positive y.
            assert!(ed.autoscroll_step(point(px(250.0), px(399.0))).y < px(0.0));
            assert!(ed.autoscroll_step(point(px(250.0), px(101.0))).y > px(0.0));
            // Past the right → negative x; past the left → positive x.
            assert!(ed.autoscroll_step(point(px(499.0), px(250.0))).x < px(0.0));
            assert!(ed.autoscroll_step(point(px(101.0), px(250.0))).x > px(0.0));
            // Speed is clamped to MAX even far past the edge.
            assert!(ed.autoscroll_step(point(px(250.0), px(9999.0))).y >= px(-48.0));
        });
    }

    /// Regression for the markdown side-by-side: a content-wide editor (its element is as wide
    /// as the longest line, for horizontal scroll) must NOT squeeze a sibling flex pane to
    /// zero. Mirrors the fixed layout: `flex_1 min_w_0 overflow_hidden` outer wrapping an inner
    /// `overflow_scroll` with very wide content, beside a plain `flex_1` pane.
    #[gpui::test]
    fn side_by_side_panes_both_get_width(cx: &mut gpui::TestAppContext) {
        use gpui::ScrollHandle;
        struct Split {
            left: ScrollHandle,
            right: ScrollHandle,
        }
        impl gpui::Render for Split {
            fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .child(
                        // Editor side: outer flex item + inner wide scroll (the fixed pattern).
                        div().flex_1().min_w_0().overflow_hidden().child(
                            div()
                                .id("L")
                                .overflow_scroll()
                                .size_full()
                                .track_scroll(&self.left)
                                .child(div().w(px(3000.0)).h(px(30.0))),
                        ),
                    )
                    .child(
                        div()
                            .id("R")
                            .overflow_y_scroll()
                            .flex_1()
                            .min_w_0()
                            .track_scroll(&self.right)
                            .child(div().w(px(200.0)).h(px(30.0))),
                    )
            }
        }
        let left = ScrollHandle::new();
        let right = ScrollHandle::new();
        let (l2, r2) = (left.clone(), right.clone());
        let _win = cx.add_window(move |_w, _cx| Split {
            left: l2.clone(),
            right: r2.clone(),
        });
        cx.run_until_parked();
        let lw = f32::from(left.bounds().size.width);
        let rw = f32::from(right.bounds().size.width);
        assert!(
            rw > 100.0 && lw > 100.0,
            "side-by-side panes squeezed: left={lw} right={rw}"
        );
    }

    /// Double-click selects the whole word under the cursor (run of alphanumeric/`_`).
    #[gpui::test]
    fn double_click_selects_word(cx: &mut gpui::TestAppContext) {
        let editor = cx.update(|cx| {
            cx.new(|cx| CodeEditor::new(cx, "foo bar_baz qux".into(), Lang::PlainText, ""))
        });
        editor.update(cx, |ed, cx| {
            // Offset 6 is inside "bar_baz" (chars 4..11).
            ed.select_word_at(6, cx);
            assert_eq!(&ed.content[ed.selected_range.clone()], "bar_baz");
        });
    }

    /// Clicking in the empty area below a short file must drop the caret at the END of the
    /// last line (standard editor behaviour), not leave it where it was. Exercises
    /// `offset_for_position`'s "click below last line" branch directly with a simulated
    /// painted frame (short 3-line file inside a tall viewport).
    #[gpui::test]
    fn click_below_last_line_snaps_to_end_of_content(cx: &mut gpui::TestAppContext) {
        let editor = cx.update(|cx| {
            cx.new(|cx| CodeEditor::new(cx, "a\nbb\nccc".into(), Lang::PlainText, ""))
        });
        editor.update(cx, |ed, _cx| {
            // Simulate the state paint would have left after a frame.
            ed.line_height = px(18.0);
            ed.display_rows = 3;
            ed.gutter_w = px(0.0);
            ed.line_starts = vec![0, 2, 5];
            ed.bounds = Some(gpui::Bounds {
                origin: gpui::point(px(0.0), px(0.0)),
                size: gpui::size(px(500.0), px(600.0)),
            });
            // Click far below the 3 lines of text (~54px tall) in the 600px viewport.
            let off = ed.offset_for_position(gpui::point(px(50.0), px(400.0)));
            assert_eq!(
                off,
                ed.content.len(),
                "caret should land at end of last line"
            );
        });
    }

    /// Perf guard (see CLAUDE.md "Performance regression tests"). `line_runs` runs for every
    /// visible line each frame; it MUST stay sub-linear in the file's total span count, or a
    /// big file (e.g. a 37k-line package-lock with ~200k spans) drops to single-digit FPS —
    /// the exact regression this replaced (`spans.iter().filter(..)` scanned every span).
    /// Simulates a viewport (~60 lines) near the END of a huge file, many frames over.
    #[test]
    fn perf_line_runs_sublinear_in_total_spans() {
        // ~200k spans, 3 per "line", lines ~30 bytes apart.
        let spans: Vec<highlight::Span> = (0..200_000)
            .map(|i| highlight::Span {
                start: i * 10,
                end: i * 10 + 6,
                color: gpui::rgb(0xffffff),
            })
            .collect();
        let total = spans.last().map(|s| s.end).unwrap_or(0);
        let font = gpui::Font {
            family: "monospace".into(),
            features: Default::default(),
            fallbacks: None,
            weight: Default::default(),
            style: Default::default(),
        };
        let line = "x".repeat(30);
        let start = std::time::Instant::now();
        // 60 visible lines near the end, 200 frames.
        for _ in 0..200 {
            for row in 0..60 {
                let ls = total.saturating_sub((60 - row) * 30);
                let _ = line_runs(&line, ls, &spans, &font, gpui::rgb(0).into());
            }
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "line_runs scanned linearly? {elapsed:?} for 200 frames × 60 rows over 200k spans"
        );
    }

    #[test]
    fn hidden_lines_inside_a_fold() {
        // 6 lines; fold region 0..=4 collapsed. Lines 1-4 hide; 0 and 5 stay.
        let foldable = [(0usize, 4usize)];
        let folded = set(&[0]);
        assert!(!is_hidden(&folded, &foldable, 0)); // the start line is visible
        for l in 1..=4 {
            assert!(is_hidden(&folded, &foldable, l), "line {l} should hide");
        }
        assert!(!is_hidden(&folded, &foldable, 5));
        // Not folded → nothing hidden.
        assert!(!is_hidden(&HashSet::new(), &foldable, 3));
    }

    #[test]
    fn visible_indices_skip_folded_body() {
        let content = "a\nb\nc\nd\ne\nf"; // 6 lines (0..5)
        let foldable = [(0usize, 4usize)];
        assert_eq!(
            visible_line_indices(content, &set(&[0]), &foldable),
            vec![0, 5]
        );
        // Unfolded → all lines.
        assert_eq!(
            visible_line_indices(content, &HashSet::new(), &foldable),
            vec![0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn nested_folds_both_collapse() {
        // outer 0..=5, inner 2..=4. Both folded → 1,2,3,4,5 hidden (outer wins),
        // line 0 and 6 visible.
        let foldable = [(0usize, 5usize), (2usize, 4usize)];
        let folded = set(&[0, 2]);
        assert_eq!(
            visible_line_indices("0\n1\n2\n3\n4\n5\n6", &folded, &foldable),
            vec![0, 6]
        );
    }

    #[test]
    fn caret_pulled_out_of_a_collapsed_region() {
        // content: line starts at 0,2,4,6,8,10 (each "x\n"); fold 0..=4 folded.
        let content = "a\nb\nc\nd\ne\nf";
        let foldable = [(0usize, 4usize)];
        let folded = set(&[0]);
        // caret on line 3 (byte 6) is hidden → should move to end of line 0 (byte 1).
        assert_eq!(caret_after_fold(content, &folded, &foldable, 6), Some(1));
        // caret already on the visible start line → no move.
        assert_eq!(caret_after_fold(content, &folded, &foldable, 0), None);
        // caret on visible line 5 → no move.
        assert_eq!(caret_after_fold(content, &folded, &foldable, 10), None);
        // nothing folded → never moves.
        assert_eq!(
            caret_after_fold(content, &HashSet::new(), &foldable, 6),
            None
        );
    }

    #[test]
    fn line_offset_round_trips() {
        let content = "alpha\nbeta\ngamma";
        assert_eq!(line_of_offset(content, 0), 0);
        assert_eq!(line_of_offset(content, 5), 0); // end of "alpha"
        assert_eq!(line_of_offset(content, 6), 1); // start of "beta"
        assert_eq!(line_of_offset(content, 11), 2);
        assert_eq!(line_byte_start(content, 1), 6);
        assert_eq!(line_byte_start(content, 2), 11);
    }

    // ── text-mutation path ────────────────────────────────────────────
    // The OS input handler, undo/redo, and find/replace all funnel through a small set of
    // mutation primitives. These guard their contracts: correct byte/UTF-16 mapping, caret
    // placement, panic-free clamping, the read-only gate, and undo coalescing.

    /// UTF-16 ⇆ UTF-8 offset conversion must round-trip across multi-byte content. The OS
    /// hands the input handler UTF-16 offsets (IME, emoji, accents); a mismatch here would
    /// corrupt every non-ASCII edit. "é" = 2 bytes / 1 UTF-16 unit; "😀" = 4 bytes / 2
    /// UTF-16 units (a surrogate pair).
    #[gpui::test]
    fn utf16_offsets_round_trip_through_multibyte(cx: &mut gpui::TestAppContext) {
        let editor =
            cx.update(|cx| cx.new(|cx| CodeEditor::new(cx, "aé😀b".into(), Lang::PlainText, "")));
        editor.update(cx, |ed, _| {
            // (byte offset, UTF-16 offset) at each char boundary, including end.
            for (u8o, u16o) in [(0, 0), (1, 1), (3, 2), (7, 4), (8, 5)] {
                assert_eq!(ed.off_to_utf16(u8o), u16o, "byte {u8o} → utf16");
                assert_eq!(ed.off_from_utf16(u16o), u8o, "utf16 {u16o} → byte");
            }
        });
    }

    /// `replace_range_text` (find/replace + the `»` revert) replaces a byte range, drops the
    /// caret after the inserted text, and marks the buffer dirty.
    #[gpui::test]
    fn replace_range_text_replaces_and_positions_caret(cx: &mut gpui::TestAppContext) {
        let editor = cx.update(|cx| {
            cx.new(|cx| CodeEditor::new(cx, "hello world".into(), Lang::PlainText, ""))
        });
        editor.update(cx, |ed, cx| {
            ed.replace_range_text(6..11, "there", cx); // "world" → "there"
            assert_eq!(ed.text(), "hello there");
            assert_eq!(ed.selected_range, 11..11);
            assert!(ed.dirty);
        });
    }

    /// Out-of-bounds ranges must clamp, not panic — `replace_range_text` slices `content`,
    /// so an unclamped end past `len()` would be a byte-index panic.
    #[gpui::test]
    fn replace_range_text_clamps_out_of_bounds(cx: &mut gpui::TestAppContext) {
        let editor =
            cx.update(|cx| cx.new(|cx| CodeEditor::new(cx, "abc".into(), Lang::PlainText, "")));
        editor.update(cx, |ed, cx| {
            ed.replace_range_text(2..999, "X", cx); // end past len → clamps to 3
            assert_eq!(ed.text(), "abX");
        });
    }

    /// A read-only editor (the diff base pane) rejects edits and stays clean.
    #[gpui::test]
    fn read_only_rejects_edits(cx: &mut gpui::TestAppContext) {
        let editor = cx
            .update(|cx| cx.new(|cx| CodeEditor::read_only(cx, "frozen".into(), Lang::PlainText)));
        editor.update(cx, |ed, cx| {
            ed.replace_range_text(0..6, "changed", cx);
            assert_eq!(ed.text(), "frozen");
            assert!(!ed.dirty);
        });
    }

    /// Typing through the OS input handler inserts at the caret, and a run of single-char
    /// inserts coalesces into one undo step — so one Undo wipes the whole run (not char by
    /// char), and Redo restores it.
    #[gpui::test]
    fn typing_coalesces_into_one_undo_step(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_w, cx| CodeEditor::new(cx, String::new(), Lang::PlainText, ""));
        win.update(cx, |ed, window, cx| {
            for ch in ["h", "i"] {
                ed.replace_text_in_range(None, ch, window, cx);
            }
            assert_eq!(ed.text(), "hi");
            ed.undo(&Undo, window, cx);
            assert_eq!(ed.text(), "", "one undo wipes the coalesced typing run");
            ed.redo(&Redo, window, cx);
            assert_eq!(ed.text(), "hi");
        })
        .unwrap();
    }

    /// Backspace with no selection deletes the character before the caret.
    #[gpui::test]
    fn backspace_deletes_previous_char(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_w, cx| CodeEditor::new(cx, "abc".into(), Lang::PlainText, ""));
        win.update(cx, |ed, window, cx| {
            ed.selected_range = 3..3; // caret at end
            ed.backspace(&Backspace, window, cx);
            assert_eq!(ed.text(), "ab");
            assert_eq!(ed.selected_range, 2..2);
        })
        .unwrap();
    }

    /// Enter inserts a newline at the caret.
    #[gpui::test]
    fn enter_inserts_newline_at_caret(cx: &mut gpui::TestAppContext) {
        let win = cx.add_window(|_w, cx| CodeEditor::new(cx, "ab".into(), Lang::PlainText, ""));
        win.update(cx, |ed, window, cx| {
            ed.selected_range = 1..1; // between 'a' and 'b'
            ed.enter(&Enter, window, cx);
            assert_eq!(ed.text(), "a\nb");
        })
        .unwrap();
    }
}
