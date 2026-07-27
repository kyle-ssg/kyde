#![deny(missing_docs)]
//! Self-update. Checks GitHub for a newer release, downloads the notarized `Kyde.app` zip,
//! swaps it over the running bundle, and the caller relaunches. Network + unzip are shelled
//! to `curl` / `ditto` (same shell-out philosophy as the git layer — no HTTP crate in the
//! default build).
//!
//! Test seams (so the whole flow is provable in dev, offline — see `scripts/test-update.sh`):
//! - `KYDE_VERSION_OVERRIDE` — pretend the running app is this version (force the banner).
//! - `KYDE_UPDATE_FEED_URL` — fetch the "latest release" JSON from here instead of GitHub. A
//!   `file://` fixture works, and its asset URL may be `file://` too.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Everything self-update can fail with. Each variant carries context — which external command
/// failed and its stderr, or the I/O operation + underlying error. (Library crate → typed
/// errors via `thiserror`, never `anyhow`.)
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The release has no downloadable `.zip` asset (nothing to install).
    #[error("release has no downloadable zip")]
    NoZip,
    /// The downloaded archive contained no `.app` bundle.
    #[error("no .app found in the download")]
    NoAppInDownload,
    /// A remote (https) release has no `.sha256` checksum asset — refuse to install an
    /// unverifiable binary. (Releases publish one alongside each zip; see release.yml.)
    #[error("release has no checksum asset — refusing unverified install")]
    MissingChecksum,
    /// The downloaded zip's SHA-256 doesn't match the published checksum.
    #[error("checksum mismatch — expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// The digest published next to the release asset.
        expected: String,
        /// The digest of what was actually downloaded.
        actual: String,
    },
    /// An external command (`curl` / `ditto`) ran but failed. `action` names the step.
    #[error("{action} failed: {stderr}")]
    Command {
        /// Which step failed — `"download"`, `"unzip"`, or `"install"`.
        action: String,
        /// The external command's captured stderr.
        stderr: String,
    },
    /// An I/O or process-spawn error, tagged with what we were doing.
    #[error("{operation}: {source}")]
    Io {
        /// What we were attempting when the error occurred (e.g. `"creating temp dir"`).
        operation: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Result alias for every fallible self-update operation.
pub type Result<T> = std::result::Result<T, UpdateError>;

/// Build an [`UpdateError::Io`] from an operation description + the underlying I/O error.
fn io_err(operation: impl Into<String>) -> impl FnOnce(std::io::Error) -> UpdateError {
    move |source| UpdateError::Io {
        operation: operation.into(),
        source,
    }
}

/// GitHub "latest release" API for this repo.
const DEFAULT_FEED: &str = "https://api.github.com/repos/kyle-ssg/kyde/releases/latest";

/// GitHub "all releases" API for this repo — the changelog window's feed (issue #71).
const DEFAULT_RELEASES_FEED: &str =
    "https://api.github.com/repos/kyle-ssg/kyde/releases?per_page=50";

/// A release newer than what's running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Normalised numeric version, e.g. "1.0.1".
    pub version: String,
    /// Raw tag, e.g. "v1.0.1".
    pub tag: String,
    /// `browser_download_url` of the macOS `.app` zip (empty if the release has no zip asset).
    /// May be a `file://` URL in tests.
    pub zip_url: String,
    /// URL of the zip's `.sha256` checksum asset (empty when the release has none — only
    /// pre-checksum releases and bare dev fixtures; https installs refuse to proceed then).
    pub sha256_url: String,
    /// Release page URL — the fallback when we can't swap in place (dev binary / no asset).
    pub page_url: String,
}

/// One published release, as shown in the changelog window (issue #71) — the GitHub
/// Releases page mirrored in-app. Unlike [`Release`] (the self-update target) this carries
/// the human-facing notes and is kept for *every* version, not just newer ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNote {
    /// Normalised numeric version, e.g. "2.5.0".
    pub version: String,
    /// Raw tag, e.g. "kyde-v2.5.0".
    pub tag: String,
    /// Release title (GitHub's `name`); falls back to the tag when unset.
    pub title: String,
    /// Release notes, markdown as published.
    pub body: String,
    /// Publication date as `YYYY-MM-DD` (empty when GitHub reports none).
    pub date: String,
    /// Release page URL — the "Open on GitHub" link.
    pub page_url: String,
    /// Whether GitHub flagged this as a pre-release.
    pub prerelease: bool,
}

