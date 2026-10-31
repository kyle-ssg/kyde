//! Getting your attention when an agent finishes or needs input: a soft chime, and while Kyde
//! is in the background a Dock icon bounce (macOS) or a desktop notification (Linux). No gpui;
//! the Dock calls must run on the main thread (they're no-ops elsewhere). Neither does
//! anything on other platforms.
//!
//! The chimes are Kyde's own, made here rather than borrowed from the system's sharp alert
//! sounds: quiet sine notes with a hint of overtone, a soft attack and a long fade — a
//! marimba-like "ding", meant to be noticed without startling.

/// Which alert: an agent finished its turn, or it's waiting on you.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sound {
    Done,
    NeedsInput,
}

/// Bumped when the chimes change, so a new file is written (older ones are left unused).
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
const CHIME_VERSION: u32 = 1;
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
const RATE: u32 = 44_100;

/// The players `play` tries, in order, as (program, flags before the file): macOS's `afplay`;
/// on Linux `PipeWire`'s, `PulseAudio`'s, plain ALSA's, then libcanberra's — the first that plays
/// it wins.
#[cfg(target_os = "macos")]
const PLAYERS: &[(&str, &[&str])] = &[("afplay", &[])];
#[cfg(target_os = "linux")]
const PLAYERS: &[(&str, &[&str])] = &[
    ("pw-play", &[]),
    ("paplay", &[]),
    ("aplay", &["-q"]),
    ("canberra-gtk-play", &["-f"]),
];

// Played on macOS and Linux; elsewhere the chimes are made but never played.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
impl Sound {
    /// Its notes: (frequency Hz, start seconds). Done rises a fourth (E5 → A5); needs-input
    /// is two gentle taps on A5 — "knock knock".
    fn notes(self) -> &'static [(f32, f32)] {
        match self {
            Sound::Done => &[(659.25, 0.0), (880.0, 0.13)],
            Sound::NeedsInput => &[(880.0, 0.0), (880.0, 0.17)],
        }
    }

    fn file_name(self) -> String {
        let name = match self {
            Sound::Done => "done",
            Sound::NeedsInput => "needs-input",
        };
        format!("kyde-{name}-v{CHIME_VERSION}.wav")
    }
}

/// `notes` as a soft chime: a mono 16-bit WAV file's bytes.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn chime_wav(notes: &[(f32, f32)]) -> Vec<u8> {
    const LEN: f32 = 0.9;
    // Quiet on purpose: a peak well under full scale.
    const PEAK: f32 = 0.22;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let samples: Vec<i16> = {
        let n = (LEN * RATE as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let v: f32 = notes
                    .iter()
                    .filter(|(_, start)| t >= *start)
                    .map(|(f, start)| {
                        let dt = t - start;
                        // 6 ms attack (no click), then a slow exponential fade.
                        let env = (dt / 0.006).min(1.0) * (-dt / 0.22).exp();
                        let w = std::f32::consts::TAU * f * dt;
                        env * (w.sin() + 0.12 * (2.0 * w).sin() + 0.04 * (3.0 * w).sin())
                    })
                    .sum();
                // The last few ms fade to silence so the file never ends on a click.
                let tail = ((LEN - t) / 0.02).clamp(0.0, 1.0);
                ((v * PEAK * tail).clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
            })
            .collect()
    };
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(0);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Play `sound` without blocking: the chime is written once (to `~/.kyde/sounds`), and the
/// player (see `PLAYERS`) runs on its own thread, which reaps it.
pub(crate) fn play(sound: Sound) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    std::thread::spawn(move || {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let dir = std::path::PathBuf::from(home).join(".kyde").join("sounds");
        if let Some(path) = chime_file(&dir, sound) {
            play_first(PLAYERS, &path);
        }
    });
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = sound;
}

/// `sound`'s chime file in `dir`, written the first time it's needed; `None` if it can't be.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn chime_file(dir: &std::path::Path, sound: Sound) -> Option<std::path::PathBuf> {
    let path = dir.join(sound.file_name());
    if !path.exists() {
        let _ = std::fs::create_dir_all(dir);
        std::fs::write(&path, chime_wav(sound.notes())).ok()?;
    }
    Some(path)
}

