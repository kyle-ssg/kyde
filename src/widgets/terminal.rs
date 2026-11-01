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
use crate::editor::{CodeEditor, EditorEvent};
use crate::theme;
use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{test::TermSize, Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{
    div, fill, point, prelude::*, px, relative, svg, App, Bounds, Context, DispatchPhase,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyDownEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent,
    ShapedLine, Style, TextRun, Window,
};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default grid until the first paint measures the real cell size and resizes.
const INIT_COLS: u16 = 80;
const INIT_ROWS: u16 = 24;

/// Scrollbar gutter width, thumb thickness and end inset — the editor panes' (`with_scrollbars`).
const SB_BAR: f32 = 12.0;
const SB_THUMB: f32 = 6.0;
const SB_END: f32 = 8.0;

/// How often an open find re-counts while output streams in.
const FIND_RESCAN: Duration = Duration::from_millis(500);

/// Opacity of SGR-dim (faint) text, e.g. Claude's greyed prompt suggestion.
const DIM_ALPHA: f32 = 0.5;

#[cfg(target_os = "windows")]
fn default_shell() -> String {
    windows_shell_from_comspec(std::env::var("COMSPEC").ok())
}

#[cfg(target_os = "windows")]
fn windows_shell_from_comspec(comspec: Option<String>) -> String {
    comspec
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "cmd.exe".to_string())
}

#[cfg(target_os = "macos")]
pub(crate) fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn default_shell() -> String {
    shell_from(std::env::var("SHELL").ok(), passwd_shell)
}

/// Off macOS: `$SHELL`, else the login shell in the user database, else `/bin/sh`. A launch
/// from a desktop entry can arrive without `SHELL`, and zsh isn't installed on most Linux
/// systems (Ubuntu ships bash), so `/bin/zsh` would leave a dead tab. Pure; `passwd` is the
/// user-database lookup.
#[cfg(all(unix, not(target_os = "macos")))]
fn shell_from(env: Option<String>, passwd: impl FnOnce() -> Option<String>) -> String {
    env.filter(|s| !s.trim().is_empty())
        .or_else(passwd)
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// The current user's login shell from the user database (`/etc/passwd`, LDAP, …).
#[cfg(all(unix, not(target_os = "macos")))]
fn passwd_shell() -> Option<String> {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `entry`, `buf` and `found` are live, writable and passed with `buf`'s real
    // length; getpwuid_r fills `entry` (its strings point into `buf`) and sets `found` to
    // `entry` on success, or leaves it null when there is no entry or on error.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            entry.as_mut_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut found,
        )
    };
    if rc != 0 || found.is_null() {
        return None;
    }
    // SAFETY: `found` is non-null, so `entry` was filled; `pw_shell` is null or a
    // NUL-terminated string inside `buf`, which outlives this read.
    let shell = unsafe {
        let p = (*found).pw_shell;
        if p.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(p)
            .to_str()
            .ok()?
            .trim()
            .to_string()
    };
    (!shell.is_empty()).then_some(shell)
}

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

/// Where a terminal's bytes go and come from.
enum Backend {
    /// Its own PTY, driven by alacritty's IO thread.
    Local {
        notifier: Notifier,
        /// IO-thread handle, kept for lifetime ownership. On `Drop` we send `Msg::Shutdown`
        /// so the thread drains + exits and the PTY/child are torn down.
        _io: std::thread::JoinHandle<(
            EventLoop<tty::Pty, EventProxy>,
            alacritty_terminal::event_loop::State,
        )>,
    },
    /// An agent host's PTY (see `agent_host`), over its socket — it outlives this view.
    #[cfg(unix)]
    Host {
        stream: std::sync::Mutex<std::os::unix::net::UnixStream>,
    },
}

impl Backend {
    fn write(&self, bytes: std::borrow::Cow<'static, [u8]>) {
        match self {
            Self::Local { notifier, .. } => notifier.notify(bytes),
            #[cfg(unix)]
            Self::Host { stream, .. } => {
                let mut s = stream
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = crate::agent_host::write_frame(&mut *s, crate::agent_host::INPUT, &bytes);
            }
        }
    }

    fn resize(&mut self, ws: WindowSize) {
        match self {
            Self::Local { notifier, .. } => notifier.on_resize(ws),
            #[cfg(unix)]
            Self::Host { stream, .. } => {
                let s = stream
                    .get_mut()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = crate::agent_host::write_frame(
                    s,
                    crate::agent_host::RESIZE,
                    &crate::agent_host::resize_payload(ws),
                );
            }
        }
    }
}

/// One terminal tab: the grid + the PTY plumbing + render state.
pub struct TerminalView {
    focus: FocusHandle,
    term: Arc<FairMutex<Term<EventProxy>>>,
    backend: Backend,
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
    /// Rows the content is pushed down by bottom-anchoring (see `prepaint`), so mouse
    /// positions map back onto the grid.
    shift: u16,
    /// Bottom-anchor content that hasn't filled the pane yet (agent tabs — their input box
    /// sits at the bottom). Off for the IDE's bottom terminal panel.
    pub anchor_bottom: bool,
    /// The shell's process id — what runs under it (an agent) can be probed. Shared: a host
    /// reports it in its first frame.
    pid: Arc<std::sync::atomic::AtomicU32>,
    /// When you last typed or pasted into it.
    pub last_input: Option<std::time::Instant>,
    /// Claude Code in it has finished its turn (Kyde's hook says so; see `set_claude_done`) —
    /// the only time it shows a prompt suggestion, which then acts as typed text: the cursor
    /// is drawn at its end, and typing there accepts it first (`accept_suggestion`). Its
    /// other greyed hints in the box come while it starts up or works.
    claude_done: bool,
    /// The suggestion last accepted or cleared (Backspace) for you. Claude greys it back in
    /// whenever the box is emptied; that time it's left out, so the box shows empty.
    taken_suggestion: Option<String>,
    /// Input held back while Claude takes in an accepted suggestion, then written in order.
    held: Option<Vec<u8>>,
    /// Wheel pixels short of a whole line, carried to the next event (a trackpad sends a few
    /// px at a time; rounding each event alone dropped every one under half a line).
    scroll_px: f32,
    /// A scrollbar thumb drag in progress.
    sb_drag: Option<SbDrag>,
    /// Find (⌘F), built the first time it opens.
    find: Option<TermFind>,
}

/// A scrollbar drag: the cursor y it started at, and how many lines sat above the view then.
#[derive(Clone, Copy)]
struct SbDrag {
    start_y: f32,
    start_top: f32,
}

/// The scrollbar as drawn: thumb length and top in its track, lines of scrollback, and how
/// many of them sit above the view.
struct Sb {
    track: f32,
    len: f32,
    pos: f32,
    hist: usize,
    top: usize,
}

/// Terminal find: the query box and the match it's on.
struct TermFind {
    open: bool,
    query: Entity<CodeEditor>,
    _query_sub: gpui::Subscription,
    /// The query, compiled (`None` while empty).
    regex: Option<RegexSearch>,
    /// The current match where it was found, with the scrollback size and scroll offset then
    /// (the offset adjusted for our own scrolling since) — see [`shifted`].
    cur: Option<(Match, usize, isize)>,
    /// Matches in the whole buffer at the last scan, and which is current (1 = newest; 0 =
    /// none).
    count: usize,
    index: usize,
    scanned: Option<Instant>,
    /// The re-count held back by [`FIND_RESCAN`], so one still follows the last output.
    rescan: Option<gpui::Task<()>>,
}

/// Which match a find scan lands on.
#[derive(Clone, Copy)]
enum Pick {
    /// The query changed: the newest match at or above the bottom of the view.
    Fresh,
    /// Toward the top of the scrollback (Enter, ⌘G).
    Older,
    /// Toward the live screen (⇧Enter, ⇧⌘G).
    Newer,
    /// Just re-count, staying on the current match.
    Keep,
}

impl TerminalView {
    pub fn new(working_dir: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self::with_env(working_dir, std::collections::HashMap::new(), cx)
    }

