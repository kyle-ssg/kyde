//! Single-instance guard (issue #72). Kyde already shows every open project as a tab, so a
//! second `kyde <path>` from the terminal shouldn't start a second app — it should hand the
//! path to the running one, which opens it as another project tab and comes to the front.
//!
//! Mechanism: a unix domain socket in the temp dir. First launch binds it and accepts
//! connections on a background thread, forwarding each payload over a channel to the UI
//! thread. A later launch connects, writes the resolved project path (or nothing, meaning
//! "just activate"), and exits before opening a window. A socket left behind by a crash is
//! detected (connect fails) and replaced.
//!
//! Opt-out: `KYDE_SINGLE_INSTANCE=0` (or a `KYDE_SHOT` screenshot run) keeps the old
//! one-process-per-launch behaviour. The socket name also carries the config directory, so
//! two instances pointed at different `XDG_CONFIG_HOME`s (the screenshot suite, dev
//! sandboxes) are separate "profiles" and never steal each other's launches.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// What a second launch asked the running instance to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    /// Open (or switch to) this project.
    Open(PathBuf),
    /// No path given (bare `kyde`) — just bring the running window forward.
    Activate,
}

/// Whether the single-instance guard is active for this launch. Off when explicitly disabled
/// or when driving a screenshot (the suite launches instances back to back).
pub(crate) fn enabled() -> bool {
    !matches!(
        std::env::var("KYDE_SINGLE_INSTANCE").as_deref(),
        Ok("0" | "false" | "off")
    ) && std::env::var_os("KYDE_SHOT").is_none()
}

/// Encode a request as the one-line wire payload.
pub(crate) fn encode(req: &Request) -> String {
    match req {
        Request::Open(p) => format!("open\t{}\n", p.display()),
        Request::Activate => "activate\n".to_string(),
    }
}

/// Decode a wire payload. Unknown verbs and empty paths degrade to `Activate` — the worst a
/// malformed message can do is raise the window.
pub(crate) fn decode(payload: &str) -> Request {
    let line = payload.lines().next().unwrap_or("").trim_end();
    match line.split_once('\t') {
        Some(("open", p)) if !p.is_empty() => Request::Open(PathBuf::from(p)),
        _ => Request::Activate,
    }
}

/// FNV-1a — a stable, dependency-free hash for the profile part of the socket name.
fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// A unix socket path can't exceed `sun_path` (104 bytes on macOS) — bind fails outright
/// with "path must be shorter than `SUN_LEN`". Leave margin below it.
const SUN_MAX: usize = 100;

/// The socket this launch talks on: one per user + config dir ("profile"), so separate users
/// on a shared `/tmp` and separate `XDG_CONFIG_HOME`s never collide.
pub(crate) fn socket_path() -> PathBuf {
    let dir = std::env::var_os("TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let profile = format!(
        "{}|{}",
        std::env::var("USER").unwrap_or_default(),
        std::env::var("XDG_CONFIG_HOME").unwrap_or_default()
    );
    socket_path_in(&dir, &profile)
}

/// Pure socket-path rule (the seam the tests drive): `<dir>/kyde-<profile hash>.sock`, or —
/// when that would blow `sun_path`, as a deep sandbox/CI `TMPDIR` does — `/tmp` with the
/// directory folded into the hash, so distinct temp dirs still get distinct sockets.
fn socket_path_in(dir: &Path, profile: &str) -> PathBuf {
    let p = dir.join(format!("kyde-{:x}.sock", fnv64(profile)));
    if p.as_os_str().len() <= SUN_MAX {
        return p;
    }
    let scoped = format!("{}|{profile}", dir.display());
    PathBuf::from("/tmp").join(format!("kyde-{:x}.sock", fnv64(&scoped)))
}

/// Try to hand this launch to an already-running instance. `true` = delivered (the caller
/// should exit without opening a window); `false` = nothing listening, so we're the instance.
/// A stale socket (owner died) is removed here so the [`listen`] that follows can bind.
pub(crate) fn try_send(req: &Request) -> bool {
    send_to(&socket_path(), req)
}

/// [`try_send`] against an explicit socket path (the seam the tests drive).
fn send_to(sock: &Path, req: &Request) -> bool {
    if let Ok(mut stream) = UnixStream::connect(sock) {
        // A running instance answered; if the write fails it's mid-shutdown — report
        // undelivered so we take over rather than exiting silently.
        return stream.write_all(encode(req).as_bytes()).is_ok() && stream.flush().is_ok();
    }
    // Nothing accepting: either no instance, or a socket file a crash left behind.
    if sock.exists() {
        let _ = std::fs::remove_file(sock);
    }
    false
}