/// Version the running app reports — env override (dev/testing) else the compiled crate version.
pub fn current_version() -> String {
    std::env::var("KYDE_VERSION_OVERRIDE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

fn feed_url() -> String {
    std::env::var("KYDE_UPDATE_FEED_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FEED.to_string())
}

/// Parse a release tag → `(major, minor, patch)`. Handles `1.2.3`, `v1.2.3`, the
/// release-please component form `kyde-v1.2.3`, and any `-pre`/`+build` suffix, by reading
/// from the first digit. Missing minor/patch default to 0. `None` if no numeric version.
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    // Start at the first digit so any leading prefix ("kyde-v", "v", …) is skipped.
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let core = s[start..].split(['-', '+', ' ']).next().unwrap_or("");
    let mut it = core.split('.');
    let major = it.next()?.trim().parse().ok()?;
    let minor = it.next().unwrap_or("0").trim().parse().ok()?;
    let patch = it.next().unwrap_or("0").trim().parse().ok()?;
    Some((major, minor, patch))
}

/// True when `latest` is a strictly higher semantic version than `current`.
///
/// ```
/// assert!(kyde_update::is_newer("v1.0.10", "v1.0.9"));
/// assert!(!kyde_update::is_newer("v1.0.0", "v1.0.0"));
/// ```
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn norm(tag: &str) -> String {
    parse_semver(tag).map_or_else(
        || tag.trim().to_string(),
        |(a, b, c)| format!("{a}.{b}.{c}"),
    )
}

/// The running `.app` bundle, if we were launched from one
/// (`…/Kyde.app/Contents/MacOS/kyde`). `None` for the bare dev binary, in which case the
/// caller opens the release page instead of swapping in place.
pub fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|a| a.extension().and_then(|e| e.to_str()) == Some("app"))
        .map(std::path::Path::to_path_buf)
}

/// Pull the latest-release metadata and return it only when it's newer than what's running.
/// `Ok(None)` = already up to date; `Err` = network/parse failure (callers stay quiet).
pub fn check() -> Result<Option<Release>> {
    let body = curl_text(&feed_url())?;
    Ok(parse_feed(&body, &current_version()))
}

/// Pure feed → `Option<Release>` so it's unit-testable without the network.
pub fn parse_feed(body: &str, current: &str) -> Option<Release> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = json["tag_name"].as_str().unwrap_or("").trim().to_string();
    if tag.is_empty() || !is_newer(&tag, current) {
        return None;
    }
    let page_url = json["html_url"].as_str().unwrap_or("").to_string();
    // Pick the asset matching the running CPU (e.g. `kyde-macos-arm64.zip` on Apple Silicon,
    // `kyde-macos-x86_64.zip` on Intel), falling back to a generic/legacy zip. Empty if none.
    let urls: Vec<&str> = json["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter_map(|a| a["browser_download_url"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let zip_url = pick_asset_url(&urls, std::env::consts::ARCH).unwrap_or_default();
    // The checksum asset sits next to the zip as `<asset>.sha256`, so its download URL is
    // exactly the zip's with that suffix. Empty when the release didn't publish one.
    let want_sha = format!("{zip_url}.sha256");
    let sha256_url = urls
        .iter()
        .find(|u| **u == want_sha)
        .map(|u| (*u).to_string())
        .unwrap_or_default();
    Some(Release {
        version: norm(&tag),
        tag,
        zip_url,
        sha256_url,
        page_url,
    })
}

