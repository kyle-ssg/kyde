//! Agent hosts: what keeps agents running when Kyde quits.
//!
//! Every agent tab runs inside a small host process — this same binary, started as
//! `kyde --agent-host <spec> <fd>` — that owns the tab's PTY and shell, keeps the last few MB
//! of output, and serves them on a Unix socket in `~/.kyde/agents/<id>.sock`. Kyde's terminal
//! view is a client: it attaches, gets the buffered output replayed, then the live stream, and
//! sends keystrokes / resizes back. Quitting Kyde only disconnects; the next launch reattaches
//! (`live_hosts`). Closing the tab sends `KILL`. The `ky agent …` CLI talks to the same
//! sockets.
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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Bumped when the wire format changes (a host from an older Kyde says so in `HELLO`).
pub(crate) const VERSION: u32 = 1;
/// Output a host keeps for a reattaching viewer.
const RING_CAP: usize = 4 * 1024 * 1024;
/// Largest frame accepted — anything bigger is a broken stream.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Client → host: start streaming (answered with `HELLO`, the buffered `OUT`, then live).
pub(crate) const ATTACH: u8 = 1;
/// Client → host: bytes for the PTY, as if typed.
pub(crate) const INPUT: u8 = 2;
/// Client → host: cols, rows, cell width, cell height — four `u16` LE.
pub(crate) const RESIZE: u8 = 3;
/// Client → host: send the buffered output once, then hang up.
pub(crate) const SNAPSHOT: u8 = 4;
/// Client → host: end the agent (SIGHUP to its process group).
pub(crate) const KILL: u8 = 5;
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
    let mut child = std::process::Command::new(std::env::current_exe()?)
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
    /// Recent output. Its lock also orders a new viewer's replay before any live output.
    ring: Mutex<Ring>,
    viewers: Mutex<Vec<UnixStream>>,
    writer: Mutex<std::fs::File>,
    pty: Mutex<tty::Pty>,
    size: Mutex<(u16, u16)>,
    child: u32,
}

impl Shared {
    /// Signal the agent's whole process group (the shell is its session leader).
    fn signal(&self, sig: i32) {
        if let Ok(pgid) = i32::try_from(self.child) {
            if pgid > 0 {
                // SAFETY: plain kill(2) on the child's own process group.
                unsafe {
                    libc::kill(-pgid, sig);
                }
            }
        }
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
    let options = PtyOptions {
        shell: Some(Shell::new(spec.shell.clone(), Vec::new())),
        working_directory: Some(spec.cwd.clone()),
        drain_on_exit: false,
        env: spec.env.iter().cloned().collect(),
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
    let mut writer = pty.file().try_clone()?;
    if let Some(line) = &spec.line {
        writer.write_all(format!("{line}\n").as_bytes())?;
    }
    let shared = Arc::new(Shared {
        ring: Mutex::new(Ring::default()),
        viewers: Mutex::new(Vec::new()),
        writer: Mutex::new(writer),
        pty: Mutex::new(pty),
        size: Mutex::new((spec.cols, spec.rows)),
        child,
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
    pump(master, &shared);
    // The agent's terminal closed: tell viewers, unregister, and go (dropping the PTY hangs
    // up anything left in it).
    for v in lock(&shared.viewers).iter_mut() {
        let _ = write_frame(v, EXIT, &[]);
    }
    let _ = std::fs::remove_file(sock_path(&spec.id));
    let _ = std::fs::remove_file(info_path(&spec.id));
    // Its log (our stderr) only stays if something went wrong.
    let log = kyde_config::agents::agents_dir().join(format!("{}.log", spec.id));
    if std::fs::metadata(&log).is_ok_and(|m| m.len() == 0) {
        let _ = std::fs::remove_file(log);
    }
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
        lock(&shared.viewers).retain_mut(|v| write_frame(v, OUT, bytes).is_ok());
    }
}

/// One connection: an attaching viewer, or a one-shot request (snapshot, input, kill).
fn serve(mut stream: UnixStream, shared: &Shared) {
    // A viewer that stops reading is dropped rather than stalling the agent.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut frames = FrameReader::default();
    while let Ok(Some((kind, payload))) = frames.next(&mut stream) {
        match kind {
            ATTACH => {
                let Ok(mut out) = stream.try_clone() else {
                    return;
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
                return;
            }
            KILL => {
                shared.signal(libc::SIGHUP);
                return;
            }
            _ => {}
        }
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
}