/// Owns the listening socket; unlinks it on drop so a clean exit leaves nothing behind.
pub(crate) struct Guard {
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Become the single instance: bind the socket and accept launches on a background thread,
/// invoking `on_request` for each. The callback runs on that thread — it must only forward
/// (the gpui entities aren't `Send`). `None` = we couldn't bind (permissions, sandbox), in
/// which case the caller just runs as a normal, unguarded instance.
pub(crate) fn listen(
    on_request: impl Fn(Request) + Send + 'static,
) -> Option<(Guard, std::thread::JoinHandle<()>)> {
    let path = socket_path();
    let listener = UnixListener::bind(&path).ok()?;
    let handle = std::thread::Builder::new()
        .name("kyde-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = String::new();
                // Bound the read: a peer that never closes must not wedge the thread, and a
                // launch payload is one short line.
                if Read::by_ref(&mut stream)
                    .take(4096)
                    .read_to_string(&mut buf)
                    .is_ok()
                {
                    on_request(decode(&buf));
                }
            }
        })
        .ok()?;
    Some((Guard { path }, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip_and_degrade_safely() {
        let open = Request::Open(PathBuf::from("/Users/x/my repo"));
        assert_eq!(decode(&encode(&open)), open, "spaces survive the wire");
        assert_eq!(decode(&encode(&Request::Activate)), Request::Activate);
        // Anything unexpected raises the window rather than opening a bogus project.
        assert_eq!(decode(""), Request::Activate);
        assert_eq!(decode("open\t"), Request::Activate);
        assert_eq!(decode("nonsense\t/tmp"), Request::Activate);
        assert_eq!(
            decode("open\t/tmp/p\nextra junk\n"),
            Request::Open(PathBuf::from("/tmp/p")),
            "only the first line is the request"
        );
    }

    /// Different config homes = different profiles = different sockets (so the screenshot
    /// suite and a dev instance never hijack each other), and the path always fits
    /// `sun_path` — a deep `TMPDIR` (sandbox, CI) falls back to `/tmp` instead of failing
    /// to bind with "path must be shorter than `SUN_LEN`".
    #[test]
    fn socket_path_is_per_profile_and_always_bindable() {
        let short = socket_path_in(Path::new("/var/folders/ab/T"), "kyle|");
        assert!(
            short.starts_with("/var/folders/ab/T"),
            "normal TMPDIR is used"
        );
        assert_ne!(
            short,
            socket_path_in(Path::new("/var/folders/ab/T"), "kyle|/tmp/other-cfg"),
            "distinct config homes get distinct sockets"
        );

        let deep = Path::new(
            "/private/tmp/claude-501/-Users-someone-project/0123456789abcdef-0123-4567/scratchpad",
        );
        let long = socket_path_in(deep, "kyle|");
        assert!(long.starts_with("/tmp"), "deep TMPDIR falls back to /tmp");
        assert!(long.as_os_str().len() <= SUN_MAX, "must fit sun_path");
        assert_ne!(
            long,
            socket_path_in(
                Path::new("/some/other/very/very/very/very/very/very/very/very/very/long/tmp"),
                "kyle|"
            ),
            "the fallback still separates temp dirs"
        );
    }

    /// The real handshake: a listener takes a launch, and a send with nothing listening
    /// reports false (and clears a stale socket file).
    #[test]
    fn a_second_launch_is_delivered_to_the_listener() {
        let dir = std::env::temp_dir().join(format!("kyde-inst-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let sock = dir.join("k.sock");

        // Nothing listening yet: a stale file is cleaned up, delivery fails.
        std::fs::write(&sock, b"stale").expect("write stale socket");
        assert!(!send_to(&sock, &Request::Activate));
        assert!(!sock.exists(), "stale socket removed so we can bind");

        let listener = UnixListener::bind(&sock).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut buf = String::new();
                let mut stream = stream;
                if stream.read_to_string(&mut buf).is_ok() {
                    let _ = tx.send(decode(&buf));
                }
            }
        });

        let want = Request::Open(PathBuf::from("/Users/x/proj"));
        assert!(send_to(&sock, &want), "listener accepts the launch");
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request forwarded");
        assert_eq!(got, want);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