/// Where the changelog window reads its release list from (`KYDE_RELEASES_FEED_URL`
/// overrides it — a `file://` fixture works, same dev seam as [`feed_url`]).
fn releases_feed_url() -> String {
    std::env::var("KYDE_RELEASES_FEED_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASES_FEED.to_string())
}

/// Fetch every published release, newest first — the changelog window's data (issue #71).
/// Network I/O: run it off the UI thread. `Err` = network/HTTP failure (the caller shows a
/// retry + an "Open on GitHub" link).
pub fn release_notes() -> Result<Vec<ReleaseNote>> {
    let body = curl_text(&releases_feed_url())?;
    Ok(parse_releases(&body))
}

/// Pure feed → release notes, so the changelog is unit-testable without the network.
/// Drafts are skipped (they aren't public), entries are sorted newest-version first, and a
/// tag with no numeric version sorts last rather than being dropped.
pub fn parse_releases(body: &str) -> Vec<ReleaseNote> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    // A single-release feed (the `latest` endpoint) parses too — treat it as a one-element list.
    let items: Vec<&serde_json::Value> = match json.as_array() {
        Some(a) => a.iter().collect(),
        None if json.is_object() => vec![&json],
        None => return Vec::new(),
    };
    let mut out: Vec<ReleaseNote> = items
        .into_iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str().unwrap_or("").trim().to_string();
            if tag.is_empty() {
                return None;
            }
            let title = r["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&tag)
                .to_string();
            Some(ReleaseNote {
                version: norm(&tag),
                title,
                // GitHub sends CRLF in release bodies; the markdown parser is happier without.
                body: r["body"].as_str().unwrap_or("").replace("\r\n", "\n"),
                date: r["published_at"]
                    .as_str()
                    .or_else(|| r["created_at"].as_str())
                    .unwrap_or("")
                    .chars()
                    .take(10)
                    .collect(),
                page_url: r["html_url"].as_str().unwrap_or("").to_string(),
                prerelease: r["prerelease"].as_bool().unwrap_or(false),
                tag,
            })
        })
        .collect();
    // Newest first; unparseable tags sink to the bottom (keeps real versions ordered).
    out.sort_by(|a, b| {
        parse_semver(&b.tag)
            .unwrap_or((0, 0, 0))
            .cmp(&parse_semver(&a.tag).unwrap_or((0, 0, 0)))
    });
    out
}

/// Filename tokens that identify a build for `arch` (Rust's `std::env::consts::ARCH`).
fn arch_tokens(arch: &str) -> &'static [&'static str] {
    match arch {
        "aarch64" => &["arm64", "aarch64"],
        "x86_64" => &["x86_64", "x86-64", "x64", "intel"],
        _ => &[],
    }
}

/// Every arch token we recognise — used to tell an arch-specific asset from a generic one.
const ALL_ARCH_TOKENS: &[&str] = &["arm64", "aarch64", "x86_64", "x86-64", "x64", "intel"];

/// Choose the `.zip` asset for `arch` from a release's asset URLs:
///   1. an asset whose name carries this arch's token (`…-arm64.zip` / `…-x86_64.zip`);
///   2. else a *generic* zip with no arch token (covers single-asset / legacy releases like
///      `kyde-macos.zip`);
///   3. else `None` — never install an explicitly wrong-arch build.
pub fn pick_asset_url(urls: &[&str], arch: &str) -> Option<String> {
    let mine = arch_tokens(arch);
    let zips = || {
        urls.iter()
            .copied()
            .filter(|u| u.to_lowercase().ends_with(".zip"))
    };
    if let Some(u) = zips().find(|u| {
        let l = u.to_lowercase();
        mine.iter().any(|t| l.contains(t))
    }) {
        return Some(u.to_string());
    }
    if let Some(u) = zips().find(|u| {
        let l = u.to_lowercase();
        !ALL_ARCH_TOKENS.iter().any(|t| l.contains(t))
    }) {
        return Some(u.to_string());
    }
    None
}

/// Extra curl flags for a URL. Remote fetches pin HTTPS + TLS 1.2 so neither the request
/// nor any `-L` redirect can ever downgrade to plain http; `file://` (the dev fixture, see
/// `scripts/test-update.sh`) is exempt.
fn curl_security_args(url: &str) -> &'static [&'static str] {
    if url.starts_with("file://") {
        &[]
    } else {
        &["--proto", "=https", "--tlsv1.2"]
    }
}

