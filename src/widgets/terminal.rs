//! Embedded terminal — a real PTY-backed VTE terminal, gated behind the `terminal`
//! Cargo feature (so the whole module + alacritty's ~2MB of parse tables leave the
//! binary when the feature is off, exactly like a language pack).
//!
//! Architecture (mirrors the `editor` module's entity + custom-Element split):
//!   - `TerminalView` is an Entity owning the alacritty `Term` grid, the PTY notifier,
//!     and the background IO thread's `EventLoop`. It is the focusable widget: typed
//!     text + control keys are translated to bytes and written to the PTY here.
//!   - `EventProxy` is alacritty's `EventListener` — the IO thread calls it on new
//!     output (`Wakeup`), title changes, child exit, etc. It forwards each event over a
//!     `futures` channel to a gpui foreground task, which repaints / writes-back / etc.
//!     (gpui entities aren't `Send`, so the IO thread can't touch them directly.)
//!   - `TerminalElement` is the custom `Element` that locks the grid each frame, shapes
//!     one line per visible row with per-cell fg/bg, and paints the block cursor.
//!
//! The shell itself (zsh/bash) owns command history (Up arrow), tab-completion, line
//! editing — all we do is faithfully relay keystrokes and render the bytes back. So
//! Up-arrow history "just works" once we send the correct `ESC [ A` for the Up key.
use crate::theme;
use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{test::TermSize, Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{
    div, fill, point, prelude::*, px, relative, App, Bounds, Context, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent,
    ShapedLine, Style, TextRun, Window,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Default grid until the first paint measures the real cell size and resizes.
const INIT_COLS: u16 = 80;
const INIT_ROWS: u16 = 24;

/// Forwards alacritty IO-thread events to the gpui foreground over an unbounded channel.
/// `Clone` because both the `Term` and the `EventLoop` need a handle to the same sink.
#[derive(Clone)]
struct EventProxy(UnboundedSender<AlacEvent>);

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        // Best-effort: if the receiver (the view) is gone, the terminal is closing.
        let _ = self.0.unbounded_send(event);
    }
}

/// One terminal tab: the grid + the PTY plumbing + render state.
pub struct TerminalView {
    focus: FocusHandle,
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    /// IO-thread handle, kept for lifetime ownership. On `Drop` we send `Msg::Shutdown`
    /// (see the `Drop` impl) so the thread drains + exits and the PTY/child are torn down.
    _io: std::thread::JoinHandle<(
        EventLoop<tty::Pty, EventProxy>,
        alacritty_terminal::event_loop::State,
    )>,
    /// Window/tab title as set by the shell (OSC 0/2). Falls back to "Local".
    pub title: String,
    /// Child process has exited — the tab shows a closed marker.
    pub exited: bool,
    /// Last grid size pushed to the PTY (cols, rows), so we only resize on a real change.
    last_size: (u16, u16),
    /// Measured cell size in px, cached from the most recent paint (for wheel/mouse math).
    cell: (f32, f32),
    bounds: Option<Bounds<Pixels>>,
    /// True while a left-drag text selection is in progress.
    selecting: bool,
    /// Right-click context menu anchor (window-space), `None` = closed.
    menu_at: Option<gpui::Point<Pixels>>,
}

impl TerminalView {
    pub fn new(working_dir: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<AlacEvent>();
        let proxy = EventProxy(tx);

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let options = PtyOptions {
            shell: Some(Shell::new(shell, Vec::new())),
            working_directory: working_dir,
            drain_on_exit: false,
            env: Default::default(),
            #[cfg(target_os = "windows")]
            escape_args: true,
        };

        let window_size = WindowSize {
            num_lines: INIT_ROWS,
            num_cols: INIT_COLS,
            cell_width: 8,
            cell_height: 16,
        };

        let config = Config::default();
        let term = Term::new(
            config,
            &TermSize::new(INIT_COLS as usize, INIT_ROWS as usize),
            proxy.clone(),
        );
        let term = Arc::new(FairMutex::new(term));

        // SAFETY note: `tty::new` is safe; the PTY fd is owned by the returned Pty.
        let pty = tty::new(&options, window_size, 0).expect("failed to spawn PTY");

        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .expect("failed to create terminal event loop");
        let notifier = Notifier(event_loop.channel());
        let io = event_loop.spawn();

        // Foreground pump: drain IO-thread events into entity updates (repaint, write-back).
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                let alive = this
                    .update(cx, |view, cx| view.on_event(ev, cx))
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();

        Self {
            focus: cx.focus_handle(),
            term,
            notifier,
            _io: io,
            title: "Local".into(),
            exited: false,
            last_size: (INIT_COLS, INIT_ROWS),
            cell: (8.0, 16.0),
            bounds: None,
            selecting: false,
            menu_at: None,
        }
    }

