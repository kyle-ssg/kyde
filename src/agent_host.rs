//! Agent hosts: what keeps agents running when Kyde quits.
//!
//! Every agent tab runs inside a small host process — this same binary, started as
//! `kyde --agent-host <spec> <fd>` — that owns the tab's PTY and shell, keeps the last few MB
//! of output, and serves them on a Unix socket in `~/.kyde/agents/<id>.sock`. Kyde's terminal
//! view is a client: it attaches, gets the buffered output replayed, then the live stream, and
//! sends keystrokes / resizes back. Quitting Kyde only disconnects, and a host nobody is
//! attached to keeps its agent just as long as it has something to finish (`watch`): a turn
//! under way stays until it's done, then the host ends it, as it does an idle one
//! straight away. The next launch releases the ones with nothing to finish (`release`: a
//! quick restart finds them still up), reattaches to the ones at work (`live_hosts`) and
//! resumes a session when its worktree is opened. Closing the tab sends `KILL`, which ends the agent and
//! everything it started (`end_tree`). The `ky agent …` CLI talks to the same sockets.
//!
//! Wire format, both directions: `[kind: u8][len: u32 LE][payload]`.

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

/// Bumped when the wire format changes (a host from an older Kyde says so in `HELLO`).
/// 2: a host answers `RELEASE`.
pub(crate) const VERSION: u32 = 2;
/// Output a host keeps for a reattaching viewer.
const RING_CAP: usize = 4 * 1024 * 1024;
/// What a Claude Code session exports to the commands it runs. An agent host started from
/// inside one (Kyde launched from a Claude Code terminal, or an agent's own `ky agent new`)
/// must not hand these on: the agent would think it's that session's child — and, for one,
/// stop saving its transcript (so it couldn't be resumed).
const PARENT_SESSION_VARS: [&str; 10] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
];

/// Largest frame accepted — anything bigger is a broken stream.
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// How long a closed agent gets to wind down after the hang-up (Claude stopping its MCP
/// servers and commands) before whatever it left running is stopped for it.
const HANGUP_GRACE: Duration = Duration::from_secs(5);
/// Then how long SIGTERM gets before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(2);
/// How often a host checks whether its agent is still watched or at work (`watch`).
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Client → host: start streaming (answered with `HELLO`, the buffered `OUT`, then live).
pub(crate) const ATTACH: u8 = 1;
/// Client → host: bytes for the PTY, as if typed.
pub(crate) const INPUT: u8 = 2;
/// Client → host: cols, rows, cell width, cell height — four `u16` LE.
pub(crate) const RESIZE: u8 = 3;
/// Client → host: send the buffered output once, then hang up.
pub(crate) const SNAPSHOT: u8 = 4;
/// Client → host: end the agent and everything it started (see `end_tree`).
pub(crate) const KILL: u8 = 5;
/// Client → host: end the agent unless it has work to finish. Answered with `EXIT` when it's
/// ending, `HELLO` when it stays.
pub(crate) const RELEASE: u8 = 6;
/// Host → client: `HostHello` JSON.
pub(crate) const HELLO: u8 = 10;
/// Host → client: PTY output.
pub(crate) const OUT: u8 = 11;
/// Host → client: the agent's terminal closed.
pub(crate) const EXIT: u8 = 12;

/// What a host runs.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct HostSpec {
    pub id: String,
    pub cwd: PathBuf,
    pub shell: String,
    pub env: Vec<(String, String)>,
    /// Typed into the shell once it starts (the agent's start command).
    pub line: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Runs the start command (vs a plain shell tab).
    pub agent: bool,
}

/// A running host, as it registers itself in `<id>.host.json`.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct HostInfo {
    pub version: u32,
    pub id: String,
    /// The host process.
    pub pid: u32,
    /// The shell it runs (what an agent runs under).
    pub child: u32,
    pub cwd: PathBuf,
    pub agent: bool,
    /// Unix seconds.
    pub started: u64,
}

/// The first frame a viewer gets.
#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct HostHello {
    pub version: u32,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
}

pub(crate) fn sock_path(id: &str) -> PathBuf {
    kyde_config::agents::agents_dir().join(format!("{id}.sock"))
}