    /// A terminal whose shell gets `env` on top of Kyde's own environment.
    pub fn with_env(
        working_dir: Option<PathBuf>,
        env: std::collections::HashMap<String, String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = unbounded::<AlacEvent>();
        let proxy = EventProxy(tx);

        let shell = default_shell();
        let options = PtyOptions {
            shell: Some(Shell::new(shell, Vec::new())),
            working_directory: working_dir,
            drain_on_exit: false,
            env,
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

        // Blessed expects: spawning the PTY + its event loop can only fail on a fatal OS
        // condition (exhausted fds / no pty device). The terminal can't exist without them and
        // this constructor returns `Self`, not `Result`, so a clear panic is the right call.
        // (Promoting the whole terminal-spawn path to `Result` is a tracked scaffold follow-up.)
        // `tty::new` is safe; the PTY fd is owned by the returned Pty.
        #[allow(clippy::expect_used)]
        let pty = tty::new(&options, window_size, 0).expect("failed to spawn PTY");
        let pid = Arc::new(std::sync::atomic::AtomicU32::new(pty.child().id()));

        #[allow(clippy::expect_used)]
        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .expect("failed to create terminal event loop");
        let notifier = Notifier(event_loop.channel());
        let io = event_loop.spawn();
        Self::pump_events(rx, cx);
        Self::build(term, Backend::Local { notifier, _io: io }, pid, cx)
    }

    /// Connect to agent host `id` as a viewer (for [`Self::attach`]). Fails when the host
    /// isn't running.
    #[cfg(unix)]
    pub fn connect_host(
        id: &str,
    ) -> std::io::Result<(
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    )> {
        use crate::agent_host as host;
        let mut stream = std::os::unix::net::UnixStream::connect(host::sock_path(id))?;
        host::write_frame(&mut stream, host::ATTACH, &[])?;
        let reader = stream.try_clone()?;
        Ok((stream, reader))
    }

    /// A view on an agent host's terminal (see `agent_host`), over a [`Self::connect_host`]
    /// connection: its recent output is replayed, then it streams live.
    #[cfg(unix)]
    pub fn attach(
        (stream, mut reader): (
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
        ),
        cx: &mut Context<Self>,
    ) -> Self {
        use crate::agent_host as host;
        let (tx, rx) = unbounded::<AlacEvent>();
        let proxy = EventProxy(tx);
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(INIT_COLS as usize, INIT_ROWS as usize),
            proxy.clone(),
        )));
        let pid = Arc::new(std::sync::atomic::AtomicU32::new(0));
        {
            let (term, pid) = (term.clone(), pid.clone());
            std::thread::spawn(move || {
                let mut parser: alacritty_terminal::vte::ansi::Processor<
                    alacritty_terminal::vte::ansi::StdSyncHandler,
                > = alacritty_terminal::vte::ansi::Processor::new();
                let mut frames = host::FrameReader::default();
                loop {
                    // A synchronized update left open is flushed at its deadline.
                    let wait = parser.sync_timeout().sync_timeout().map(|d| {
                        d.saturating_duration_since(std::time::Instant::now())
                            .max(std::time::Duration::from_millis(1))
                    });
                    let _ = reader.set_read_timeout(wait);
                    match frames.next(&mut reader) {
                        Ok(Some((host::HELLO, json))) => {
                            if let Ok(h) = serde_json::from_slice::<host::HostHello>(&json) {
                                pid.store(h.pid, std::sync::atomic::Ordering::Relaxed);
                                // Replay at the size it was drawn at.
                                term.lock()
                                    .resize(TermSize::new(h.cols as usize, h.rows as usize));
                            }
                        }
                        Ok(Some((host::OUT, bytes))) => {
                            parser.advance(&mut *term.lock(), &bytes);
                            proxy.send_event(AlacEvent::Wakeup);
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            parser.stop_sync(&mut *term.lock());
                            proxy.send_event(AlacEvent::Wakeup);
                        }
                        // EXIT, or the host went away: the tab closes.
                        Err(_) => {
                            proxy.send_event(AlacEvent::Exit);
                            break;
                        }
                    }
                }
            });
        }
        Self::pump_events(rx, cx);
        Self::build(
            term,
            Backend::Host {
                stream: std::sync::Mutex::new(stream),
            },
            pid,
            cx,
        )
    }

    /// Foreground pump: drain IO-thread events into entity updates (repaint, write-back).
    fn pump_events(
        mut rx: futures::channel::mpsc::UnboundedReceiver<AlacEvent>,
        cx: &mut Context<Self>,
    ) {
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
    }

    fn build(
        term: Arc<FairMutex<Term<EventProxy>>>,
        backend: Backend,
        pid: Arc<std::sync::atomic::AtomicU32>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            term,
            backend,
            title: "Local".into(),
            exited: false,
            last_size: (INIT_COLS, INIT_ROWS),
            cell: (8.0, 16.0),
            bounds: None,
            selecting: false,
            menu_at: None,
            shift: 0,
            anchor_bottom: false,
            pid,
            last_input: None,
            claude_done: false,
            taken_suggestion: None,
            held: None,
            scroll_px: 0.0,
            sb_drag: None,
            find: None,
        }
    }

    /// The shell's process id (0 until a host has reported it).
    pub fn shell_pid(&self) -> u32 {
        self.pid.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// End a hosted terminal's agent for good (closing its tab). A local terminal ends when
    /// the view drops.
    pub fn kill_host(&self) {
        #[cfg(unix)]
        if let Backend::Host { stream, .. } = &self.backend {
            let mut s = stream
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = crate::agent_host::write_frame(&mut *s, crate::agent_host::KILL, &[]);
        }
    }

    /// Handle one event from the IO thread. Returns `false` once the pump should stop.
    fn on_event(&mut self, ev: AlacEvent, cx: &mut Context<Self>) -> bool {
        match ev {
            AlacEvent::Wakeup => {
                // Shell output may mean files changed (touch/mkdir/git …) — let the
                // app schedule a debounced git/tree refresh. Emitted per output
                // batch; the subscriber debounces, so a build's stream of wakeups
                // costs one refresh after it quiets down.
                cx.emit(TerminalEvent::Output);
                // An open find re-counts now and then as output streams in, and once more
                // after it stops.
                if let Some(f) = self.find.as_mut().filter(|f| f.open && f.regex.is_some()) {
                    let since = f.scanned.map_or(FIND_RESCAN, |t| t.elapsed());
                    if since >= FIND_RESCAN {
                        self.find_scan(Pick::Keep);
                    } else if f.rescan.is_none() {
                        f.rescan = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(FIND_RESCAN.saturating_sub(since))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if let Some(f) = this.find.as_mut() {
                                    f.rescan = None;
                                }
                                if this.find_open() {
                                    this.find_scan(Pick::Keep);
                                    cx.notify();
                                }
                            });
                        }));
                    }
                }
                cx.notify();
            }
            AlacEvent::MouseCursorDirty => cx.notify(),
            // The emulator wants bytes written back to the PTY (query replies, etc.).
            AlacEvent::PtyWrite(text) => self.backend.write(text.into_bytes().into()),
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
                    self.backend.write(format(&text).into_bytes().into());
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
                self.backend.write(format(ws).into_bytes().into());
            }
        }
        true
    }

    /// The last `max_lines` non-blank lines of the live screen, as plain text (what the agent
    /// last showed — fed to the cleanup's AI check).
    pub fn tail_text(&self, max_lines: usize) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let (rows, cols) = (grid.screen_lines(), grid.columns());
        let mut lines: Vec<String> = (0..rows)
            .map(|l| {
                let row = &grid[Line(i32::try_from(l).unwrap_or(0))];
                (0..cols)
                    .map(|c| row[Column(c)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].join("\n")
    }

    /// Everything it has shown — scrollback and screen — as plain text (trailing blanks
    /// trimmed), for reading in an editor.
    pub fn scrollback_text(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let (history, rows, cols) = (grid.history_size(), grid.screen_lines(), grid.columns());
        let top = -i32::try_from(history).unwrap_or(0);
        let bottom = i32::try_from(rows).unwrap_or(0);
        let mut lines: Vec<String> = (top..bottom)
            .map(|l| {
                let row = &grid[Line(l)];
                (0..cols)
                    .map(|c| row[Column(c)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// The widget's focus handle (so the panel can focus the active tab).
    pub fn handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Write raw bytes to the PTY (typed text, pasted text, key escape sequences) — after
    /// any input still held back for an accepted suggestion.
    fn write<B: Into<std::borrow::Cow<'static, [u8]>>>(&mut self, bytes: B) {
        let bytes = bytes.into();
        match &mut self.held {
            Some(held) => held.extend_from_slice(&bytes),
            None => self.backend.write(bytes),
        }
    }

    /// Feed a string to the shell as if typed (used by the screenshot harness to seed a
    /// realistic session). The PTY buffers it until the shell is ready, so it's race-free.
    pub fn send_input(&self, text: &str) {
        self.backend.write(text.as_bytes().to_vec().into());
    }

    /// Whether Claude Code in it has finished its turn, from Kyde's hook. A new turn forgets
    /// the suggestion accepted in the last one (however its prompt was sent).
    pub fn set_claude_done(&mut self, done: bool, cx: &mut Context<Self>) {
        if !done {
            self.taken_suggestion = None;
        }
        if self.claude_done != done {
            self.claude_done = done;
            cx.notify();
        }
    }

    /// Claude's prompt suggestion on screen, unless it's the one already accepted for you.
    fn open_suggestion(&self, term: &Term<EventProxy>) -> Option<(String, GridPoint)> {
        if !self.claude_done {
            return None;
        }
        prompt_suggestion(term).filter(|(text, _)| self.taken_suggestion.as_ref() != Some(text))
    }

    /// The rows of Claude's input box holding a suggestion already accepted or cleared, which
    /// Claude has greyed back into the emptied box. They're drawn as the empty box they are.
    fn cleared_suggestion_rows(&self, term: &Term<EventProxy>) -> Option<RangeInclusive<Line>> {
        if !self.claude_done {
            return None;
        }
        let (text, end) = prompt_suggestion(term)?;
        (self.taken_suggestion.as_ref() == Some(&text))
            .then(|| term.grid().cursor.point.line..=end.line)
    }

    /// Backspace on Claude's prompt suggestion: clear it whole, not its last character.
    /// Nothing reaches Claude (its box is empty, the suggestion only greyed in) — the
    /// suggestion is just left out from then on, and typing replaces it as Claude does.
    fn clear_suggestion(&mut self) -> bool {
        let Some((text, _)) = self.open_suggestion(&self.term.lock()) else {
            return false;
        };
        self.taken_suggestion = Some(text);
        true
    }

    /// Before a key that edits Claude's input: accept its prompt suggestion with →, Claude's
    /// own key for that, so the key lands at the suggestion's end, where the cursor is drawn.
    /// The key waits a moment — arriving in the same read as the →, Claude applies it to the
    /// box as it was before the accept, and the suggestion is lost.
    fn accept_suggestion(&mut self, cx: &mut Context<Self>) {
        if self.held.is_some() {
            return;
        }
        let Some((text, _)) = self.open_suggestion(&self.term.lock()) else {
            return;
        };
        self.taken_suggestion = Some(text);
        self.backend.write(b"\x1b[C".to_vec().into());
        self.held = Some(Vec::new());
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(25))
                .await;
            let _ = this.update(cx, |v, _| {
                if let Some(held) = v.held.take().filter(|h| !h.is_empty()) {
                    v.backend.write(held.into());
                }
            });
        })
        .detach();
    }

    /// Relay a key's raw bytes to the PTY (typed keys from `on_key`; Enter / backspace / escape
    /// from their action handlers): jump to the live screen, accept or clear Claude's prompt
    /// suggestion first when the key acts on it, write, repaint.
    fn send_key(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.last_input = Some(std::time::Instant::now());
        self.scroll(Scroll::Bottom);
        cx.notify();
        match on_suggestion(&bytes) {
            Some(OnSuggestion::Accept) => self.accept_suggestion(cx),
            Some(OnSuggestion::Clear) if self.clear_suggestion() => return,
            _ => {}
        }
        self.write(bytes);
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
        self.backend.resize(WindowSize {
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

        if let Some(bytes) = key_bytes(key, m.control, ks.key_char.as_deref()) {
            if !bytes.is_empty() {
                self.send_key(bytes, cx);
            }
        }
    }

    /// Paste clipboard text, honouring bracketed-paste mode when the program enabled it.
    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.paste_text(text, cx);
        }
    }

    /// Linux: a middle click pastes the PRIMARY selection (the last text selected with the
    /// mouse, here or in any other app), as every Linux terminal does.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn paste_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        if let Some(text) = cx.read_from_primary().and_then(|i| i.text()) {
            self.paste_text(text, cx);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    fn paste_primary(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    /// Linux: a finished mouse selection also becomes the PRIMARY selection.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn selection_to_primary(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            if !text.is_empty() {
                cx.write_to_primary(gpui::ClipboardItem::new_string(text));
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    fn selection_to_primary(&self, _: &mut Context<Self>) {}

    /// Send pasted `text` to the shell, honouring bracketed-paste mode.
    fn paste_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.last_input = Some(std::time::Instant::now());
        self.accept_suggestion(cx);
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

    /// Clear the scrollback + screen (iTerm "Clear Buffer"): drop history, scroll to the live
    /// region, and send Ctrl-L so the shell redraws a fresh prompt (keeping any typed input).
    fn clear_screen(&mut self, cx: &mut Context<Self>) {
        {
            let mut t = self.term.lock();
            t.grid_mut().clear_history();
            t.scroll_display(Scroll::Bottom);
        }
        if let Some(f) = self.find.as_mut() {
            f.cur = None;
        }
        self.write(b"\x0c".to_vec());
        cx.notify();
    }

    /// Clear the current (unsubmitted) command-line input — the shell's "kill whole line".
    /// Sends Ctrl-E (→ end of line) then Ctrl-U (kill line) so the entire input is wiped in
    /// both zsh (Ctrl-U = kill-whole-line) and bash (Ctrl-U = kill-to-start, hence Ctrl-E
    /// first), regardless of cursor position. A no-op at an empty prompt. The shell echoes the
    /// erase, so no manual repaint of the grid is needed.
    fn clear_input(&mut self, cx: &mut Context<Self>) {
        self.write(vec![0x05, 0x15]); // Ctrl-E, Ctrl-U
        cx.notify();
    }

    /// Scroll the scrollback buffer by a number of lines (positive = up/back in history).
    fn scroll_lines(&mut self, lines: i32, cx: &mut Context<Self>) {
        if lines != 0 {
            self.scroll(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Scroll the view. Everything that scrolls comes through here, so an open find can tell
    /// its own scrolling from output pushing lines up (see [`shifted`]).
    fn scroll(&mut self, s: Scroll) {
        let mut t = self.term.lock();
        let before = t.grid().display_offset();
        t.scroll_display(s);
        let after = t.grid().display_offset();
        drop(t);
        if let Some((_, _, off)) = self.find.as_mut().and_then(|f| f.cur.as_mut()) {
            *off += after as isize - before as isize;
        }
    }

    /// The scrollbar, when there's scrollback to scroll through and the pane has painted.
    fn scrollbar(&self) -> Option<Sb> {
        let b = self.bounds?;
        let t = self.term.lock();
        let (hist, off) = (t.grid().history_size(), t.grid().display_offset());
        drop(t);
        if hist == 0 {
            return None;
        }
        let ch = self.cell.1.max(1.0);
        let track = f32::from(b.size.height);
        let top = hist.saturating_sub(off);
        let (len, pos) =
            crate::ui::scrollbar_thumb(track, hist as f32 * ch, -(top as f32) * ch, SB_END);
        Some(Sb {
            track,
            len,
            pos,
            hist,
            top,
        })
    }

    /// Press in the scrollbar gutter at window `y`: on the thumb, grab it; elsewhere, jump
    /// the thumb's middle there first — either way it then follows the cursor.
    fn sb_press(&mut self, y: f32, cx: &mut Context<Self>) {
        let (Some(sb), Some(b)) = (self.scrollbar(), self.bounds) else {
            return;
        };
        let rel = y - f32::from(b.top());
        let on_thumb = rel >= sb.pos && rel <= sb.pos + sb.len;
        let mut start_top = sb.top as f32;
        if !on_thumb {
            let span = sb.track - sb.len - 2.0 * SB_END;
            if span > 0.0 {
                let frac = ((rel - SB_END - sb.len / 2.0) / span).clamp(0.0, 1.0);
                start_top = frac * sb.hist as f32;
                self.scroll_to_top_lines(start_top);
            }
        }
        self.sb_drag = Some(SbDrag {
            start_y: y,
            start_top,
        });
        cx.notify();
    }

    /// The thumb follows a drag to window `y`.
    fn sb_drag_to(&mut self, y: f32, cx: &mut Context<Self>) {
        let (Some(d), Some(sb)) = (self.sb_drag, self.scrollbar()) else {
            return;
        };
        let span = sb.track - sb.len - 2.0 * SB_END;
        if span <= 0.0 {
            return;
        }
        self.scroll_to_top_lines(d.start_top + (y - d.start_y) * sb.hist as f32 / span);
        cx.notify();
    }

    /// Scroll so `top` lines of scrollback sit above the view.
    fn scroll_to_top_lines(&mut self, top: f32) {
        let t = self.term.lock();
        let (hist, off) = (t.grid().history_size(), t.grid().display_offset());
        drop(t);
        let top = (top.round().max(0.0) as usize).min(hist);
        let delta = (hist - top) as i32 - off as i32;
        if delta != 0 {
            self.scroll(Scroll::Delta(delta));
        }
    }

    /// Whether the find bar is open.
    fn find_open(&self) -> bool {
        self.find.as_ref().is_some_and(|f| f.open)
    }

    /// Open the find bar (⌘F) with its query selected, so typing replaces it.
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let f = self.find_state(cx);
        f.open = true;
        let query = f.query.clone();
        query.update(cx, |e, cx| {
            let n = e.text().len();
            e.select_range(0..n, cx);
        });
        window.focus(&query.read(cx).focus_handle);
        if let Some(m) = self.find_scan(Pick::Fresh) {
            self.reveal(&m);
        }
        cx.notify();
    }

    /// The find state, built the first time it's needed.
    fn find_state(&mut self, cx: &mut Context<Self>) -> &mut TermFind {
        self.find.get_or_insert_with(|| {
            let query = cx.new(|cx| {
                let mut e = CodeEditor::single_line(cx, "Find");
                // Enter / ⇧Enter / Escape: the find bar's keys, ahead of the terminal's.
                e.ctx_override = Some("FindBar");
                e
            });
            let sub = cx.subscribe(&query, |this, _q, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Changed) {
                    this.find_query_changed(cx);
                }
            });
            TermFind {
                open: false,
                query,
                _query_sub: sub,
                regex: None,
                cur: None,
                count: 0,
                index: 0,
                scanned: None,
                rescan: None,
            }
        })
    }

    /// Close the find bar and hand the keyboard back to the shell.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.find.as_mut() {
            f.open = false;
            f.cur = None;
        }
        window.focus(&self.focus);
        cx.notify();
    }

    /// The query was edited: search for it afresh.
    fn find_query_changed(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else {
            return;
        };
        let q = f.query.read(cx).text().to_string();
        f.regex = (!q.is_empty())
            .then(|| RegexSearch::new(&literal_regex(&q)).ok())
            .flatten();
        f.cur = None;
        if let Some(m) = self.find_scan(Pick::Fresh) {
            self.reveal(&m);
        }
        cx.notify();
    }

    /// Step to the next match up or down the buffer and bring it into view.
    fn find_step(&mut self, pick: Pick, cx: &mut Context<Self>) {
        if let Some(m) = self.find_scan(pick) {
            self.reveal(&m);
        }
        cx.notify();
    }

    /// Search the whole buffer for the query and land on the match `pick` says — returned,
    /// for scrolling to (`None` with no match, or for a [`Pick::Keep`] re-count).
    fn find_scan(&mut self, pick: Pick) -> Option<Match> {
        let f = self.find.as_mut()?;
        f.scanned = Some(Instant::now());
        let Some(regex) = f.regex.as_mut() else {
            (f.cur, f.count, f.index) = (None, 0, 0);
            return None;
        };
        let term = self.term.lock();
        let all = scan_all(&term, regex);
        let grid = term.grid();
        let (hist, off) = (grid.history_size(), grid.display_offset());
        let now = f.cur.as_ref().and_then(|c| shifted(&term, c));
        let bottom = Line(term.screen_lines() as i32 - 1 - off as i32);
        drop(term);
        let n = all.len();
        let last = n.checked_sub(1);
        let i = match (pick, &now) {
            _ if n == 0 => None,
            (Pick::Keep, Some(c)) => all.iter().position(|m| m == c),
            (Pick::Keep, None) => None,
            (Pick::Fresh, _) => all.iter().rposition(|m| m.start().line <= bottom).or(last),
            (Pick::Older, Some(c)) => all.iter().rposition(|m| m.start() < c.start()).or(last),
            (Pick::Newer, Some(c)) => all.iter().position(|m| m.start() > c.start()).or(Some(0)),
            (Pick::Older | Pick::Newer, None) => last,
        };
        f.count = n;
        f.index = i.map_or(0, |i| n - i);
        let m = i.map(|i| all[i].clone());
        // A re-count that can't find the current match any more (cleared, scrolled off the
        // top, or lost track of once the scrollback is full) drops it.
        f.cur = m.clone().map(|m| (m, hist, off as isize));
        m.filter(|_| !matches!(pick, Pick::Keep))
    }

    /// Scroll `m` to the middle of the view, unless it's already all on screen.
    fn reveal(&mut self, m: &Match) {
        let t = self.term.lock();
        let rows = t.screen_lines() as i32;
        let (hist, off) = (
            t.grid().history_size() as i32,
            t.grid().display_offset() as i32,
        );
        drop(t);
        let (first, last) = (m.start().line.0, m.end().line.0);
        if first >= -off && last < rows - off {
            return;
        }
        let target = (rows / 2 - first).clamp(0, hist);
        self.scroll(Scroll::Delta(target - off));
    }

    /// The open find's matches on screen, and its current match if it's one of them.
    fn visible_matches(&mut self) -> (Vec<Match>, Option<Match>) {
        let Some(f) = self.find.as_mut().filter(|f| f.open) else {
            return (Vec::new(), None);
        };
        let Some(regex) = f.regex.as_mut() else {
            return (Vec::new(), None);
        };
        let term = self.term.lock();
        let off = term.grid().display_offset() as i32;
        let top = GridPoint::new(Line(-off), Column(0));
        let bottom = GridPoint::new(
            Line(term.screen_lines() as i32 - 1 - off),
            term.last_column(),
        );
        let on_screen: Vec<Match> =
            RegexIter::new(top, bottom, Direction::Right, &term, regex).collect();
        // Only outlined where a match still is: with the scrollback full, output scrolls the
        // text without moving any count `shifted` can follow.
        let cur = f
            .cur
            .as_ref()
            .and_then(|c| shifted(&term, c))
            .filter(|c| on_screen.contains(c));
        (on_screen, cur)
    }

    /// Bring the prompt you sent before (`older`) or after the one at the top of the view to
    /// the top (⌘↑ / ⌘↓). Past the newest, back to the live screen.
    fn jump_prompt(&mut self, older: bool, cx: &mut Context<Self>) {
        let t = self.term.lock();
        let (hist, off) = (
            i32::try_from(t.grid().history_size()).unwrap_or(0),
            i32::try_from(t.grid().display_offset()).unwrap_or(0),
        );
        let prompts = prompt_lines(&t);
        drop(t);
        // The view can't scroll past the live screen: a prompt already on it is as far down
        // as a jump gets.
        match prompt_jump(&prompts, -off, older).map(|top| (-top).clamp(0, hist)) {
            Some(want) if want != off => self.scroll(Scroll::Delta(want - off)),
            Some(_) | None if !older => self.scroll(Scroll::Bottom),
            _ => {}
        }
        cx.notify();
    }

    /// Your newest prompt, when it's scrolled off above the view: its text, for the pill
    /// that jumps back to it.
    fn prompt_above(&self) -> Option<String> {
        let t = self.term.lock();
        let top = -i32::try_from(t.grid().display_offset()).unwrap_or(0);
        last_prompt(&t)
            .filter(|l| l.0 < top)
            .map(|l| prompt_text(&t, l))
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
        let row = ((ly / ch).floor() as i32 - i32::from(self.shift)).max(0);
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
/// punctuation is trimmed so "see <https://x.com>." doesn't capture the period.
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

/// `query` as a regex that matches it literally.
fn literal_regex(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Every match of `regex` in `term` — scrollback and screen — top to bottom.
fn scan_all<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<Match> {
    let start = GridPoint::new(term.topmost_line(), Column(0));
    let end = GridPoint::new(term.bottommost_line(), term.last_column());
    RegexIter::new(start, end, Direction::Right, term, regex).collect()
}

/// Where a find's current match `(m, history size, scroll offset)` is now. Output pushes
/// lines up: by as many as the scrollback grew, or — once it's full and stops growing — as
/// many as a scrolled-back view moved to stay on them (our own scrolling is already taken
/// out of the offset). `None` once it's gone off the top or the scrollback shrank (cleared).
fn shifted<T>(term: &Term<T>, (m, hist, off): &(Match, usize, isize)) -> Option<Match> {
    let grid = term.grid();
    let grew = grid.history_size() as isize - *hist as isize;
    if grew < 0 {
        return None;
    }
    let moved = grid.display_offset() as isize - off;
    let up = i32::try_from(grew.max(moved)).ok()?;
    let lift = |p: &GridPoint| GridPoint::new(Line(p.line.0 - up), p.column);
    let (a, b) = (lift(m.start()), lift(m.end()));
    (a.line >= term.topmost_line() && b.line <= term.bottommost_line()).then_some(a..=b)
}

/// Claude Code's prompt marker: on its input box, and on each prompt you sent where it echoes
/// it into the conversation.
const PROMPT_MARK: char = '❯';

/// How far above the view a prompt that runs onto it is looked for.
const PROMPT_LOOKBACK: i32 = 1_000;

/// The tint of `line` when it's Claude Code's echo of a prompt you sent: "❯ " and the text,
/// on a background of its own. Its input box's "❯" is followed by a no-break space and isn't
/// tinted, and neither is a shell prompt's.
fn prompt_tint<T>(term: &Term<T>, line: Line) -> Option<AnsiColor> {
    if term.columns() < 2 {
        return None;
    }
    let row = &term.grid()[line];
    let first = &row[Column(0)];
    (first.c == PROMPT_MARK
        && row[Column(1)].c == ' '
        && first.bg != AnsiColor::Named(NamedColor::Background))
    .then_some(first.bg)
}

/// The tint of `line` when it opens on a tinted blank — how a prompt's wrapped (or further)
/// lines continue, indented under its text on its tint.
fn tinted_blank<T>(term: &Term<T>, line: Line) -> Option<AnsiColor> {
    let first = &term.grid()[line][Column(0)];
    (first.c == ' ' && first.bg != AnsiColor::Named(NamedColor::Background)).then_some(first.bg)
}

/// For each of the `rows` rows from `top`, the tint of the prompt it's part of, if any.
fn prompt_rows<T>(term: &Term<T>, top: Line, rows: usize) -> Vec<Option<AnsiColor>> {
    let end = Line((top.0 + i32::try_from(rows).unwrap_or(0) - 1).min(term.bottommost_line().0));
    // A prompt already under way at the top of the view: start from its first row.
    let mut line = top;
    let floor = Line((top.0 - PROMPT_LOOKBACK).max(term.topmost_line().0));
    while line > floor && tinted_blank(term, line).is_some() {
        line = Line(line.0 - 1);
    }
    let mut tint = None;
    let mut out = Vec::with_capacity(rows);
    while line <= end {
        tint = prompt_tint(term, line)
            .or_else(|| tint.filter(|&t| tinted_blank(term, line) == Some(t)));
        if line >= top {
            out.push(tint);
        }
        line = Line(line.0 + 1);
    }
    out
}

/// Every prompt you sent that's still in the buffer, oldest first.
fn prompt_lines<T>(term: &Term<T>) -> Vec<Line> {
    (term.topmost_line().0..=term.bottommost_line().0)
        .map(Line)
        .filter(|&l| prompt_tint(term, l).is_some())
        .collect()
}

/// The newest prompt you sent that's still in the buffer.
fn last_prompt<T>(term: &Term<T>) -> Option<Line> {
    (term.topmost_line().0..=term.bottommost_line().0)
        .rev()
        .map(Line)
        .find(|&l| prompt_tint(term, l).is_some())
}

/// The view's new top line for a jump from top line `top` to the prompt before it (`older`)
/// or after it, among `prompts` (oldest first): the prompt a row down, so it reads as the
/// start of its turn. `None` past the last one either way.
fn prompt_jump(prompts: &[Line], top: i32, older: bool) -> Option<i32> {
    let at = |l: &Line| l.0 - 1;
    if older {
        prompts.iter().rev().map(at).find(|&t| t < top)
    } else {
        prompts.iter().map(at).find(|&t| t > top)
    }
}

/// The text of the prompt at `line` (its first row, after the "❯ ").
fn prompt_text<T>(term: &Term<T>, line: Line) -> String {
    let row = &term.grid()[line];
    (2..term.columns())
        .map(|c| &row[Column(c)])
        .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        .map(|cell| cell.c)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Dropped paths as the terminal's shell takes them: one quoted word each, space-separated,
/// with one trailing space so the next word starts clear of them. A path that isn't UTF-8 or
/// holds a control character types nothing: the tty and line editor act on ESC, CR and Ctrl-C
/// even inside quotes (a crafted name could end a bracketed paste or run a command), and a
/// lossy copy of a non-UTF-8 name points at no file.
fn shell_quoted(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        let Some(s) = p.to_str().filter(|s| !s.contains(char::is_control)) else {
            continue;
        };
        // POSIX shells: single quotes, an embedded `'` written `'\''`.
        #[cfg(not(target_os = "windows"))]
        {
            out.push('\'');
            out.push_str(&s.replace('\'', r"'\''"));
            out.push_str("' ");
        }
        // cmd.exe doesn't quote with `'`; double quotes do, and a Windows path can't hold `"`.
        #[cfg(target_os = "windows")]
        {
            out.push('"');
            out.push_str(s);
            out.push_str("\" ");
        }
    }
    out
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
        match &self.backend {
            Backend::Local { notifier, .. } => {
                let _ = notifier.0.send(Msg::Shutdown);
            }
            // A hosted terminal keeps running; just hang up (the reader thread ends).
            #[cfg(unix)]
            Backend::Host { stream, .. } => {
                let s = stream
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Emitted so the panel (Kyde) can react: repaint the tab label on a title change, or close
/// this tab when the terminal's own context menu requests it (tabs live on `Kyde`).
pub enum TerminalEvent {
    TitleChanged,
    CloseRequested,
    /// The shell produced output — files may have changed (subscriber debounces).
    Output,
    /// Enter was pressed — a command or prompt was submitted.
    Submitted,
    /// "Open Scrollback in Editor" from its menu.
    OpenScrollback,
}
impl EventEmitter<TerminalEvent> for TerminalView {}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let menu_at = self.menu_at;
        div()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .relative()
            .size_full()
            .bg(theme::get().main_bg)
            // Files dropped from a file manager are typed in as shell-quoted paths, sent the
            // way a paste is (bracketed when the program asked for it), so an image dropped on
            // an agent tab reaches Claude as a pasted path. Tinted like a tree folder while
            // they hover.
            .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(theme::get().selected_bg))
            .on_drop(cx.listener(|this, ep: &gpui::ExternalPaths, window, cx| {
                window.focus(&this.focus);
                let text = shell_quoted(ep.paths());
                if !text.is_empty() {
                    this.paste_text(text, cx);
                }
            }))
            // ⌘K clears the current (unsubmitted) input line — overrides the global commit
            // binding in this context.
            .on_action(cx.listener(|this, _: &crate::ClearTerminal, window, cx| {
                if this.focus.is_focused(window) {
                    this.clear_input(cx);
                }
            }))
            // ⌘F searches the scrollback. While the bar is open, Enter / ⌘G step up to older
            // matches, ⇧Enter / ⇧⌘G back down, Escape closes it; closed, they're the app's.
            .on_action(cx.listener(|this, _: &crate::TerminalFind, window, cx| {
                this.open_find(window, cx);
            }))
            // ⌘↑ / ⌘↓ step through the prompts you sent an agent (Terminal.app's marks keys).
            .on_action(cx.listener(|this, _: &crate::TerminalPrevPrompt, _w, cx| {
                this.jump_prompt(true, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::TerminalNextPrompt, _w, cx| {
                this.jump_prompt(false, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::FindNext, _w, cx| {
                if this.find_open() {
                    this.find_step(Pick::Older, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::FindPrev, _w, cx| {
                if this.find_open() {
                    this.find_step(Pick::Newer, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::CloseFind, window, cx| {
                if this.find_open() {
                    this.close_find(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            // Enter / Backspace / Escape are also app shortcuts. gpui dispatches
            // binding actions before on_key_down and consumes the key, so we relay the PTY byte
            // here rather than let it fall through to the app action (which deleted the selected
            // file / closed a modal). Mirrors how the editor binds backspace to a buffer action.
            .on_action(cx.listener(|this, _: &crate::TerminalEnter, _w, cx| {
                // An emptied box sends nothing, so a cleared suggestion stays cleared.
                let empty = this.cleared_suggestion_rows(&this.term.lock()).is_some();
                this.send_key(b"\r".to_vec(), cx);
                if !empty {
                    this.taken_suggestion = None;
                }
                cx.emit(TerminalEvent::Submitted);
            }))
            .on_action(cx.listener(|this, _: &crate::TerminalBackspace, _w, cx| {
                this.send_key(vec![0x7f], cx);
            }))
            .on_action(cx.listener(|this, _: &crate::TerminalEscape, _w, cx| {
                this.send_key(vec![0x1b], cx);
            }))
            // ⌘C / ⌘V are the file tree's copy / paste at the app root; here they're the
            // terminal's (selection → clipboard, clipboard → the shell).
            .on_action(cx.listener(|this, _: &crate::TerminalCopy, _w, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &crate::TerminalPaste, _w, cx| this.paste(cx)))
            // Pin the mono font + a fixed size so the cell metrics the element measures (the
            // `M` advance) match the glyphs it actually shapes — otherwise the cursor, placed
            // at `col × cell_w`, drifts off the text (it inherits a proportional UI font).
            // The UI size: dense like a real terminal, and the same as the Agents sidebar.
            .font_family(theme::font::FAMILY)
            .text_size(px(theme::get().ui_font_size))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    // Cmd-click (Ctrl-click off macOS) opens a URL under the cursor; a
                    // plain click starts a selection (double = word, triple = line).
                    if e.modifiers.secondary() {
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
                cx.listener(|this, _e: &MouseUpEvent, _w, cx| {
                    if this.selecting {
                        this.selection_to_primary(cx);
                    }
                    this.selecting = false;
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _e: &MouseDownEvent, window, cx| this.paste_primary(window, cx)),
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
            // Keys typed in the find box aren't the shell's.
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.focus.is_focused(window) {
                    this.on_key(e, cx);
                }
            }))
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _w, cx| {
                let ch = this.cell.1.max(1.0);
                if matches!(e.touch_phase, gpui::TouchPhase::Started) {
                    this.scroll_px = 0.0;
                }
                this.scroll_px += f32::from(e.delta.pixel_delta(px(ch)).y);
                let lines = (this.scroll_px / ch).trunc();
                this.scroll_px -= lines * ch;
                this.scroll_lines(lines as i32, cx);
            }))
            .flex()
            .flex_row()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(TerminalElement { view }),
            )
            .child(self.render_scrollbar(cx))
            .children(
                self.find
                    .as_ref()
                    .filter(|f| f.open)
                    .map(|f| self.render_find(f, cx)),
            )
            .children(
                (!self.find_open())
                    .then(|| self.prompt_above())
                    .flatten()
                    .map(|text| self.render_prompt_pill(text, cx)),
            )
            .children(menu_at.map(|at| self.render_context_menu(at, window, cx)))
    }
}

impl TerminalView {
    /// The scrollbar's gutter at the right edge — always there, so the grid doesn't reflow
    /// when scrollback first appears; the thumb shows once there's something to scroll.
    fn render_scrollbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let sb = self.scrollbar();
        let dragging = self.sb_drag.is_some();
        div()
            .id("term-scrollbar")
            .w(px(SB_BAR))
            .flex_none()
            .h_full()
            .relative()
            .when(sb.is_some(), |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.sb_press(f32::from(e.position.y), cx);
                    }),
                )
            })
            .children(sb.map(|sb| {
                div()
                    .absolute()
                    .left(px((SB_BAR - SB_THUMB) / 2.0))
                    .top(px(sb.pos))
                    .w(px(SB_THUMB))
                    .h(px(sb.len))
                    .rounded_full()
                    .bg(t.line_number)
                    .opacity(if dragging { 0.85 } else { 0.5 })
                    .hover(|s| s.opacity(0.85))
                    .cursor_pointer()
            }))
    }

    /// The find bar, floated at the top right over the output.
    fn render_find(&self, f: &TermFind, cx: &mut Context<Self>) -> impl IntoElement {
        let (next, prev) = crate::FIND_STEP_KEYS;
        let bar = crate::ui::find::Bar::new(
            "term-find",
            f.query.clone(),
            crate::ui::find::match_label(f.regex.is_some(), f.count, f.index),
        )
        .on_up(
            format!("Older match  ({next})"),
            cx.listener(|this, _e, _w, cx| this.find_step(Pick::Older, cx)),
        )
        .on_down(
            format!("Newer match  ({prev})"),
            cx.listener(|this, _e, _w, cx| this.find_step(Pick::Newer, cx)),
        )
        .on_close(cx.listener(|this, _e, window, cx| this.close_find(window, cx)));
        crate::ui::find::floating(bar)
    }

    /// The pill showing your last prompt once it has scrolled off the top, floated at the top
    /// right like the find bar; a click scrolls back to it.
    fn render_prompt_pill(&self, text: String, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let key = crate::pretty_key(&format!("{}-up", crate::TERMINAL_MOD));
        div()
            .id("term-prompt-pill")
            .absolute()
            .top(px(6.0))
            .right(px(SB_BAR + 6.0))
            .max_w(px(360.0))
            .occlude()
            .tooltip(crate::ui::tip(format!("Back to your last prompt  ({key})")))
            .on_mouse_down(MouseButton::Right, |_e, _w, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    cx.stop_propagation();
                    this.jump_prompt(true, cx);
                }),
            )
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1()
            .bg(t.panel_bg)
            .border_1()
            .border_color(t.divider)
            .rounded_md()
            .shadow_md()
            .cursor_pointer()
            .hover(|d| d.bg(t.selected_bg))
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(t.ui_font_size - 1.0))
            .text_color(t.secondary_text)
            .child(
                svg()
                    .path("icons/arrow-up.svg")
                    .flex_none()
                    .size(px(12.0))
                    .text_color(t.primary),
            )
            .child(div().min_w_0().truncate().child(text))
    }

    /// Right-click menu: a transparent dismiss backdrop + a small box anchored at the click.
    fn render_context_menu(
        &self,
        at: gpui::Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme::get();
        let has_selection = self
            .term
            .lock()
            .selection_to_string()
            .is_some_and(|s| !s.is_empty());
        // A row: its label, the shortcut (muted) on the right; greyed out when `enabled` is off.
        let item = |label: &'static str, key: String, enabled: bool| {
            div()
                .id(label)
                .flex()
                .flex_row()
                .items_center()
                .gap_6()
                .px_3()
                .py_1()
                .text_size(px(13.0))
                .rounded_md()
                .text_color(if enabled { t.text } else { t.line_number })
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|d| d.bg(t.selected_bg))
                })
                .child(div().flex_1().child(label))
                .child(div().text_color(t.line_number).child(key))
        };
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.menu_at = None;
            cx.notify();
        };
        // The terminal's own keys: ⌘C / ⌘V on macOS, Ctrl+Shift+C / V elsewhere.
        let term_key = |key: &str| crate::pretty_key(&format!("{}-{key}", crate::TERMINAL_MOD));
        let scrollback_key = if cfg!(target_os = "macos") {
            "⌥⌘E".to_string()
        } else {
            crate::pretty_key("cmd-alt-e")
        };
        let panel = div()
            .flex()
            .flex_col()
            .min_w(px(220.0))
            .p_1()
            .bg(t.panel_bg)
            .border_1()
            .border_color(t.divider)
            .rounded_md()
            .shadow_md()
            .font_family(theme::font::UI_FAMILY)
            .child(
                item("Copy", term_key("c"), has_selection).when(has_selection, |d| {
                    d.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            close(this, cx);
                            this.copy_selection(cx);
                        }),
                    )
                }),
            )
            .child(item("Paste", term_key("v"), true).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    cx.stop_propagation();
                    close(this, cx);
                    this.paste(cx);
                }),
            ))
            .child(
                item("Open Scrollback in Editor", scrollback_key, true).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        close(this, cx);
                        cx.emit(TerminalEvent::OpenScrollback);
                    }),
                ),
            )
            .child(item("Clear Screen", String::new(), true).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    cx.stop_propagation();
                    close(this, cx);
                    this.clear_screen(cx);
                }),
            ))
            .child(item("Close", String::new(), true).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    cx.stop_propagation();
                    close(this, cx);
                    // Tabs live on Kyde — ask it to close this one.
                    cx.emit(TerminalEvent::CloseRequested);
                }),
            ));
        // Drawn over everything (not clipped by the pane), at the cursor — flipped / nudged
        // back inside the window near an edge — over a window-wide backdrop: any click
        // outside closes it.
        let viewport = window.viewport_size();
        gpui::deferred(
            gpui::anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| close(this, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _e, _w, cx| close(this, cx)),
                    )
                    .child(
                        gpui::anchored()
                            .position(at)
                            .snap_to_window_with_margin(px(8.0))
                            .child(panel),
                    ),
            ),
        )
        .with_priority(1)
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
    /// Find matches on screen (the current one outlined), over the backgrounds.
    find_quads: Vec<gpui::PaintQuad>,
    /// Block/bar cursor quad, if visible (not in scrollback, not hidden).
    cursor: Option<gpui::PaintQuad>,
    line_height: Pixels,
    /// Rows to push everything down by (bottom-anchoring).
    shift: f32,
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
        (): &mut (),
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
        let (found, current) = self.view.update(cx, |v, _| v.visible_matches());

        let view = self.view.read(cx);
        let term = view.term.lock();
        let content = term.renderable_content();
        let default_fg: Hsla = theme::get().text.into();
        let default_bg: Hsla = theme::get().main_bg.into();

        let off = i32::try_from(content.display_offset).unwrap_or(0);
        // Prompts you sent an agent (Claude Code's echo of them) take an accent tint, and their
        // ❯ the accent, so your turns stand out from its replies.
        let prompts = prompt_rows(&term, Line(-off), term.screen_lines());
        let cleared = view.cleared_suggestion_rows(&term);
        let accent: Hsla = theme::get().primary.into();
        let prompt_bg = default_bg.blend(accent.opacity(0.2));

        let selection = content.selection;
        let sel_bg: Hsla = theme::get().selected_bg.into();
        let mut lines: Vec<ShapedLine> = Vec::new();
        // Background and selection runs — one quad per run of same-coloured cells, not per
        // cell: a full-width coloured line was ~200 quads, and quads were most of a frame.
        // Laid out once the shift is known.
        let mut bg_runs: Vec<BgRun> = Vec::new();
        let mut sel_runs: Vec<BgRun> = Vec::new();
        // Last row with anything on it, for bottom-anchoring.
        let mut last_used: i32 = -1;
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
            let row_idx = line - base_line.unwrap_or(line);
            let mut fg = resolve(cell.fg, default_fg, content.colors);
            let mut bg = resolve_opt(cell.bg, content.colors);
            let prompt = usize::try_from(row_idx)
                .ok()
                .and_then(|r| prompts.get(r).copied().flatten());
            if let Some(tint) = prompt {
                if cell.bg == tint {
                    bg = Some(prompt_bg);
                }
                if col == 0 && cell.c == PROMPT_MARK {
                    fg = accent;
                }
            }
            if cell.flags.contains(Flags::INVERSE) {
                let f = fg;
                fg = bg.unwrap_or(default_bg);
                bg = Some(f);
            }
            // SGR dim (faint) — how Claude greys its prompt suggestion apart from typed text.
            if cell.flags.contains(Flags::DIM) {
                fg = fg.opacity(DIM_ALPHA);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg.unwrap_or(default_bg);
            }
            // Selection tint wins over the cell's own bg (its runs paint after).
            let selected = selection.as_ref().is_some_and(|s| s.contains(cell.point));
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            if let Some(bg) = bg {
                push_run(&mut bg_runs, row_idx, col, width, bg);
            }
            if selected {
                push_run(&mut sel_runs, row_idx, col, width, sel_bg);
            }
            // A cleared suggestion Claude greyed back into its emptied box is left out.
            let ch = if cell.flags.contains(Flags::DIM)
                && col >= 2
                && cleared
                    .as_ref()
                    .is_some_and(|r| r.contains(&cell.point.line))
            {
                ' '
            } else {
                cell.c
            };
            if (ch != ' ' && ch != '\0') || bg.is_some() {
                last_used = last_used.max(line - base_line.unwrap_or(line));
            }
            cur_row.push((if ch == '\0' { ' ' } else { ch }, fg));
        }
        flush_row(&mut lines, &mut cur_row);

        // Bottom-anchor a terminal that hasn't filled its pane yet — live screen, no
        // scrollback, not a fullscreen (alt-screen) app: push the content down so the prompt
        // or an agent's input box sits at the bottom, herdr-style, rather than floating over
        // an empty lower half. Once output fills the pane the shift is naturally zero.
        let cursor_row = content.cursor.point.line.0 - base_line.unwrap_or(0);
        let anchor = view.anchor_bottom
            && content.display_offset == 0
            && term.grid().history_size() == 0
            && !content.mode.contains(TermMode::ALT_SCREEN);
        let shift_rows = if anchor {
            (i32::from(rows) - (last_used.max(cursor_row) + 1)).max(0)
        } else {
            0
        };
        #[allow(clippy::cast_precision_loss)] // a row count, far below f32's exact range
        let shift = shift_rows as f32;
        let quad = |row: i32, col: usize, len: usize, color: Hsla| {
            let x = bounds.left() + px(col as f32 * cell_w_f);
            let y = bounds.top() + line_height * (row as f32 + shift);
            fill(
                Bounds::new(
                    point(x, y),
                    gpui::size(px(len as f32 * cell_w_f), line_height),
                ),
                color,
            )
        };
        let bg_quads: Vec<gpui::PaintQuad> = bg_runs
            .iter()
            .chain(&sel_runs)
            .map(|r| quad(r.row, r.col, r.len, r.color))
            .collect();
        let last_col = term.columns().saturating_sub(1);
        let find_bg: Hsla = theme::get().find_match_bg.into();
        let mut find_quads: Vec<gpui::PaintQuad> = found
            .iter()
            .flat_map(|m| match_rows(m, off, i32::from(rows), last_col))
            .map(|(row, c0, c1)| quad(row, c0, c1 - c0 + 1, find_bg))
            .collect();
        if let Some(m) = &current {
            let caret: Hsla = theme::get().caret.into();
            find_quads.extend(
                match_rows(m, off, i32::from(rows), last_col)
                    .into_iter()
                    .map(|(row, c0, c1)| {
                        quad(row, c0, c1 - c0 + 1, find_bg)
                            .border_widths(px(1.0))
                            .border_color(caret)
                    }),
            );
        }

        // Block cursor — only on the live screen (no scrollback offset) and when shown. Over
        // a Claude prompt suggestion it's at the suggestion's end: that's where typing goes.
        let cursor = if content.display_offset == 0 && content.cursor.shape != CursorShape::Hidden {
            let base = base_line.unwrap_or(0);
            let at = view
                .open_suggestion(&term)
                .map_or(content.cursor.point, |(_, end)| end);
            let row = at.line.0 - base;
            let col = at.column.0.min(usize::from(cols).saturating_sub(1));
            if row >= 0 && (row as u16) < rows {
                let x = bounds.left() + px(col as f32 * cell_w_f);
                let y = bounds.top() + line_height * (row as f32 + shift);
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
        let shift_u16 = u16::try_from(shift_rows).unwrap_or(0);
        self.view.update(cx, |v, _| v.shift = shift_u16);
        Prepaint {
            lines,
            bg_quads,
            find_quads,
            cursor,
            line_height,
            shift,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        for q in pre.bg_quads.drain(..).chain(pre.find_quads.drain(..)) {
            window.paint_quad(q);
        }
        // Cursor under the glyph so an inverse block still shows the character.
        if let Some(c) = pre.cursor.take() {
            window.paint_quad(c);
        }
        let lh = pre.line_height;
        for (i, line) in pre.lines.iter().enumerate() {
            let origin = point(bounds.left(), bounds.top() + lh * (i as f32 + pre.shift));
            let _ = line.paint(origin, lh, window, cx);
        }
        self.view.update(cx, |v, _| v.bounds = Some(bounds));
        // A scrollbar drag follows the cursor anywhere in the window, until the button's up.
        if self.view.read(cx).sb_drag.is_some() {
            let view = self.view.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _w, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |v, cx| v.sb_drag_to(f32::from(e.position.y), cx));
                }
            });
            let view = self.view.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _w, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |v, cx| {
                        v.sb_drag = None;
                        cx.notify();
                    });
                }
            });
        }
    }
}