    /// Handle one event from the IO thread. Returns `false` once the pump should stop.
    fn on_event(&mut self, ev: AlacEvent, cx: &mut Context<Self>) -> bool {
        match ev {
            AlacEvent::Wakeup | AlacEvent::MouseCursorDirty => cx.notify(),
            // The emulator wants bytes written back to the PTY (query replies, etc.).
            AlacEvent::PtyWrite(text) => self.notifier.notify(text.into_bytes()),
            AlacEvent::Title(t) => {
                self.title = t;
                cx.emit(TerminalEvent::TitleChanged);
                cx.notify();
            }
            AlacEvent::ResetTitle => {
                self.title = "Local".into();
                cx.emit(TerminalEvent::TitleChanged);
                cx.notify();
            }
            AlacEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
            AlacEvent::ClipboardLoad(_, format) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    self.notifier.notify(format(&text).into_bytes());
                }
            }
            AlacEvent::ChildExit(_) | AlacEvent::Exit => {
                // Shell exited (e.g. `exit` / ^D) → close the tab, like hitting its ×. Kyde's
                // subscription removes it and hides the panel when it was the last tab.
                self.exited = true;
                cx.emit(TerminalEvent::CloseRequested);
                cx.notify();
                return false;
            }
            AlacEvent::Bell | AlacEvent::CursorBlinkingChange | AlacEvent::ColorRequest(..) => {}
            AlacEvent::TextAreaSizeRequest(format) => {
                let (cols, rows) = self.last_size;
                let ws = WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: self.cell.0 as u16,
                    cell_height: self.cell.1 as u16,
                };
                self.notifier.notify(format(ws).into_bytes());
            }
        }
        true
    }

    /// The widget's focus handle (so the panel can focus the active tab).
    pub fn handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Write raw bytes to the PTY (typed text, pasted text, key escape sequences).
    fn write<B: Into<std::borrow::Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
    }

    /// Feed a string to the shell as if typed (used by the screenshot harness to seed a
    /// realistic session). The PTY buffers it until the shell is ready, so it's race-free.
    pub fn send_input(&self, text: &str) {
        self.write(text.as_bytes().to_vec());
    }

    /// Re-size the grid + PTY when the panel geometry changes. No-op if unchanged.
    fn resize(&mut self, cols: u16, rows: u16, cell_w: f32, cell_h: f32) {
        self.cell = (cell_w, cell_h);
        if (cols, rows) == self.last_size || cols == 0 || rows == 0 {
            return;
        }
        self.last_size = (cols, rows);
        self.term
            .lock()
            .resize(TermSize::new(cols as usize, rows as usize));
        self.notifier.on_resize(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_w as u16,
            cell_height: cell_h as u16,
        });
    }

    /// Translate a key event to PTY bytes. Returns whether it was handled.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();

        // Cmd-V paste / Cmd-C copy-selection (Ctrl-C still sends SIGINT, handled below).
        if m.platform && key == "v" {
            self.paste(cx);
            return;
        }
        if m.platform && key == "c" {
            self.copy_selection(cx);
            return;
        }
        // Any other ⌘-combo (⌘T new tab, ⌘W, …) is an app shortcut, never PTY input — let it
        // bubble to the action system instead of typing a stray character.
        if m.platform {
            return;
        }

        // Ctrl + letter → control byte (Ctrl-C = 0x03, etc.). The shell + foreground
        // program rely on these (SIGINT, EOF, …).
        if m.control && key.len() == 1 {
            if let Some(c) = key.chars().next() {
                let lc = c.to_ascii_lowercase();
                if lc.is_ascii_alphabetic() {
                    self.write(vec![(lc as u8 - b'a') + 1]);
                    cx.notify();
                    return;
                }
            }
        }

        let bytes: Option<Vec<u8>> = match key {
            "enter" => Some(b"\r".to_vec()),
            "backspace" => Some(vec![0x7f]),
            "tab" => Some(b"\t".to_vec()),
            "escape" => Some(vec![0x1b]),
            // Arrows drive shell history (Up/Down) + line editing (Left/Right).
            "up" => Some(b"\x1b[A".to_vec()),
            "down" => Some(b"\x1b[B".to_vec()),
            "right" => Some(b"\x1b[C".to_vec()),
            "left" => Some(b"\x1b[D".to_vec()),
            "home" => Some(b"\x1b[H".to_vec()),
            "end" => Some(b"\x1b[F".to_vec()),
            "delete" => Some(b"\x1b[3~".to_vec()),
            "pageup" => Some(b"\x1b[5~".to_vec()),
            "pagedown" => Some(b"\x1b[6~".to_vec()),
            _ => ks.key_char.as_ref().map(|s| s.clone().into_bytes()),
        };

        if let Some(bytes) = bytes {
            if !bytes.is_empty() {
                // Any keystroke jumps back to the live screen (out of scrollback).
                self.term.lock().scroll_display(Scroll::Bottom);
                self.write(bytes);
                cx.notify();
            }
        }
    }

    /// Paste clipboard text, honouring bracketed-paste mode when the program enabled it.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.write(b"\x1b[200~".to_vec());
            self.write(text.into_bytes());
            self.write(b"\x1b[201~".to_vec());
        } else {
            self.write(text.replace('\n', "\r").into_bytes());
        }
        cx.notify();
    }

    /// Clear the terminal (⌘K): drop the scrollback buffer, then send the shell Ctrl-L so it
    /// wipes the visible screen and redraws its prompt at the top — matching iTerm's "clear".
    fn clear(&mut self, cx: &mut Context<Self>) {
        {
            let mut t = self.term.lock();
            t.grid_mut().clear_history();
            t.scroll_display(Scroll::Bottom);
        }
        self.write(b"\x0c".to_vec());
        cx.notify();
    }

    /// Scroll the scrollback buffer by a number of lines (positive = up/back in history).
    fn scroll_lines(&mut self, lines: i32, cx: &mut Context<Self>) {
        if lines != 0 {
            self.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Map a window-space cursor position to a grid point + which half of the cell it's in.
    /// Accounts for the current scrollback offset so a selection in history stays anchored.
    fn point_at(&self, pos: gpui::Point<Pixels>) -> Option<(GridPoint, Side)> {
        let b = self.bounds?;
        let (cw, ch) = self.cell;
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let lx = f32::from(pos.x - b.left());
        let ly = f32::from(pos.y - b.top());
        let t = self.term.lock();
        let cols = t.columns() as i32;
        let off = t.grid().display_offset() as i32;
        drop(t);
        let col_f = (lx / cw).floor();
        let row = (ly / ch).floor().max(0.0) as i32;
        let col = (col_f as i32).clamp(0, (cols - 1).max(0)) as usize;
        let side = if lx - col_f * cw < cw / 2.0 {
            Side::Left
        } else {
            Side::Right
        };
        Some((GridPoint::new(Line(row - off), Column(col)), side))
    }

    /// Begin a text selection (Simple drag / Semantic double-click / Lines triple-click).
    fn start_selection(
        &mut self,
        pos: gpui::Point<Pixels>,
        ty: SelectionType,
        cx: &mut Context<Self>,
    ) {
        if let Some((p, side)) = self.point_at(pos) {
            self.term.lock().selection = Some(Selection::new(ty, p, side));
            self.selecting = true;
            cx.notify();
        }
    }

    /// Extend the in-progress selection to the current cursor position.
    fn update_selection(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if let Some((p, side)) = self.point_at(pos) {
            let mut t = self.term.lock();
            if let Some(sel) = t.selection.as_mut() {
                sel.update(p, side);
                drop(t);
                cx.notify();
            }
        }
    }

    /// Copy the current selection to the clipboard (Cmd-C). No-op with no selection.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            if !text.is_empty() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
        }
    }

    /// Cmd-click: open the URL under the cursor — an OSC-8 hyperlink if the cell carries
    /// one, else a plain http(s) token scanned out of that grid row.
    fn open_url_at(&mut self, pos: gpui::Point<Pixels>) {
        let Some((p, _)) = self.point_at(pos) else {
            return;
        };
        let t = self.term.lock();
        if let Some(h) = t.grid()[p].hyperlink() {
            let uri = h.uri().to_string();
            drop(t);
            open_uri(&uri);
            return;
        }
        let cols = t.columns();
        let line: String = (0..cols)
            .map(|c| t.grid()[GridPoint::new(p.line, Column(c))].c)
            .collect();
        drop(t);
        if let Some(url) = url_at(&line, p.column.0) {
            open_uri(&url);
        }
    }
}

