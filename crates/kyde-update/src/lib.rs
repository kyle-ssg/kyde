//! Self-update. Checks GitHub for a newer release, downloads the notarized `Kyde.app` zip,
//! swaps it over the running bundle, and the caller relaunches. Network + unzip are shelled
//! to `curl` / `ditto` (same shell-out philosophy as the git layer — no HTTP crate in the
//! default build).
//!
//! Test seams (so the whole flow is provable in dev, offline — see `scripts/test-update.sh`):
//! - `KYDE_VERSION_OVERRIDE` — pretend the running app is this version (force the banner).
//! - `KYDE_UPDATE_FEED_URL` — fetch the "latest release" JSON from here instead of GitHub. A
//!   `file://` fixture works, and its asset URL may be `file://` too.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// GitHub "latest release" API for this repo.
const DEFAULT_FEED: &str = "https://api.github.com/repos/kyle-ssg/kyde/releases/latest";

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
    /// Release page URL — the fallback when we can't swap in place (dev binary / no asset).
    pub page_url: String,
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
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn norm(tag: &str) -> String {
    parse_semver(tag)
        .map(|(a, b, c)| format!("{a}.{b}.{c}"))
        .unwrap_or_else(|| tag.trim().to_string())
}

/// The running `.app` bundle, if we were launched from one
/// (`…/Kyde.app/Contents/MacOS/kyde`). `None` for the bare dev binary, in which case the
/// caller opens the release page instead of swapping in place.
pub fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|a| a.extension().and_then(|e| e.to_str()) == Some("app"))
        .map(|a| a.to_path_buf())
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
    Some(Release {
        version: norm(&tag),
        tag,
        zip_url,
        page_url,
    })
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

/// Download `zip_url`, unzip, and swap the new `Kyde.app` over `bundle` in place. On success
/// the caller relaunches (`open <bundle>`) and quits. Runs off the UI thread (blocking I/O).
pub fn download_and_swap(zip_url: &str, bundle: &Path) -> Result<()> {
    if zip_url.is_empty() {
        return Err(anyhow!("release has no downloadable zip"));
    }
    let tmp = std::env::temp_dir().join(format!("kyde-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).context("creating temp dir")?;

    // Download (curl handles `file://` for the dev fixture too).
    let zip = tmp.join("kyde.zip");
    let out = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&zip)
        .arg(zip_url)
        .output()
        .context("spawning curl")?;
    if !out.status.success() {
        return Err(anyhow!(
            "download failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    // Unzip with ditto (preserves bundle symlinks/metadata/signature, unlike `unzip`).
    let extract = tmp.join("extract");
    std::fs::create_dir_all(&extract)?;
    let out = Command::new("ditto")
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(&extract)
        .output()
        .context("spawning ditto")?;
    if !out.status.success() {
        return Err(anyhow!(
            "unzip failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let new_app = find_app(&extract).ok_or_else(|| anyhow!("no .app found in the download"))?;
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
    std::fs::rename(bundle, &backup).with_context(|| format!("moving aside {bundle:?}"))?;
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
        return Err(anyhow!("release has no downloadable zip"));
    }
    std::fs::create_dir_all(dir).ok();
    let name = zip_url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("kyde-update.zip");
    let dest = dir.join(name);
    let out = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&dest)
        .arg(zip_url)
        .output()
        .context("spawning curl")?;
    if !out.status.success() {
        return Err(anyhow!(
            "download failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(dest)
}

/// Move (same volume) or copy (cross-volume, e.g. temp → /Applications) the new bundle in.
fn install(new_app: &Path, bundle: &Path) -> Result<()> {
    if std::fs::rename(new_app, bundle).is_ok() {
        return Ok(());
    }
    let out = Command::new("ditto")
        .arg(new_app)
        .arg(bundle)
        .output()
        .context("spawning ditto copy")?;
    if !out.status.success() {
        return Err(anyhow!(
            "install failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn find_app(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        (p.extension().and_then(|x| x.to_str()) == Some("app")).then_some(p)
    })
}

fn curl_text(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "User-Agent: kyde-updater", url])
        .output()
        .context("spawning curl")?;
    if !out.status.success() {
        return Err(anyhow!(
            "curl {url} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
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
                { "browser_download_url": "https://example.com/kyde-macos.zip" }
            ]
        }"#;
        // Newer than 1.0.0 → surfaced, picks the .zip asset.
        let r = parse_feed(feed, "1.0.0").expect("should surface a newer release");
        assert_eq!(r.version, "1.2.0");
        assert_eq!(r.tag, "v1.2.0");
        assert_eq!(r.zip_url, "https://example.com/kyde-macos.zip");
        assert!(r.page_url.contains("v1.2.0"));
        // Same/newer current → nothing to offer.
        assert_eq!(parse_feed(feed, "1.2.0"), None);
        assert_eq!(parse_feed(feed, "1.3.0"), None);
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