/// A run of same-coloured cell backgrounds on one row (painted as one quad).
struct BgRun {
    row: i32,
    col: usize,
    len: usize,
    color: Hsla,
}

/// Add `width` cells at (`row`, `col`) to `runs`, extending the last run when they continue
/// it in its colour.
fn push_run(runs: &mut Vec<BgRun>, row: i32, col: usize, width: usize, color: Hsla) {
    if let Some(r) = runs.last_mut() {
        if r.row == row && r.col + r.len == col && r.color == color {
            r.len += width;
            return;
        }
    }
    runs.push(BgRun {
        row,
        col,
        len: width,
        color,
    });
}

/// The screen rows `m` covers — `(row, first column, last column)` — given the view's scroll
/// offset; rows off screen are left out.
fn match_rows(m: &Match, off: i32, rows: i32, last_col: usize) -> Vec<(i32, usize, usize)> {
    let (a, b) = (m.start(), m.end());
    (a.line.0..=b.line.0)
        .filter(|l| (0..rows).contains(&(l + off)))
        .map(|l| {
            let c0 = if l == a.line.0 { a.column.0 } else { 0 };
            let c1 = if l == b.line.0 { b.column.0 } else { last_col };
            (l + off, c0, c1.max(c0))
        })
        .collect()
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
        AnsiColor::Named(n) => named_rgb(n, colors).map_or(default, rgb_to_hsla),
        AnsiColor::Spec(rgb) => rgb_to_hsla(rgb),
        AnsiColor::Indexed(i) => rgb_to_hsla(indexed_rgb(i, colors)),
    }
}