/// Find an http(s) URL token in `line` that covers char column `col`. Trailing sentence
/// punctuation is trimmed so "see https://x.com." doesn't capture the period.
fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        let trimmed = token.trim_end_matches(['.', ',', ')', ']', '}', '"', '\'', '>']);
        if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && col >= start
            && col < start + trimmed.chars().count()
        {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Open a URI in the OS default handler.
fn open_uri(uri: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(uri).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = std::process::Command::new("xdg-open").arg(uri).spawn();
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // Closing a tab drops this entity. Tell the IO thread to shut down so it drains +
        // exits, closes the PTY (the child shell gets SIGHUP), and frees the grid/scrollback.
        // Without this the thread keeps running detached — leaking MBs of grid + a zombie shell.
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// Emitted so the panel (Kyde) can react: repaint the tab label on a title change, or close
/// this tab when the terminal's own context menu requests it (tabs live on `Kyde`).
pub enum TerminalEvent {
    TitleChanged,
    CloseRequested,
}
impl EventEmitter<TerminalEvent> for TerminalView {}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let menu_at = self.menu_at;
        div()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .relative()
            .size_full()
            .bg(theme::get().main_bg)
            // ⌘K clears the terminal — overrides the global commit binding in this context.
            .on_action(cx.listener(|this, _: &crate::ClearTerminal, _w, cx| this.clear(cx)))
            // Pin the mono font + editor size so the cell metrics the element measures (the
            // `M` advance) match the glyphs it actually shapes — otherwise the cursor, placed
            // at `col × cell_w`, drifts off the text (it inherits a proportional UI font).
            .font_family(theme::font::FAMILY)
            .text_size(px(theme::get().editor_font_size))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    // Cmd-click opens a URL under the cursor; a plain click starts a
                    // selection (double = word, triple = line).
                    if e.modifiers.platform {
                        this.open_url_at(e.position);
                        return;
                    }
                    let ty = match e.click_count {
                        2 => SelectionType::Semantic,
                        n if n >= 3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    };
                    this.start_selection(e.position, ty, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _w, cx| {
                if this.selecting {
                    this.update_selection(e.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseUpEvent, _w, _cx| {
                    this.selecting = false;
                }),
            )
            // Right-click → context menu (Paste / Clear / Close) at the cursor.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.menu_at = Some(e.position);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _w, cx| this.on_key(e, cx)))
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _w, cx| {
                // 3 lines per wheel notch, matching the editor panes' feel.
                let dy = e.delta.pixel_delta(px(this.cell.1.max(1.0))).y;
                let lines = (f32::from(dy) / this.cell.1.max(1.0)).round() as i32;
                this.scroll_lines(lines, cx);
            }))
            .child(TerminalElement { view })
            .children(menu_at.map(|at| self.render_context_menu(at, cx)))
    }
}