fn info_path(id: &str) -> PathBuf {
    kyde_config::agents::agents_dir().join(format!("{id}.host.json"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Write one frame.
pub(crate) fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| io::Error::other("frame too large"))?;
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

/// Reassembles frames from a byte stream.
#[derive(Default)]
pub(crate) struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// The next whole frame. `Ok(None)` when a read timed out first (a set read timeout);
    /// an error at end of stream.
    pub(crate) fn next(&mut self, r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            if let Some(f) = self.take()? {
                return Ok(Some(f));
            }
            match r.read(&mut chunk) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn take(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let Some(head) = self.buf.get(..5) else {
            return Ok(None);
        };
        let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > MAX_FRAME {
            return Err(io::Error::other("oversized frame"));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let kind = self.buf[0];
        let payload = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        Ok(Some((kind, payload)))
    }
}

/// A `RESIZE` payload.
pub(crate) fn resize_payload(ws: WindowSize) -> [u8; 8] {
    let mut p = [0u8; 8];
    p[0..2].copy_from_slice(&ws.num_cols.to_le_bytes());
    p[2..4].copy_from_slice(&ws.num_lines.to_le_bytes());
    p[4..6].copy_from_slice(&ws.cell_width.to_le_bytes());
    p[6..8].copy_from_slice(&ws.cell_height.to_le_bytes());
    p
}

fn parse_resize(p: &[u8]) -> Option<WindowSize> {
    let v = |i: usize| Some(u16::from_le_bytes([*p.get(i)?, *p.get(i + 1)?]));
    Some(WindowSize {
        num_cols: v(0)?,
        num_lines: v(2)?,
        cell_width: v(4)?,
        cell_height: v(6)?,
    })
}

// ── Kyde side: start / find / poke hosts ─────────────────────────────────────────────────

/// Start a host for `spec`, detached (it outlives Kyde). Kyde binds the socket and hands it
/// over, so a client can connect the moment this returns.
pub(crate) fn spawn(spec: &HostSpec) -> io::Result<()> {
    let dir = kyde_config::agents::agents_dir();
    std::fs::create_dir_all(&dir)?;
    let sock = sock_path(&spec.id);
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    let fd = listener.as_raw_fd();
    // std opens sockets close-on-exec; the host must inherit this one.
    // SAFETY: `fd` is the open listening socket owned by `listener` (alive for both calls);
    // F_GETFD/F_SETFD only read/write its descriptor flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let log = std::fs::File::create(dir.join(format!("{}.log", spec.id)))?;
    let json = serde_json::to_string(spec).map_err(io::Error::other)?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    for var in PARENT_SESSION_VARS {
        cmd.env_remove(var);
    }
    let mut child = cmd
        .arg("--agent-host")
        .arg(json)
        .arg(fd.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()?;
    drop(listener); // the host has its own copy
                    // Reap it if it ends while Kyde runs (no zombie); after Kyde quits, launchd adopts it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Wait (up to 2 s) for a just-spawned host to register — the CLI's `new`, so a `prompt`
/// straight after finds it. (Kyde itself needn't: it connects to the socket it handed over.)
pub(crate) fn wait_registered(id: &str) {
    let info = info_path(id);
    for _ in 0..100 {
        if info.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    i32::try_from(pid).is_ok_and(|p| p > 0 && unsafe { libc::kill(p, 0) } == 0)
}

/// Every host still running (registered, process alive, socket there). Clears out what dead
/// ones left behind. Reads the agents dir: call it off the UI thread.
pub(crate) fn live_hosts() -> Vec<HostInfo> {
    let dir = kyde_config::agents::agents_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(id) = name.strip_suffix(".host.json") else {
            continue;
        };
        let info: Option<HostInfo> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        match info {
            Some(h) if alive(h.pid) && sock_path(&h.id).exists() => out.push(h),
            _ => {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(sock_path(id));
            }
        }
    }
    out.sort_by_key(|h| h.started);
    out
}

/// Connect to host `id` and send one frame (`INPUT`, `KILL`).
pub(crate) fn send(id: &str, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut s = UnixStream::connect(sock_path(id))?;
    write_frame(&mut s, kind, payload)
}

/// Launch: let go of host `info` unless its agent has work to finish. Whether it's ending
/// (or gone already) — `false` when it stays. Reads the agent's state file: call it off the
/// UI thread.
pub(crate) fn release(info: &HostInfo) -> bool {
    if info.version < 2 {
        // From a Kyde too old to be asked: go by what its agent last reported.
        let idle = info.agent
            && std::fs::read_to_string(crate::agents::state_path(&info.id))
                .is_ok_and(|s| crate::agents::reports_idle(&s));
        return idle && send(&info.id, KILL, &[]).is_ok();
    }
    let Ok(mut s) = UnixStream::connect(sock_path(&info.id)) else {
        return true;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if write_frame(&mut s, RELEASE, &[]).is_err() {
        return true;
    }
    // No answer in time: it stays.
    !matches!(
        FrameReader::default().next(&mut s),
        Ok(Some((HELLO, _)) | None)
    )
}

/// Host `id`'s size and buffered output (everything it has shown recently, raw).
pub(crate) fn snapshot(id: &str) -> io::Result<(HostHello, Vec<u8>)> {
    let mut s = UnixStream::connect(sock_path(id))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    write_frame(&mut s, SNAPSHOT, &[])?;
    let mut frames = FrameReader::default();
    let bad = || io::Error::other("no snapshot from the agent host");
    let hello = match frames.next(&mut s)? {
        Some((HELLO, json)) => serde_json::from_slice(&json).map_err(|_| bad())?,
        _ => return Err(bad()),
    };
    match frames.next(&mut s)? {
        Some((OUT, bytes)) => Ok((hello, bytes)),
        _ => Err(bad()),
    }
}

// ── the host process ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Ring {
    buf: VecDeque<u8>,
}

impl Ring {
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
        let over = self.buf.len().saturating_sub(RING_CAP);
        if over > 0 {
            self.buf.drain(..over);
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

struct Shared {
    id: String,
    /// Recent output. Its lock also orders a new viewer's replay before any live output.
    ring: Mutex<Ring>,
    viewers: Mutex<Vec<UnixStream>>,
    /// Attached connections still open: somebody (Kyde) is watching. Not `viewers`: one too
    /// slow to take output is dropped from that list, but is still there.
    watchers: AtomicUsize,
    writer: Mutex<std::fs::File>,
    pty: Mutex<tty::Pty>,
    /// The PTY master (`pty`'s, open as long as it is).
    master: i32,
    size: Mutex<(u16, u16)>,
    child: u32,
    /// Runs the start command (vs a plain shell tab).
    agent: bool,
    started: Instant,
    last_output: Mutex<Instant>,
    /// The agent's latest hook report (`watch` keeps it current).
    report: Mutex<Option<Report>>,
    /// When somebody was last there (a viewer attached, a client sent something) or the agent
    /// had work to finish. `None` once the host has given up on it and is ending it (`watch`).
    attended: Mutex<Option<Instant>>,
    /// Set (and `closed_cv` signalled) once the agent's terminal has closed.
    closed: Mutex<bool>,
    closed_cv: Condvar,
    /// Held while a `KILL` ends the agent, so the host stays up until that's done.
    ending: Mutex<()>,
}

impl Shared {
    /// Signal the agent's whole process group (the shell is its session leader).
    fn signal(&self, sig: i32) {
        signal_group(self.child, sig);
    }

    /// Wait up to `max` for the agent's terminal to close; whether it has.
    fn wait_closed(&self, max: Duration) -> bool {
        let closed = lock(&self.closed);
        match self.closed_cv.wait_timeout_while(closed, max, |c| !*c) {
            Ok((c, _)) => *c,
            Err(p) => *p.into_inner().0,
        }
    }

    /// `KILL`: end the agent and everything it started. A second one meanwhile is a no-op.
    fn end(&self) {
        let _ending = match self.ending.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        end_tree(
            self.child,
            || {
                self.wait_closed(HANGUP_GRACE);
            },
            TERM_GRACE,
        );
        // Something from outside the tree still holds the terminal open: go anyway. Exiting
        // drops the PTY, which hangs up whatever is left on it.
        if !self.wait_closed(Duration::from_secs(1)) {
            self.unregister();
            std::process::exit(0);
        }
    }

    /// Somebody is there (or the agent is at work): the unwatched clock starts over. `false`
    /// once the host has given up and is ending the agent.
    fn touch(&self) -> bool {
        match &mut *lock(&self.attended) {
            Some(at) => {
                *at = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Whether something runs in the shell (the agent, or a command): the terminal's
    /// foreground process group isn't the shell's own.
    fn running(&self) -> bool {
        // SAFETY: reads the foreground process group of the PTY master this process owns,
        // open as long as `self.pty`.
        let fg = unsafe { libc::tcgetpgrp(self.master) };
        // A failed probe counts as running: never a reason to end an agent.
        u32::try_from(fg) != Ok(self.child)
    }

    /// Whether the agent has something to finish (`agents::has_work`).
    fn has_work(&self) -> bool {
        let report = lock(&self.report);
        let report = report.as_ref();
        let now = std::time::SystemTime::now();
        crate::agents::has_work(&crate::agents::Unwatched {
            agent: self.agent,
            running: self.running(),
            hook: report.map(|r| {
                // Background agents report to files of their own.
                let at = if r.background {
                    r.at.max(newest_in(&crate::agents::sub_dir(&self.id)))
                } else {
                    r.at
                };
                (r.hook.clone(), now.duration_since(at).unwrap_or_default())
            }),
            background: report.is_some_and(|r| r.background),
            quiet: lock(&self.last_output).elapsed(),
            age: self.started.elapsed(),
        })
    }

    /// Give up on the agent, ahead of ending it (`end`): no viewer from here on, and it's
    /// unregistered at once — a Kyde launching now resumes its session rather than attaching
    /// to an agent on its way out. `false` if that's under way already.
    fn abandon(&self) -> bool {
        if lock(&self.attended).take().is_none() {
            return false;
        }
        let _ = std::fs::remove_file(sock_path(&self.id));
        let _ = std::fs::remove_file(info_path(&self.id));
        true
    }

    /// The agent's terminal closed (or is being abandoned): tell viewers and unregister.
    fn unregister(&self) {
        for v in lock(&self.viewers).iter_mut() {
            let _ = write_frame(v, EXIT, &[]);
        }
        let _ = std::fs::remove_file(sock_path(&self.id));
        let _ = std::fs::remove_file(info_path(&self.id));
        // Its log (our stderr) only stays if something went wrong.
        let log = kyde_config::agents::agents_dir().join(format!("{}.log", self.id));
        if std::fs::metadata(&log).is_ok_and(|m| m.len() == 0) {
            let _ = std::fs::remove_file(log);
        }
    }
}

// ── ending an agent nobody is watching ───────────────────────────────────────────────────

/// The agent's latest hook report, as the host keeps it.
struct Report {
    hook: crate::agents::Hook,
    /// Its last finished turn left background tasks running.
    background: bool,
    /// When it was written.
    at: std::time::SystemTime,
}

/// When the newest file in `dir` was written (the epoch if there's none).
fn newest_in(dir: &std::path::Path) -> std::time::SystemTime {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().and_then(|m| m.modified()).ok())
        .max()
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Until the agent's terminal closes: end the agent once nobody has been attached and it has
/// had nothing to finish for `UNWATCHED_GRACE` — Kyde quit (or never knew it), and its
/// session is resumed at the next launch. One still at work is kept until it's done.
fn watch(shared: &Shared) {
    let state = crate::agents::state_path(&shared.id);
    let mut read = None;
    while !shared.wait_closed(WATCH_EVERY) {
        // Kept across reads: an event that says nothing of its state (a notification)
        // overwrites the file too.
        let at = std::fs::metadata(&state).and_then(|m| m.modified()).ok();
        if let Some(at) = at.filter(|_| at != read) {
            read = Some(at);
            let new = std::fs::read_to_string(&state)
                .ok()
                .and_then(|s| crate::agents::parse_hook(&s));
            if let Some(r) = new {
                let mut report = lock(&shared.report);
                let was = report.as_ref().is_some_and(|r| r.background);
                *report = Some(Report {
                    background: crate::agents::left_background(was, &r),
                    hook: r.hook,
                    at,
                });
            }
        }
        let watched = shared.watchers.load(Ordering::Relaxed) > 0;
        if watched || shared.has_work() {
            shared.touch();
            continue;
        }
        let unattended = lock(&shared.attended).map(|at| at.elapsed());
        if unattended.is_some_and(|d| d >= crate::agents::UNWATCHED_GRACE) && shared.abandon() {
            shared.end();
            return;
        }
    }
}

// ── ending an agent's process tree ───────────────────────────────────────────────────────

/// A process, told apart from a later one that reuses its pid by its start time.
#[derive(Clone, Debug)]
struct Proc {
    pid: u32,
    ppid: u32,
    start: String,
}

/// Every running process, per `ps` (zombies left out: they've already ended).
fn processes() -> Vec<Proc> {
    let Ok(out) = std::process::Command::new("ps")
        .args([
            "-A", "-o", "pid=", "-o", "ppid=", "-o", "stat=", "-o", "lstart=",
        ])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let ppid = f.next()?.parse().ok()?;
            if f.next()?.starts_with('Z') {
                return None;
            }
            let start = f.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, ppid, start })
        })
        .collect()
}

/// `root` and every process under it, as they are now.
fn proc_tree(root: u32) -> Vec<Proc> {
    let all = processes();
    let mut tree: Vec<Proc> = all.iter().filter(|p| p.pid == root).cloned().collect();
    let mut i = 0;
    while let Some(parent) = tree.get(i).map(|p| p.pid) {
        tree.extend(
            all.iter()
                .filter(|p| p.ppid == parent && p.pid != parent)
                .cloned(),
        );
        i += 1;
    }
    tree
}

/// Which of `procs` are still running (the same process: pid *and* start time).
fn still_running(procs: &[Proc]) -> Vec<Proc> {
    if procs.is_empty() {
        return Vec::new();
    }
    let now = processes();
    procs
        .iter()
        .filter(|p| now.iter().any(|n| n.pid == p.pid && n.start == p.start))
        .cloned()
        .collect()
}

fn signal_pid(pid: u32, sig: i32) {
    if let Ok(p) = i32::try_from(pid) {
        if p > 0 {
            // SAFETY: plain kill(2) on one process.
            unsafe {
                libc::kill(p, sig);
            }
        }
    }
}

fn signal_group(pgid: u32, sig: i32) {
    if let Ok(p) = i32::try_from(pgid) {
        if p > 0 {
            // SAFETY: plain kill(2) on one process group.
            unsafe {
                libc::kill(-p, sig);
            }
        }
    }
}

/// End `root` (an agent's shell) and everything under it, so closing a tab leaves nothing
/// running. The tree is read *before* the hang-up: once a parent exits, its children are
/// re-parented to launchd and can no longer be traced back. The hang-up reaches only the
/// shell's process group, and Claude runs each tool command (a dev server, say) in a session
/// of its own, so after `wait_hangup` (the agent winding down) whatever is still running gets
/// SIGTERM, then — `term_grace` later — SIGKILL.
fn end_tree(root: u32, wait_hangup: impl FnOnce(), term_grace: Duration) {
    let tree = proc_tree(root);
    signal_group(root, libc::SIGHUP);
    wait_hangup();
    let mut left = still_running(&tree);
    for p in &left {
        signal_pid(p.pid, libc::SIGTERM);
    }
    let until = Instant::now() + term_grace;
    while !left.is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
        left = still_running(&left);
    }
    for p in &left {
        signal_pid(p.pid, libc::SIGKILL);
    }
}

/// `kyde --agent-host <spec> <fd>`: run the host until its agent's terminal closes.
pub(crate) fn host_main(spec: &str, fd: &str) -> ! {
    let code = match run(spec, fd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kyde agent host: {e}");
            1
        }
    };
    std::process::exit(code)
}

fn run(spec: &str, fd: &str) -> io::Result<()> {
    // Its own session, deaf to Kyde's terminal hanging up; a viewer vanishing mid-write is an
    // error to handle, not a reason to die.
    // SAFETY: process-wide signal dispositions and a new session for this process only.
    unsafe {
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let fd: i32 = fd
        .parse()
        .map_err(|_| io::Error::other("bad listener fd"))?;
    // SAFETY: Kyde passed this listening socket's descriptor (inherited); nothing else in this
    // process owns it.
    let listener = unsafe { UnixListener::from_raw_fd(fd) };
    let spec: HostSpec = serde_json::from_str(spec).map_err(io::Error::other)?;
    let env = spec.env.iter().cloned().collect();
    #[cfg(target_os = "macos")]
    let env = crate::platform::locale::with_lang(env);
    let options = PtyOptions {
        shell: Some(Shell::new(spec.shell.clone(), Vec::new())),
        working_directory: Some(spec.cwd.clone()),
        drain_on_exit: false,
        env,
    };
    let ws = WindowSize {
        num_lines: spec.rows,
        num_cols: spec.cols,
        cell_width: 8,
        cell_height: 16,
    };
    let pty = tty::new(&options, ws, 0)?;
    let child = pty.child().id();
    // alacritty opens the master non-blocking (it polls); the host reads it on a thread.
    let master_fd = pty.file().as_raw_fd();
    // SAFETY: F_GETFL/F_SETFL on the PTY master `pty` owns, alive for both calls.
    unsafe {
        let flags = libc::fcntl(master_fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(master_fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
    let master = pty.file().try_clone()?;
    let writer = pty.file().try_clone()?;
    let shared = Arc::new(Shared {
        id: spec.id.clone(),
        ring: Mutex::new(Ring::default()),
        viewers: Mutex::new(Vec::new()),
        watchers: AtomicUsize::new(0),
        writer: Mutex::new(writer),
        pty: Mutex::new(pty),
        master: master_fd,
        size: Mutex::new((spec.cols, spec.rows)),
        child,
        agent: spec.agent,
        started: Instant::now(),
        last_output: Mutex::new(Instant::now()),
        report: Mutex::new(None),
        attended: Mutex::new(Some(Instant::now())),
        closed: Mutex::new(false),
        closed_cv: Condvar::new(),
        ending: Mutex::new(()),
    });
    let info = HostInfo {
        version: VERSION,
        id: spec.id.clone(),
        pid: std::process::id(),
        child,
        cwd: spec.cwd.clone(),
        agent: spec.agent,
        started: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    std::fs::write(
        info_path(&spec.id),
        serde_json::to_string(&info).map_err(io::Error::other)?,
    )?;
    {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve(stream, &shared));
            }
        });
    }
    {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || watch(&shared));
    }
    // Typed beside `pump`, never before it: past ~1 KB the write waits for the shell to read,
    // and a starting shell can't while its prompt is stuck in output nobody drains. The
    // writer is held until it's all in, so a viewer's keys land after it.
    if let Some(line) = spec.line {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            if let Err(e) = lock(&shared.writer).write_all(format!("{line}\n").as_bytes()) {
                eprintln!("kyde agent host: typing the start line: {e}");
            }
        });
    }
    pump(master, &shared);
    // The agent's terminal closed: let a `KILL` under way know, and outlast it.
    *lock(&shared.closed) = true;
    shared.closed_cv.notify_all();
    drop(lock(&shared.ending));
    // Then unregister and go (dropping the PTY hangs up anything left in it).
    shared.unregister();
    Ok(())
}

/// PTY output → the ring + every viewer, until the terminal closes.
fn pump(mut master: std::fs::File, shared: &Shared) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let bytes = &buf[..n];
        let mut ring = lock(&shared.ring);
        ring.push(bytes);
        *lock(&shared.last_output) = Instant::now();
        lock(&shared.viewers).retain_mut(|v| write_frame(v, OUT, bytes).is_ok());
    }
}