/// Like `resolve` but returns `None` for the default background (so it isn't painted).
fn resolve_opt(color: AnsiColor, colors: &alacritty_terminal::term::color::Colors) -> Option<Hsla> {
    match color {
        AnsiColor::Named(NamedColor::Background) => None,
        AnsiColor::Named(n) => named_rgb(n, colors).map(rgb_to_hsla),
        AnsiColor::Spec(rgb) => Some(rgb_to_hsla(rgb)),
        AnsiColor::Indexed(i) => Some(rgb_to_hsla(indexed_rgb(i, colors))),
    }
}

fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    gpui::rgb((u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)).into()
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

fn indexed_rgb(i: u8, colors: &alacritty_terminal::term::color::Colors) -> Rgb {
    colors[i as usize].unwrap_or_else(|| default_indexed(i))
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

/// Claude Code's prompt suggestion, when its input box shows one: a predicted next prompt
/// greyed (SGR dim) into the empty box, the cursor parked at its start just after the `❯ `
/// marker — it looks typed, but typing replaces it. Its text and the point just past its end
/// (on the box's indented rows below, when it wraps). Its hints (a fresh session's `Try "…"`,
/// queued-message help) are drawn the same way, so this alone can't tell them apart — they
/// only show while it isn't done, which `claude_done` gates on.
fn prompt_suggestion<T>(term: &Term<T>) -> Option<(String, GridPoint)> {
    let grid = term.grid();
    let at = grid.cursor.point;
    let ghost = |c: &Cell| c.flags.contains(Flags::DIM) && !matches!(c.c, ' ' | '\0');
    let row = &grid[at.line];
    if !term.mode().contains(TermMode::SHOW_CURSOR)
        || at.column.0 != 2
        || row[Column(0)].c != PROMPT_MARK
        || !ghost(&row[at.column])
    {
        return None;
    }
    let (mut text, mut end, mut line) = (String::new(), at, at.line);
    while line <= grid.bottommost_line() {
        let row = &grid[line];
        if line != at.line && (row[Column(0)].c != ' ' || !ghost(&row[Column(2)])) {
            break;
        }
        for col in 2..grid.columns() {
            let cell = &row[Column(col)];
            if ghost(cell) {
                text.push(cell.c);
                let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                end = GridPoint::new(line, Column(col + width));
            } else if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                text.push(' ');
            }
        }
        line = Line(line.0 + 1);
    }
    Some((text.split_whitespace().collect::<Vec<_>>().join(" "), end))
}