/// Verify `file`'s SHA-256 against the checksum published at `sha256_url` (a text file whose
/// first whitespace-separated token is the hex digest — `shasum -a 256` output). The local
/// digest is computed by shelling to `shasum` (ships with macOS), matching this crate's
/// no-extra-deps philosophy.
fn verify_checksum(file: &Path, sha256_url: &str) -> Result<()> {
    let body = curl_text(sha256_url)?;
    let expected = body
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpdateError::ChecksumMismatch {
            expected: format!("<malformed checksum asset: {expected:.16}…>"),
            actual: String::new(),
        });
    }
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(file)
        .output()
        .map_err(io_err("spawning shasum"))?;
    if !out.status.success() {
        return Err(UpdateError::Command {
            action: "checksum".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let actual = stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }
    Ok(())
}

/// Download `zip_url`, verify it against `sha256_url`, unzip, and swap the new `Kyde.app`
/// over `bundle` in place. On success the caller relaunches (`open <bundle>`) and quits.
/// Runs off the UI thread (blocking I/O).
///
/// Integrity policy: an https download REQUIRES a checksum asset (every release publishes
/// `<zip>.sha256` — see release.yml — so a missing one means an incomplete or tampered
/// release; refuse rather than install unverified). A `file://` fixture may omit it (dev
/// seam), but is verified when present.
pub fn download_and_swap(zip_url: &str, sha256_url: &str, bundle: &Path) -> Result<()> {
    if zip_url.is_empty() {
        return Err(UpdateError::NoZip);
    }
    if sha256_url.is_empty() && !zip_url.starts_with("file://") {
        return Err(UpdateError::MissingChecksum);
    }
    let tmp = std::env::temp_dir().join(format!("kyde-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(io_err("creating temp dir"))?;

    // Download (curl handles `file://` for the dev fixture too).
    let zip = tmp.join("kyde.zip");
    let out = Command::new("curl")
        .args(curl_security_args(zip_url))
        .args(["-fsSL", "-o"])
        .arg(&zip)
        .arg(zip_url)
        .output()
        .map_err(io_err("spawning curl"))?;
    if !out.status.success() {
        return Err(UpdateError::Command {
            action: "download".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    if !sha256_url.is_empty() {
        verify_checksum(&zip, sha256_url)?;
    }

    // Unzip with ditto (preserves bundle symlinks/metadata/signature, unlike `unzip`).
    let extract = tmp.join("extract");
    std::fs::create_dir_all(&extract).map_err(io_err("creating extract dir"))?;
    let out = Command::new("ditto")
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(&extract)
        .output()
        .map_err(io_err("spawning ditto"))?;
    if !out.status.success() {
        return Err(UpdateError::Command {
            action: "unzip".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let new_app = find_app(&extract).ok_or(UpdateError::NoAppInDownload)?;
    // Strip the download quarantine so the relaunched copy isn't Gatekeeper-gated (a notarized
    // + stapled app still picks up `com.apple.quarantine` from the zip).
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&new_app)
        .output();

    // Swap: move the old bundle aside, install the new one, then drop the backup. If install
    // fails, roll the old bundle back so the user is never left with no app.
    let backup = bundle.with_extension("app.bak");
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(bundle, &backup).map_err(io_err(format!("moving aside {bundle:?}")))?;
    if let Err(e) = install(&new_app, bundle) {
        let _ = std::fs::rename(&backup, bundle);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Download `zip_url` into `dir`, returning the saved file path. The non-bundle fallback
/// (dev binary): we can't swap in place, so just fetch the zip for the user to install.
pub fn download_zip(zip_url: &str, dir: &Path) -> Result<PathBuf> {
    if zip_url.is_empty() {
        return Err(UpdateError::NoZip);
    }
    std::fs::create_dir_all(dir).ok();
    let name = zip_url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("kyde-update.zip");
    let dest = dir.join(name);
    let out = Command::new("curl")
        .args(curl_security_args(zip_url))
        .args(["-fsSL", "-o"])
        .arg(&dest)
        .arg(zip_url)
        .output()
        .map_err(io_err("spawning curl"))?;
    if !out.status.success() {
        return Err(UpdateError::Command {
            action: "download".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(dest)
}

/// Move (same volume) or copy (cross-volume, e.g. temp → /Applications) the new bundle in.
/// The copy path stages into a sibling and renames into place, so a half-copied bundle can
/// never sit at the real path (a direct `ditto` to the destination could die mid-copy AND
/// block the caller's rollback rename — leaving the user with no launchable app).
fn install(new_app: &Path, bundle: &Path) -> Result<()> {
    if std::fs::rename(new_app, bundle).is_ok() {
        return Ok(());
    }
    // Stage inside the destination's directory (same volume) → the final rename is atomic.
    let staged = bundle.with_extension("app.staging");
    let _ = std::fs::remove_dir_all(&staged);
    let out = Command::new("ditto")
        .arg(new_app)
        .arg(&staged)
        .output()
        .map_err(io_err("spawning ditto copy"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(UpdateError::Command {
            action: "install".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    std::fs::rename(&staged, bundle).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staged);
        io_err(format!("installing {bundle:?}"))(e)
    })
}

fn find_app(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        (p.extension().and_then(|x| x.to_str()) == Some("app")).then_some(p)
    })
}

fn curl_text(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(curl_security_args(url))
        .args(["-fsSL", "-H", "User-Agent: kyde-updater", url])
        .output()
        .map_err(io_err("spawning curl"))?;
    if !out.status.success() {
        return Err(UpdateError::Command {
            action: format!("curl {url}"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parsing_tolerates_prefixes_and_suffixes() {
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("v2.0.0-rc1"), Some((2, 0, 0)));
        assert_eq!(parse_semver("1.4"), Some((1, 4, 0)));
        assert_eq!(parse_semver("3"), Some((3, 0, 0)));
        assert_eq!(parse_semver("v1.2.3+build5"), Some((1, 2, 3)));
        // release-please component-prefixed tag (what this repo actually publishes).
        assert_eq!(parse_semver("kyde-v1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_semver("kyde-v2.3.4-rc1"), Some((2, 3, 4)));
        assert_eq!(parse_semver("nightly"), None);
    }

    #[test]
    fn is_newer_compares_numerically_not_lexically() {
        assert!(is_newer("v1.0.10", "v1.0.9")); // 10 > 9 (lexical would say "10" < "9")
        assert!(is_newer("v1.1.0", "1.0.99"));
        assert!(is_newer("2.0.0", "v1.9.9"));
        assert!(!is_newer("v1.0.0", "v1.0.0")); // equal → not newer
        assert!(!is_newer("v1.0.0", "v1.0.1")); // older
        assert!(!is_newer("garbage", "v1.0.0")); // unparseable → not newer
    }

    #[test]
    fn parse_feed_surfaces_only_newer_releases_with_zip_asset() {
        let feed = r#"{
            "tag_name": "v1.2.0",
            "html_url": "https://github.com/kyle-ssg/kyde/releases/tag/v1.2.0",
            "assets": [
                { "browser_download_url": "https://example.com/notes.txt" },
                { "browser_download_url": "https://example.com/kyde-macos.zip" },
                { "browser_download_url": "https://example.com/kyde-macos.zip.sha256" }
            ]
        }"#;
        // Newer than 1.0.0 → surfaced, picks the .zip asset (NOT its .sha256 sibling) and
        // the matching checksum asset.
        let r = parse_feed(feed, "1.0.0").expect("should surface a newer release");
        assert_eq!(r.version, "1.2.0");
        assert_eq!(r.tag, "v1.2.0");
        assert_eq!(r.zip_url, "https://example.com/kyde-macos.zip");
        assert_eq!(r.sha256_url, "https://example.com/kyde-macos.zip.sha256");
        assert!(r.page_url.contains("v1.2.0"));
        // Same/newer current → nothing to offer.
        assert_eq!(parse_feed(feed, "1.2.0"), None);
        assert_eq!(parse_feed(feed, "1.3.0"), None);
    }

    /// A release without a checksum asset parses (old releases exist), but an https install
    /// from it must be refused — never swap in an unverifiable binary.
    #[test]
    fn https_install_requires_a_checksum_asset() {
        let feed = r#"{
            "tag_name": "v9.9.9",
            "html_url": "u",
            "assets": [ { "browser_download_url": "https://example.com/kyde-macos.zip" } ]
        }"#;
        let r = parse_feed(feed, "1.0.0").expect("release parses");
        assert!(r.sha256_url.is_empty());
        let tmp = std::env::temp_dir().join("kyde-nochecksum-test.app");
        let err = download_and_swap(&r.zip_url, &r.sha256_url, &tmp)
            .expect_err("https install without a checksum must be refused");
        assert!(matches!(err, UpdateError::MissingChecksum));
    }

    /// End-to-end checksum verification against a real file + a `file://` checksum asset:
    /// the matching digest passes, a corrupted one fails with `ChecksumMismatch`.
    #[test]
    fn verify_checksum_accepts_match_and_rejects_mismatch() {
        let dir = std::env::temp_dir().join(format!("kyde-sha-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let payload = dir.join("payload.bin");
        std::fs::write(&payload, b"hello update\n").unwrap();
        // Compute the real digest the same way the verifier does (shasum ships on macOS
        // and the Linux CI runners alike).
        let out = Command::new("shasum")
            .args(["-a", "256"])
            .arg(&payload)
            .output()
            .expect("shasum available");
        let digest = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
        let good = dir.join("payload.bin.sha256");
        std::fs::write(&good, format!("{digest}  payload.bin\n")).unwrap();
        let good_url = format!("file://{}", good.display());
        verify_checksum(&payload, &good_url).expect("matching digest verifies");

        // Flip a hex digit → mismatch.
        let bad_digest: String = digest
            .chars()
            .map(|c| if c == '0' { '1' } else { '0' })
            .collect();
        let bad = dir.join("bad.sha256");
        std::fs::write(&bad, format!("{bad_digest}  payload.bin\n")).unwrap();
        let bad_url = format!("file://{}", bad.display());
        let err = verify_checksum(&payload, &bad_url).expect_err("corrupt digest must fail");
        assert!(matches!(err, UpdateError::ChecksumMismatch { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pick_asset_matches_running_arch() {
        let arm = "https://x/kyde-macos-arm64.zip";
        let intel = "https://x/kyde-macos-x86_64.zip";
        let urls = [arm, intel, "https://x/notes.txt"];
        // Each arch picks its own slice.
        assert_eq!(pick_asset_url(&urls, "aarch64").as_deref(), Some(arm));
        assert_eq!(pick_asset_url(&urls, "x86_64").as_deref(), Some(intel));
        // A generic/legacy single asset is used by any arch.
        let legacy = ["https://x/kyde-macos.zip"];
        assert_eq!(
            pick_asset_url(&legacy, "aarch64").as_deref(),
            Some("https://x/kyde-macos.zip")
        );
        assert_eq!(
            pick_asset_url(&legacy, "x86_64").as_deref(),
            Some("https://x/kyde-macos.zip")
        );
        // Never install an explicitly wrong-arch build (only arm offered, we're Intel).
        assert_eq!(pick_asset_url(&[arm], "x86_64"), None);
        // No zip at all.
        assert_eq!(pick_asset_url(&["https://x/notes.txt"], "aarch64"), None);

        // Real shipping layout: arm64 keeps the generic `kyde-macos.zip` name (back-compat),
        // Intel is the suffixed one. arm64 falls through to the generic; Intel matches.
        let shipping = ["https://x/kyde-macos.zip", intel];
        assert_eq!(
            pick_asset_url(&shipping, "aarch64").as_deref(),
            Some("https://x/kyde-macos.zip")
        );
        assert_eq!(pick_asset_url(&shipping, "x86_64").as_deref(), Some(intel));
    }

    /// The changelog feed (issue #71): drafts dropped, newest version first, CRLF cleaned,
    /// dates trimmed to `YYYY-MM-DD`, missing `name` falling back to the tag.
    #[test]
    fn parse_releases_orders_newest_first_and_skips_drafts() {
        let feed = r#"[
            { "tag_name": "kyde-v2.4.0", "name": "2.4.0", "body": "old\r\nnotes",
              "published_at": "2026-05-01T10:11:12Z", "html_url": "u24", "prerelease": false },
            { "tag_name": "kyde-v2.10.0", "name": "", "body": "newest",
              "published_at": "2026-07-01T00:00:00Z", "html_url": "u210" },
            { "tag_name": "kyde-v2.5.0", "name": "2.5.0", "body": "mid",
              "published_at": "2026-06-02T00:00:00Z", "html_url": "u25", "prerelease": true },
            { "tag_name": "kyde-v9.9.9", "name": "draft", "body": "x", "draft": true }
        ]"#;
        let rs = parse_releases(feed);
        // Numeric order (2.10 > 2.5), draft excluded.
        assert_eq!(
            rs.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(),
            ["2.10.0", "2.5.0", "2.4.0"]
        );
        assert_eq!(rs[0].title, "kyde-v2.10.0", "no name → tag as the title");
        assert_eq!(
            rs[2].body, "old\nnotes",
            "CRLF normalised for the md parser"
        );
        assert_eq!(rs[2].date, "2026-05-01");
        assert!(rs[1].prerelease);
        assert_eq!(rs[0].page_url, "u210");
    }

    #[test]
    fn parse_releases_tolerates_junk_and_single_objects() {
        assert!(parse_releases("not json").is_empty());
        assert!(parse_releases("[]").is_empty());
        // Tagless entries are skipped; a lone object (the `latest` endpoint) still parses.
        assert!(parse_releases(r#"[{ "body": "x" }]"#).is_empty());
        let one = parse_releases(r#"{ "tag_name": "v1.2.3", "body": "b" }"#);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].version, "1.2.3");
        assert_eq!(one[0].date, "", "no timestamp → empty date, not a panic");
    }

    #[test]
    fn parse_feed_handles_release_with_no_zip_asset() {
        let feed = r#"{ "tag_name": "v9.9.9", "html_url": "u", "assets": [] }"#;
        let r = parse_feed(feed, "1.0.0").unwrap();
        assert!(
            r.zip_url.is_empty(),
            "no zip → empty url, page link still works"
        );
        assert_eq!(r.page_url, "u");
    }
}
