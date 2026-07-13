#!/usr/bin/env bash
#
# Prove the self-updater end-to-end, fully offline — no GitHub, no /Applications touched.
#
#   ./scripts/test-update.sh
#
# What it does:
#   1. Builds dist/Kyde.app (the "installed" app).
#   2. Fabricates a *newer* release locally: a copy of the bundle with its Info.plist version
#      bumped to 9.9.9, zipped as the release asset, described by a fixture latest.json whose
#      asset URL is a file:// path. (No recompile — we're proving the download+swap mechanism,
#      and the bumped Info.plist is the on-disk marker that the swap actually happened.)
#   3. Launches dist/Kyde.app's binary directly with:
#        KYDE_VERSION_OVERRIDE=0.0.1      → the running app is "behind" any fixture
#        KYDE_UPDATE_FEED_URL=file://…    → "latest release" comes from the local fixture
#      so the update banner appears for v9.9.9.
#   4. You click "Update & Relaunch". The app downloads the fixture zip, swaps dist/Kyde.app
#      in place, and relaunches.
#   5. Press Enter back here; the script asserts dist/Kyde.app's on-disk version is now 9.9.9.
#
# The swap targets whatever bundle current_exe() resolves to — here dist/Kyde.app — so a real
# install elsewhere is never touched.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"

FIX_VERSION="9.9.9"
APP="$ROOT/dist/Kyde.app"
FIX="$ROOT/dist/update-fixture"
BIN="$APP/Contents/MacOS/kyde"

echo "==> building dist/Kyde.app (the installed app)"
./scripts/bundle-macos.sh >/dev/null

echo "==> fabricating a newer ($FIX_VERSION) release fixture"
rm -rf "$FIX"
mkdir -p "$FIX"
cp -R "$APP" "$FIX/Kyde.app"
# Bump the copy's bundle version → the on-disk proof that the swap replaced the app.
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $FIX_VERSION" "$FIX/Kyde.app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $FIX_VERSION" "$FIX/Kyde.app/Contents/Info.plist"
# Re-sign (editing Info.plist invalidated the ad-hoc signature).
codesign --force -s - "$FIX/Kyde.app" >/dev/null 2>&1 || true
# Package exactly like the release workflow does — including the .sha256 checksum asset,
# so the updater's verification path is exercised end-to-end (it verifies whenever the
# checksum asset is present, file:// fixtures included).
ditto -c -k --keepParent "$FIX/Kyde.app" "$FIX/kyde-macos.zip"
(cd "$FIX" && shasum -a 256 kyde-macos.zip > kyde-macos.zip.sha256)

cat > "$FIX/latest.json" <<JSON
{
  "tag_name": "v$FIX_VERSION",
  "html_url": "https://github.com/kyle-ssg/kyde/releases",
  "assets": [
    { "browser_download_url": "file://$FIX/kyde-macos.zip" },
    { "browser_download_url": "file://$FIX/kyde-macos.zip.sha256" }
  ]
}
JSON

echo "==> on-disk version BEFORE update:"
/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Contents/Info.plist"

echo
echo "==> launching dist/Kyde.app pointed at the fixture feed…"
echo "    A banner 'Update available — v$FIX_VERSION' should appear. Click 'Update & Relaunch'."
KYDE_VERSION_OVERRIDE=0.0.1 \
KYDE_UPDATE_FEED_URL="file://$FIX/latest.json" \
  "$BIN" "$ROOT" >/dev/null 2>&1 &

echo
read -r -p "Clicked it and the app relaunched? Press Enter to verify… " _

GOT="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Contents/Info.plist" 2>/dev/null || echo "?")"
echo "==> on-disk version AFTER update: $GOT"
if [ "$GOT" = "$FIX_VERSION" ]; then
  echo "✅ PASS — the bundle was downloaded, unzipped, and swapped in place."
else
  echo "❌ FAIL — expected $FIX_VERSION, dist/Kyde.app is still $GOT (swap did not happen)."
  exit 1
fi

# Clean up the fixture (leave dist/Kyde.app — it's now the 9.9.9 copy).
rm -rf "$FIX"
echo "==> done (fixture removed; dist/Kyde.app left as the swapped 9.9.9 copy)"