/// What a key does to Claude's prompt suggestion, drawn as if typed into its box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnSuggestion {
    /// Accept it first, as typed text: typed text and ← edit at the cursor, and Enter sends
    /// it — Claude's own Enter skips a suggestion and finds the box empty.
    Accept,
    /// Clear it whole: Backspace.
    Clear,
}

/// How a key's bytes act on Claude's prompt suggestion, if at all.
fn on_suggestion(bytes: &[u8]) -> Option<OnSuggestion> {
    match bytes {
        [0x7f] => Some(OnSuggestion::Clear),
        b"\x1b[D" | b"\r" => Some(OnSuggestion::Accept),
        [b, ..] if *b >= 0x20 && *b != 0x7f => Some(OnSuggestion::Accept),
        _ => None,
    }
}

/// Translate a (non-⌘) key press to the bytes to write to the PTY, or `None` if the key
/// produces nothing. Pure (no gpui / no PTY) so the mapping is unit-testable:
/// - Ctrl + letter → its control byte (Ctrl-A = 0x01 … Ctrl-Z = 0x1a; Ctrl-C = 0x03 = SIGINT).
/// - Named keys → their terminal escape sequences (arrows = shell history / line editing,
///   Backspace = DEL 0x7f, Enter = CR, Tab, Esc, Home/End/Delete/PageUp/PageDown).
/// - Anything else → the key's typed character (`key_char`), e.g. plain letters / symbols.
pub(crate) fn key_bytes(key: &str, control: bool, key_char: Option<&str>) -> Option<Vec<u8>> {
    if control && key.len() == 1 {
        if let Some(c) = key.chars().next() {
            let lc = c.to_ascii_lowercase();
            if lc.is_ascii_alphabetic() {
                return Some(vec![(lc as u8 - b'a') + 1]);
            }
        }
    }
    match key {
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
        _ => key_char.map(|s| s.as_bytes().to_vec()),
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shell_uses_comspec_with_cmd_fallback() {
        assert_eq!(
            windows_shell_from_comspec(Some(r"C:\Windows\System32\cmd.exe".to_string())),
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(windows_shell_from_comspec(None), "cmd.exe");
        assert_eq!(
            windows_shell_from_comspec(Some("  ".to_string())),
            "cmd.exe"
        );
    }

    /// Off macOS a missing `SHELL` falls back to the login shell, then `/bin/sh` — never to
    /// a `/bin/zsh` that most Linux systems don't have.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn shell_falls_back_to_the_login_shell_then_sh() {
        let none = || -> Option<String> { None };
        let fish = || Some("/usr/bin/fish".to_string());
        assert_eq!(shell_from(Some("/bin/bash".into()), fish), "/bin/bash");
        assert_eq!(shell_from(None, fish), "/usr/bin/fish");
        assert_eq!(shell_from(Some(" ".into()), fish), "/usr/bin/fish");
        assert_eq!(shell_from(None, none), "/bin/sh");
        // The real lookup: this user's entry, when the system has one, is an absolute path.
        assert!(passwd_shell().is_none_or(|s| s.starts_with('/')));
    }

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

    /// Perf guard (see CLAUDE.md "Performance regression tests"): find re-scans the whole
    /// buffer per keystroke / step, so a full 10k-line scrollback must scan fast. Also pins the
    /// query being literal (`a.b` isn't a regex) and smart-case (lowercase = any case).
    #[test]
    fn perf_find_scans_full_scrollback() {
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(120, 40),
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        let mut out = String::new();
        for i in 0..10_000 {
            let tag = if i % 2 == 0 { "a.b" } else { "AxB" };
            out.push_str(&format!(
                "line {i} {tag} the quick brown fox jumps over\r\n"
            ));
        }
        parser.advance(&mut term, out.as_bytes());
        let count = |q: &str, term: &Term<_>| {
            let mut re = RegexSearch::new(&literal_regex(q)).unwrap();
            scan_all(term, &mut re).len()
        };
        let t0 = Instant::now();
        let fox = count("fox", &term);
        let took = t0.elapsed();
        // 10k written, minus the lines scrolled past the 10k-line history cap.
        assert!(fox > 9_900, "{fox}");
        assert!(
            took < Duration::from_secs(2),
            "full-buffer find took {took:?}"
        );
        assert_eq!(count("a.b", &term), fox / 2 + fox % 2, "a.b is literal");
        assert_eq!(count("axb", &term), fox / 2, "lowercase matches AxB");
        assert_eq!(count("AXB", &term), 0, "uppercase is case-sensitive");
    }

    /// A find's current match follows output up the scrollback, and is gone once the
    /// scrollback is cleared — even from a scrolled-back view, whose offset drops with it.
    #[test]
    fn find_match_follows_output_and_goes_with_a_clear() {
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(40, 5),
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        let mut re = RegexSearch::new(&literal_regex("needle")).unwrap();
        parser.advance(&mut term, b"needle\r\n");
        let m = scan_all(&term, &mut re).remove(0);
        let cur = (m, term.grid().history_size(), 0);
        parser.advance(&mut term, b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n");
        let now = shifted(&term, &cur).unwrap();
        assert_eq!(now, scan_all(&term, &mut re)[0]);
        term.scroll_display(Scroll::Delta(3));
        let cur = (now, term.grid().history_size(), 3);
        // What `clear` sends.
        parser.advance(&mut term, b"\x1b[H\x1b[2J\x1b[3J");
        assert_eq!(shifted(&term, &cur), None);
    }

    /// A headless terminal that has been sent `bytes`.
    fn term_with(
        cols: usize,
        rows: usize,
        bytes: &[u8],
    ) -> Term<alacritty_terminal::event::VoidListener> {
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    /// Your prompts are found the way Claude Code echoes them — "❯ " on a tint, a long one
    /// wrapping onto indented rows — but not a shell's "❯" prompt or Claude's input box; and
    /// ⌘↑ / ⌘↓ step through them, a row of air above each.
    #[test]
    fn prompts_are_found_and_stepped_through() {
        let tint = "\x1b[48;2;55;55;55m";
        let mut out = String::from("❯ claude\r\n\r\n");
        out.push_str(&format!(
            "{tint}❯ first, long enough to\r\n  wrap\x1b[49m\r\n\r\n"
        ));
        for i in 0..30 {
            out.push_str(&format!("⏺ reply {i}\r\n"));
        }
        out.push_str(&format!("\r\n{tint}❯ second\x1b[49m\r\n\r\n"));
        for i in 0..20 {
            out.push_str(&format!("⏺ more {i}\r\n"));
        }
        out.push_str("────\r\n❯\u{a0}\r\n────");
        let term = term_with(40, 10, out.as_bytes());

        let prompts = prompt_lines(&term);
        assert_eq!(prompts.len(), 2, "{prompts:?}");
        let (first, second) = (prompts[0], prompts[1]);
        assert_eq!(prompt_text(&term, second), "second");
        assert_eq!(last_prompt(&term), Some(second));
        // The wrapped row is part of the prompt, even with the view's top on it.
        let wrap = Line(first.0 + 1);
        let rows: Vec<bool> = prompt_rows(&term, first, 3)
            .iter()
            .map(Option::is_some)
            .collect();
        assert_eq!(rows, [true, true, false]);
        assert!(prompt_rows(&term, wrap, 1)[0].is_some());

        // From the live screen: up to the second, the first, then no further.
        let (a, b) = (first.0 - 1, second.0 - 1);
        assert_eq!(prompt_jump(&prompts, 0, true), Some(b));
        assert_eq!(prompt_jump(&prompts, b, true), Some(a));
        assert_eq!(prompt_jump(&prompts, a, true), None);
        assert_eq!(prompt_jump(&prompts, a, false), Some(b));
        assert_eq!(prompt_jump(&prompts, b, false), None);
    }

    /// Perf guard (see CLAUDE.md "Performance regression tests"): every frame finds the
    /// prompts on screen and every render looks for your last one — through a full 10k-line
    /// scrollback when there's none.
    #[test]
    fn perf_prompt_scan_full_scrollback() {
        let mut out = String::new();
        for i in 0..10_000 {
            out.push_str(&format!("line {i} the quick brown fox jumps over\r\n"));
        }
        let term = term_with(120, 40, out.as_bytes());
        let t0 = Instant::now();
        assert_eq!(last_prompt(&term), None);
        assert!(prompt_lines(&term).is_empty());
        let top = term.topmost_line();
        assert!(prompt_rows(&term, top, 40).iter().all(Option::is_none));
        let took = t0.elapsed();
        assert!(took < Duration::from_secs(2), "prompt scan took {took:?}");
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

    fn dropped(ps: &[&str]) -> String {
        shell_quoted(&ps.iter().map(PathBuf::from).collect::<Vec<_>>())
    }

    /// A name the tty or line editor would act on types nothing, on every OS: Ctrl-C then a
    /// command and CR, and an early end of bracketed paste (ESC and C1 CSI forms).
    #[test]
    fn dropped_paths_with_control_chars_type_nothing() {
        let crafted = [
            "/tmp/a\x03cmd\r.png",
            "/tmp/b\x1b[201~.png",
            "/tmp/c\u{9b}201~.png",
        ];
        assert_eq!(dropped(&crafted), "");
        let mixed = dropped(&["/tmp/a.png", crafted[0], crafted[1], "/tmp/b.png"]);
        assert!(!mixed.contains(char::is_control), "{mixed:?}");
        assert!(
            mixed.contains("a.png") && mixed.contains("b.png"),
            "{mixed:?}"
        );
    }

    /// A non-UTF-8 name types nothing (a lossy copy would point at no file).
    #[cfg(unix)]
    #[test]
    fn dropped_non_utf8_path_types_nothing() {
        use std::os::unix::ffi::OsStrExt;
        let latin1 = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9.png"));
        assert_eq!(shell_quoted(&[latin1]), "");
    }

    /// Dropped paths are typed in as one shell word each, whatever printable characters they hold.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn dropped_paths_are_single_quoted_shell_words() {
        assert_eq!(dropped(&[]), "");
        assert_eq!(dropped(&["/tmp/shot.png"]), "'/tmp/shot.png' ");
        assert_eq!(
            dropped(&["/tmp/My Shots/a b.png"]),
            "'/tmp/My Shots/a b.png' "
        );
        assert_eq!(dropped(&["/tmp/it's.png"]), r"'/tmp/it'\''s.png' ");
        assert_eq!(dropped(&["/tmp/café 日本.png"]), "'/tmp/café 日本.png' ");
        let all = [
            "/a",
            "/My Shots/b c.png",
            "/it's",
            "/\"$HOME\" `x` \\y",
            "/café 日本.png",
        ];
        assert_eq!(
            dropped(&all),
            "'/a' '/My Shots/b c.png' '/it'\\''s' '/\"$HOME\" `x` \\y' '/café 日本.png' "
        );
        // A POSIX shell reads each back as exactly the path, nothing expanded.
        #[cfg(unix)]
        {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf '%s\\n' {}", dropped(&all)))
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                all.join("\n") + "\n"
            );
        }
    }

    /// cmd.exe doesn't quote with `'`, so on Windows each path is double-quoted.
    #[cfg(target_os = "windows")]
    #[test]
    fn dropped_paths_are_double_quoted_on_windows() {
        assert_eq!(dropped(&[]), "");
        assert_eq!(dropped(&[r"C:\shot.png"]), "\"C:\\shot.png\" ");
        assert_eq!(
            dropped(&[r"C:\My Shots\it's.png", r"C:\café 日本.png"]),
            "\"C:\\My Shots\\it's.png\" \"C:\\café 日本.png\" "
        );
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

    #[test]
    fn key_bytes_translates_named_keys_and_control() {
        // Backspace = DEL (0x7f) — the byte a Unix shell expects (the thing the user hit).
        assert_eq!(key_bytes("backspace", false, None), Some(vec![0x7f]));
        assert_eq!(key_bytes("enter", false, None), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("tab", false, None), Some(b"\t".to_vec()));
        assert_eq!(key_bytes("escape", false, None), Some(vec![0x1b]));
        // Arrows = CSI sequences (Up/Down drive shell history).
        assert_eq!(key_bytes("up", false, None), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes("down", false, None), Some(b"\x1b[B".to_vec()));
        assert_eq!(key_bytes("left", false, None), Some(b"\x1b[D".to_vec()));
        assert_eq!(key_bytes("right", false, None), Some(b"\x1b[C".to_vec()));
        // Ctrl-C = 0x03 (SIGINT), Ctrl-D = 0x04 (EOF → shell exits → tab closes), Ctrl-A = 0x01.
        assert_eq!(key_bytes("c", true, Some("c")), Some(vec![0x03]));
        assert_eq!(key_bytes("d", true, Some("d")), Some(vec![0x04]));
        assert_eq!(key_bytes("a", true, Some("a")), Some(vec![0x01]));
        // Plain typed char falls through to key_char; an unknown key with no char → None.
        assert_eq!(key_bytes("a", false, Some("a")), Some(b"a".to_vec()));
        assert_eq!(key_bytes("f5", false, None), None);
    }

    /// Claude's input box, drawn the way a real session draws it: a greyed suggestion with the
    /// cursor parked before it is one, ending after its text; the same words typed are not.
    #[test]
    fn claude_prompt_suggestion_is_the_dim_text_under_the_cursor() {
        use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
        let box_with = |text: &str| {
            let mut term = Term::new(
                Config::default(),
                &TermSize::new(40, 4),
                alacritty_terminal::event::VoidListener,
            );
            let bytes = format!("\r\n\u{276f}\u{a0}{text}\r\n\u{2500}\u{2500}\r\x1b[2C\x1b[1A");
            Processor::<StdSyncHandler>::new().advance(&mut term, bytes.as_bytes());
            prompt_suggestion(&term)
        };
        assert_eq!(
            box_with("\x1b[2myes commit it\x1b[22m"),
            Some(("yes commit it".into(), GridPoint::new(Line(1), Column(15))))
        );
        assert_eq!(box_with("yes commit it"), None);
    }

    /// Enter on a suggestion sends it, as it would typed text — Claude alone submits nothing.
    /// Backspace clears it whole rather than taking it and deleting its last character.
    #[test]
    fn keys_on_claude_prompt_suggestion() {
        assert_eq!(on_suggestion(b"\r"), Some(OnSuggestion::Accept));
        assert_eq!(on_suggestion(&[0x7f]), Some(OnSuggestion::Clear));
        assert_eq!(on_suggestion(b"\x1b"), None);
    }
}