impl TerminalView {
    /// Right-click menu: a transparent dismiss backdrop + a small box anchored at the click.
    fn render_context_menu(
        &self,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme::get();
        let item = |label: &'static str| {
            div()
                .id(label)
                .px_3()
                .py_1()
                .text_size(px(13.0))
                .text_color(t.text)
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(t.selected_bg))
                .child(label)
        };
        // Full-window backdrop: any click closes the menu (and doesn't fall through).
        div()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.menu_at = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _e, _w, cx| {
                    this.menu_at = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left(at.x)
                    .top(at.y)
                    .flex()
                    .flex_col()
                    .min_w(px(160.0))
                    .p_1()
                    .bg(t.panel_bg)
                    .border_1()
                    .border_color(t.divider)
                    .rounded_md()
                    .font_family(theme::font::UI_FAMILY)
                    .child(item("Paste").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.menu_at = None;
                            this.paste(cx);
                        }),
                    ))
                    .child(item("Clear").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.menu_at = None;
                            this.clear(cx);
                        }),
                    ))
                    .child(item("Close").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.menu_at = None;
                            // Tabs live on Kyde — ask it to close this one.
                            cx.emit(TerminalEvent::CloseRequested);
                        }),
                    )),
            )
    }
}

// ── the painting element ──────────────────────────────────────────
struct TerminalElement {
    view: Entity<TerminalView>,
}

