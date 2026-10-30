//! Getting your attention when an agent finishes or needs input: a soft chime, and a Dock
//! icon bounce. No gpui; the Dock calls must run on the main thread (they're no-ops
//! elsewhere, and off macOS).
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const CHIME_VERSION: u32 = 1;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const RATE: u32 = 44_100;

// Played with macOS's `afplay`; elsewhere the chimes are made but never played.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

/// Play `sound` without blocking: the chime is written once (to `~/.kyde/sounds`), and
/// `afplay` runs on its own thread, which reaps it.
pub(crate) fn play(sound: Sound) {
    #[cfg(target_os = "macos")]
    std::thread::spawn(move || {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let dir = std::path::PathBuf::from(home).join(".kyde").join("sounds");
        let path = dir.join(sound.file_name());
        if !path.exists() {
            let _ = std::fs::create_dir_all(&dir);
            if std::fs::write(&path, chime_wav(sound.notes())).is_err() {
                return;
            }
        }
        let _ = std::process::Command::new("afplay")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
    #[cfg(not(target_os = "macos"))]
    let _ = sound;
}

/// Bounce the Dock icon once (an informational request — it doesn't keep bouncing).
pub(crate) fn dock_bounce() {
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
}