/// Play `path` with the first of `players` that works: a missing player, or one that fails
/// (no sound server of its kind running), hands on to the next. Blocks until it's done;
/// `true` if one played it.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn play_first(players: &[(&str, &[&str])], path: &std::path::Path) -> bool {
    players.iter().any(|(program, flags)| {
        std::process::Command::new(program)
            .args(*flags)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Ask for attention while Kyde is in the background. macOS: bounce the Dock icon once (an
/// informational request — it doesn't keep bouncing). Linux, which has no Dock (and gpui no
/// window-urgency call): a desktop notification saying what happened, at most one per burst.
pub(crate) fn request_attention(sound: Sound) {
    #[cfg(not(target_os = "linux"))]
    let _ = sound;
    #[cfg(target_os = "linux")]
    if notify_now(std::time::Instant::now()) {
        std::thread::spawn(move || {
            let _ = std::process::Command::new("notify-send")
                .args(notification_args(sound))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        NSApplication::sharedApplication(mtm)
            .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
    }
}

/// Whether a notification may go out at `now`: not within 1.5 s of the last one, so several
/// agents finishing together make one notification, not a stack (a Dock bounce can't stack).
#[cfg(target_os = "linux")]
fn notify_now(now: std::time::Instant) -> bool {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let quiet =
        last.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(1500));
    if !quiet {
        *last = Some(now);
    }
    !quiet
}

/// `notify-send`'s arguments for `sound`: Kyde's name and icon (the `.desktop` entry's), a
/// title and a line saying what happened.
#[cfg(target_os = "linux")]
fn notification_args(sound: Sound) -> [&'static str; 4] {
    let body = match sound {
        Sound::Done => "An agent finished",
        Sound::NeedsInput => "An agent needs your input",
    };
    ["--app-name=Kyde", "--icon=kyde", "Kyde", body]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chimes_are_valid_quiet_wavs() {
        for sound in [Sound::Done, Sound::NeedsInput] {
            let wav = chime_wav(sound.notes());
            assert_eq!(&wav[..4], b"RIFF");
            let data = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
            assert_eq!(wav.len(), 44 + data, "header length matches the samples");
            // Quiet: no sample anywhere near full scale.
            let loudest = wav[44..]
                .chunks(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
                .max()
                .unwrap();
            assert!(
                loudest < i16::MAX as u16 / 3,
                "{sound:?} peaks at {loudest}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn chime_is_written_once_and_played_by_the_first_player_that_works() {
        let dir = std::env::temp_dir().join(format!("kyde-chimes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = chime_file(&dir, Sound::NeedsInput).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            chime_wav(Sound::NeedsInput.notes())
        );
        // Already there → left as it is.
        std::fs::write(&path, b"kept").unwrap();
        assert_eq!(chime_file(&dir, Sound::NeedsInput), Some(path.clone()));
        assert_eq!(std::fs::read(&path).unwrap(), b"kept");
        // A missing player and a failing one hand on; `test -f <path>` "plays" it — so the
        // file goes last, after the flags.
        assert!(play_first(
            &[
                ("kyde-no-such-player", &[]),
                ("false", &[]),
                ("test", &["-f"])
            ],
            &path
        ));
        assert!(!play_first(
            &[("kyde-no-such-player", &[]), ("test", &["-d"])],
            &path
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_notification_says_what_happened_once_per_burst() {
        assert_eq!(
            notification_args(Sound::NeedsInput),
            [
                "--app-name=Kyde",
                "--icon=kyde",
                "Kyde",
                "An agent needs your input"
            ]
        );
        assert_eq!(notification_args(Sound::Done)[3], "An agent finished");
        let t = std::time::Instant::now();
        assert!(notify_now(t));
        assert!(!notify_now(t + std::time::Duration::from_millis(500)));
        assert!(notify_now(t + std::time::Duration::from_millis(1600)));
    }
}