struct Prepaint {
    /// One shaped line per visible grid row (dense, top→bottom).
    lines: Vec<ShapedLine>,
    /// Background quads (cell rects with a non-default bg), painted under the text.
    bg_quads: Vec<gpui::PaintQuad>,
    /// Block/bar cursor quad, if visible (not in scrollback, not hidden).
    cursor: Option<gpui::PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let style = window.text_style();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        // Monospace cell width = advance of a representative glyph.
        let cell_w = window
            .text_system()
            .shape_line(
                "M".into(),
                font_size,
                &[TextRun {
                    len: 1,
                    font: font.clone(),
                    color: theme::get().text.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width;
        let cell_w_f = f32::from(cell_w).max(1.0);
        let cell_h_f = f32::from(line_height).max(1.0);

        let cols = (f32::from(bounds.size.width) / cell_w_f).floor().max(1.0) as u16;
        let rows = (f32::from(bounds.size.height) / cell_h_f).floor().max(1.0) as u16;

        // Push geometry changes back to the grid/PTY before reading the content.
        self.view
            .update(cx, |v, _| v.resize(cols, rows, cell_w_f, cell_h_f));

        let view = self.view.read(cx);
        let term = view.term.lock();
        let content = term.renderable_content();
        let default_fg: Hsla = theme::get().text.into();
        let default_bg: Hsla = theme::get().main_bg.into();

        let selection = content.selection;
        let sel_bg: Hsla = theme::get().selected_bg.into();
        let mut lines: Vec<ShapedLine> = Vec::new();
        let mut bg_quads: Vec<gpui::PaintQuad> = Vec::new();
        let mut base_line: Option<i32> = None;
        let mut cur_row: Vec<(char, Hsla)> = Vec::new();
        let mut cur_line_idx: i32 = i32::MIN;

        let flush_row = |lines: &mut Vec<ShapedLine>, row: &mut Vec<(char, Hsla)>| {
            // Coalesce a row into one shaped line with a run per color change.
            let text: String = row.iter().map(|(c, _)| *c).collect();
            let mut runs: Vec<TextRun> = Vec::new();
            for (c, color) in row.iter() {
                let len = c.len_utf8();
                if let Some(last) = runs.last_mut() {
                    if last.color == *color {
                        last.len += len;
                        continue;
                    }
                }
                runs.push(TextRun {
                    len,
                    font: font.clone(),
                    color: *color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            lines.push(
                window
                    .text_system()
                    .shape_line(text.into(), font_size, &runs, None),
            );
            row.clear();
        };

        for cell in content.display_iter {
            let line = cell.point.line.0;
            let col = cell.point.column.0;
            if base_line.is_none() {
                base_line = Some(line);
                cur_line_idx = line;
            }
            // New grid row → flush the accumulated one.
            if line != cur_line_idx {
                flush_row(&mut lines, &mut cur_row);
                cur_line_idx = line;
            }
            // Second half of a wide char is a spacer — skip (the wide glyph already shaped).
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let mut fg = resolve(cell.fg, default_fg, content.colors);
            let mut bg = resolve_opt(cell.bg, content.colors);
            if cell.flags.contains(Flags::INVERSE) {
                let f = fg;
                fg = bg.unwrap_or(default_bg);
                bg = Some(f);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg.unwrap_or(default_bg);
            }
            let row_idx = (line - base_line.unwrap_or(line)) as f32;
            // Selection tint wins over the cell's own bg (pushed last → painted on top).
            let selected = selection.as_ref().is_some_and(|s| s.contains(cell.point));
            if let Some(bg) = bg {
                let x = bounds.left() + px(col as f32 * cell_w_f);
                let y = bounds.top() + line_height * row_idx;
                bg_quads.push(fill(
                    Bounds::new(point(x, y), gpui::size(px(cell_w_f), line_height)),
                    bg,
                ));
            }
            if selected {
                let x = bounds.left() + px(col as f32 * cell_w_f);
                let y = bounds.top() + line_height * row_idx;
                bg_quads.push(fill(
                    Bounds::new(point(x, y), gpui::size(px(cell_w_f), line_height)),
                    sel_bg,
                ));
            }
            let ch = cell.c;
            cur_row.push((if ch == '\0' { ' ' } else { ch }, fg));
        }
        flush_row(&mut lines, &mut cur_row);

        // Block cursor — only on the live screen (no scrollback offset) and when shown.
        let cursor = if content.display_offset == 0 && content.cursor.shape != CursorShape::Hidden {
            let base = base_line.unwrap_or(0);
            let row = content.cursor.point.line.0 - base;
            let col = content.cursor.point.column.0;
            if row >= 0 && (row as u16) < rows {
                let x = bounds.left() + px(col as f32 * cell_w_f);
                let y = bounds.top() + line_height * row as f32;
                let focused = view.focus.is_focused(window);
                let color: Hsla = theme::get().primary.into();
                let rect = match content.cursor.shape {
                    CursorShape::Beam => Bounds::new(point(x, y), gpui::size(px(2.0), line_height)),
                    CursorShape::Underline => Bounds::new(
                        point(x, y + line_height - px(2.0)),
                        gpui::size(px(cell_w_f), px(2.0)),
                    ),
                    // Block: filled when focused, hollow-ish (thin) when not.
                    _ if focused => Bounds::new(point(x, y), gpui::size(px(cell_w_f), line_height)),
                    _ => Bounds::new(point(x, y), gpui::size(px(cell_w_f), px(2.0))),
                };
                Some(fill(rect, color))
            } else {
                None
            }
        } else {
            None
        };

        drop(term);
        Prepaint {
            lines,
            bg_quads,
            cursor,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        for q in pre.bg_quads.drain(..) {
            window.paint_quad(q);
        }
        // Cursor under the glyph so an inverse block still shows the character.
        if let Some(c) = pre.cursor.take() {
            window.paint_quad(c);
        }
        let lh = pre.line_height;
        for (i, line) in pre.lines.iter().enumerate() {
            let origin = point(bounds.left(), bounds.top() + lh * i as f32);
            let _ = line.paint(origin, lh, window, cx);
        }
        self.view.update(cx, |v, _| v.bounds = Some(bounds));
    }
}

/// Resolve an alacritty cell color to an `Hsla`, falling back to `default` for the
/// default-foreground sentinel.
fn resolve(
    color: AnsiColor,
    default: Hsla,
    colors: &alacritty_terminal::term::color::Colors,
) -> Hsla {
    match color {
        AnsiColor::Named(NamedColor::Foreground) => default,
        AnsiColor::Named(n) => named_rgb(n, colors).map(rgb_to_hsla).unwrap_or(default),
        AnsiColor::Spec(rgb) => rgb_to_hsla(rgb),
        AnsiColor::Indexed(i) => indexed_rgb(i, colors).map(rgb_to_hsla).unwrap_or(default),
    }
}

/// Like `resolve` but returns `None` for the default background (so it isn't painted).
fn resolve_opt(color: AnsiColor, colors: &alacritty_terminal::term::color::Colors) -> Option<Hsla> {
    match color {
        AnsiColor::Named(NamedColor::Background) => None,
        AnsiColor::Named(n) => named_rgb(n, colors).map(rgb_to_hsla),
        AnsiColor::Spec(rgb) => Some(rgb_to_hsla(rgb)),
        AnsiColor::Indexed(i) => indexed_rgb(i, colors).map(rgb_to_hsla),
    }
}

fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    gpui::rgb(((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32).into()
}

/// Named ANSI color → RGB: prefer an OSC-overridden value, else the builtin palette.
fn named_rgb(n: NamedColor, colors: &alacritty_terminal::term::color::Colors) -> Option<Rgb> {
    if let Some(c) = colors[n] {
        return Some(c);
    }
    let idx = n as usize;
    if idx < 16 {
        Some(ANSI_PALETTE[idx])
    } else {
        None
    }
}

fn indexed_rgb(i: u8, colors: &alacritty_terminal::term::color::Colors) -> Option<Rgb> {
    if let Some(c) = colors[i as usize] {
        return Some(c);
    }
    Some(default_indexed(i))
}

/// The standard 256-color cube + grayscale ramp for indices the program hasn't overridden.
fn default_indexed(i: u8) -> Rgb {
    match i {
        0..=15 => ANSI_PALETTE[i as usize],
        16..=231 => {
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb {
                r: step(r),
                g: step(g),
                b: step(b),
            }
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}

/// 16-color ANSI palette (a balanced dark-theme set tuned to match Kyde's chrome).
const ANSI_PALETTE: [Rgb; 16] = [
    Rgb {
        r: 0x1e,
        g: 0x20,
        b: 0x24,
    }, // black
    Rgb {
        r: 0xe0,
        g: 0x6c,
        b: 0x75,
    }, // red
    Rgb {
        r: 0x98,
        g: 0xc3,
        b: 0x79,
    }, // green
    Rgb {
        r: 0xe5,
        g: 0xc0,
        b: 0x7b,
    }, // yellow
    Rgb {
        r: 0x61,
        g: 0xaf,
        b: 0xef,
    }, // blue
    Rgb {
        r: 0xc6,
        g: 0x78,
        b: 0xdd,
    }, // magenta
    Rgb {
        r: 0x56,
        g: 0xb6,
        b: 0xc2,
    }, // cyan
    Rgb {
        r: 0xab,
        g: 0xb2,
        b: 0xbf,
    }, // white
    Rgb {
        r: 0x5c,
        g: 0x63,
        b: 0x70,
    }, // bright black
    Rgb {
        r: 0xe0,
        g: 0x6c,
        b: 0x75,
    }, // bright red
    Rgb {
        r: 0x98,
        g: 0xc3,
        b: 0x79,
    }, // bright green
    Rgb {
        r: 0xe5,
        g: 0xc0,
        b: 0x7b,
    }, // bright yellow
    Rgb {
        r: 0x61,
        g: 0xaf,
        b: 0xef,
    }, // bright blue
    Rgb {
        r: 0xc6,
        g: 0x78,
        b: 0xdd,
    }, // bright magenta
    Rgb {
        r: 0x56,
        g: 0xb6,
        b: 0xc2,
    }, // bright cyan
    Rgb {
        r: 0xff,
        g: 0xff,
        b: 0xff,
    }, // bright white
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_palette_covers_all_256_without_panicking() {
        // The 256-color resolver is hit per visible cell; make sure every index maps to a
        // concrete RGB across the three regions (16 ANSI / 216 cube / 24 grayscale) with no
        // arithmetic panic (the cube/ramp math casts u8s).
        for i in 0u8..=255 {
            let _ = default_indexed(i);
        }
        // Cube corners: index 16 = origin (black), 231 = white max.
        assert_eq!(default_indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            default_indexed(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        // Grayscale ramp is monotonic.
        assert!(default_indexed(232).r < default_indexed(255).r);
    }

    #[test]
    fn url_at_finds_token_under_column_and_trims_punctuation() {
        let line = "see https://example.com/path. and clang++ next";
        // Column inside the URL → returns it, trailing period trimmed.
        assert_eq!(
            url_at(line, 10).as_deref(),
            Some("https://example.com/path")
        );
        // Column on a non-URL word → None.
        assert_eq!(url_at(line, 0), None);
        assert_eq!(url_at(line, 35), None);
        // No URL at all → None.
        assert_eq!(url_at("plain text only", 3), None);
    }

    #[test]
    fn named_foreground_uses_caller_default() {
        let colors = alacritty_terminal::term::color::Colors::default();
        let def: Hsla = gpui::rgb(0x123456).into();
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Foreground), def, &colors),
            def
        );
        // Default background resolves to None (not painted).
        assert!(resolve_opt(AnsiColor::Named(NamedColor::Background), &colors).is_none());
    }
}