/// One connection: an attaching viewer, or a one-shot request (snapshot, input, kill).
fn serve(mut stream: UnixStream, shared: &Shared) {
    // A viewer that stops reading is dropped rather than stalling the agent.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut frames = FrameReader::default();
    // Whether this connection attached: a watcher until it hangs up.
    let mut attached = false;
    while let Ok(Some((kind, payload))) = frames.next(&mut stream) {
        // Whoever this is, somebody's there. (Not once the agent is being ended: no viewer
        // then, so Kyde resumes its session instead.)
        if !shared.touch() && kind != KILL {
            break;
        }
        match kind {
            ATTACH => {
                let Ok(mut out) = stream.try_clone() else {
                    break;
                };
                let ring = lock(&shared.ring);
                let (cols, rows) = *lock(&shared.size);
                let hello = HostHello {
                    version: VERSION,
                    pid: shared.child,
                    cols,
                    rows,
                };
                let hello = serde_json::to_vec(&hello).unwrap_or_default();
                if write_frame(&mut out, HELLO, &hello).is_ok()
                    && write_frame(&mut out, OUT, &ring.bytes()).is_ok()
                {
                    lock(&shared.viewers).push(out);
                    if !std::mem::replace(&mut attached, true) {
                        shared.watchers.fetch_add(1, Ordering::Relaxed);
                    }
                }
                drop(ring);
                // Repaint for the new viewer (the replay may start mid-screen).
                shared.signal(libc::SIGWINCH);
            }
            INPUT => {
                let _ = lock(&shared.writer).write_all(&payload);
            }
            RESIZE => {
                if let Some(ws) = parse_resize(&payload) {
                    lock(&shared.pty).on_resize(ws);
                    *lock(&shared.size) = (ws.num_cols, ws.num_lines);
                }
            }
            SNAPSHOT => {
                let bytes = lock(&shared.ring).bytes();
                let (cols, rows) = *lock(&shared.size);
                let hello = HostHello {
                    version: VERSION,
                    pid: shared.child,
                    cols,
                    rows,
                };
                let hello = serde_json::to_vec(&hello).unwrap_or_default();
                let _ = write_frame(&mut stream, HELLO, &hello)
                    .and_then(|()| write_frame(&mut stream, OUT, &bytes));
                break;
            }
            KILL => {
                shared.end();
                break;
            }
            RELEASE => {
                // Kyde is launching and won't show this agent: gone now, not a grace later.
                // (Never from under a viewer: somebody is looking at it.)
                let leaving = shared.watchers.load(Ordering::Relaxed) == 0
                    && !shared.has_work()
                    && shared.abandon();
                let _ = write_frame(&mut stream, if leaving { EXIT } else { HELLO }, &[]);
                if leaving {
                    shared.end();
                }
                break;
            }
            _ => {}
        }
    }
    if attached {
        shared.watchers.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_survive_being_split_and_joined() {
        let mut wire = Vec::new();
        write_frame(&mut wire, OUT, b"hello").unwrap();
        write_frame(&mut wire, INPUT, b"").unwrap();
        write_frame(
            &mut wire,
            RESIZE,
            &resize_payload(WindowSize {
                num_cols: 120,
                num_lines: 40,
                cell_width: 8,
                cell_height: 17,
            }),
        )
        .unwrap();
        // Delivered a byte at a time, the frames still come out whole and in order.
        struct Trickle(Vec<u8>, usize);
        impl Read for Trickle {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                let Some(&x) = self.0.get(self.1) else {
                    return Ok(0);
                };
                self.1 += 1;
                b[0] = x;
                Ok(1)
            }
        }
        let mut r = Trickle(wire, 0);
        let mut f = FrameReader::default();
        assert_eq!(f.next(&mut r).unwrap(), Some((OUT, b"hello".to_vec())));
        assert_eq!(f.next(&mut r).unwrap(), Some((INPUT, Vec::new())));
        let (k, p) = f.next(&mut r).unwrap().unwrap();
        assert_eq!(k, RESIZE);
        let ws = parse_resize(&p).unwrap();
        assert_eq!((ws.num_cols, ws.num_lines, ws.cell_height), (120, 40, 17));
        assert!(f.next(&mut r).is_err(), "end of stream");
    }

    #[test]
    fn ending_an_agent_stops_what_outlives_its_hangup() {
        use std::os::unix::process::CommandExt;
        // A shell leading its own process group (as a PTY's does), running a child that
        // shrugs off SIGHUP and SIGTERM — like a dev server the hang-up never reaches.
        let mut shell = std::process::Command::new("sh")
            .args(["-c", "(trap '' HUP TERM; exec sleep 300) & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let root = shell.id();
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // Once it's `sleep`, its traps are set (the signals stay ignored across the exec).
        let is_sleep = |pid: u32| {
            let out = std::process::Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .ends_with("sleep")
        };
        wait_for("the child never started", &|| {
            proc_tree(root)
                .iter()
                .any(|p| p.pid != root && is_sleep(p.pid))
        });
        let child = proc_tree(root).into_iter().find(|p| p.pid != root).unwrap();

        end_tree(root, || {}, Duration::from_millis(300));
        shell.wait().unwrap();
        wait_for("the child outlived its agent", &|| {
            still_running(std::slice::from_ref(&child)).is_empty()
        });
    }
}
