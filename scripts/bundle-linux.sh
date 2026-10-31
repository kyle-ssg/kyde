#!/usr/bin/env bash
# Wrap the release binary in an AppImage — a single portable, double-clickable
# file with the Kyde icon + a .desktop launcher. Mirrors bundle-macos.sh.
#
#   ./scripts/bundle-linux.sh            # builds release, emits dist/Kyde-x86_64.AppImage
#
# Note: this does NOT bundle gpui's system libraries (Vulkan/Wayland/X11/xkb).
# The target machine needs those installed — a stock Ubuntu desktop already has
# them (README "Install" lists the apt line for a minimal one). Bundling them is
# deliberately skipped: the Vulkan loader must come from the host's GPU driver
# stack, and glibc can't be bundled at all — the oldest distro the AppImage runs
# on is set by the glibc of the machine that BUILDS it (release.yml pins it).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Always (re)build — cargo is incremental, so this is cheap when nothing changed,
# and it avoids packaging a STALE binary just because one already exists.
echo "building release binary..."
cargo build --release
BIN="target/release/kyde"

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

APPDIR="dist/Kyde.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/scalable/apps"

cp "$BIN" "$APPDIR/usr/bin/kyde"

# Icon: the rounded tile with margins (same art as the macOS Dock icon) — the
# full-bleed AppIcon.png reads as a hard square next to native Linux app icons
# too. The SVG covers every size the icon theme asks for; the PNG at the AppDir
# root (named after the desktop entry) becomes the AppImage's own .DirIcon.
cp assets/logo-icon.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/kyde.svg"
cp assets/logo.png "$APPDIR/kyde.png"

# StartupWMClass = the window's app id (APP_ID in src/main.rs → X11 WM_CLASS /
# Wayland app_id), so the dock groups Kyde's windows under this entry.
cat > "$APPDIR/kyde.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Kyde
GenericName=Git commit & diff tool
Exec=kyde
Icon=kyde
Categories=Development;RevisionControl;
Terminal=false
StartupWMClass=kyde
EOF
cp "$APPDIR/kyde.desktop" "$APPDIR/usr/share/applications/kyde.desktop"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/kyde" "$@"
EOF
chmod +x "$APPDIR/AppRun"

# appimagetool + the AppImage runtime it embeds, pinned to tagged releases and
# checked against their published SHA-256 (the "continuous" builds change daily).
# APPIMAGETOOL=/path/to/appimagetool uses a local one instead of downloading.
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage"
APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64"
RUNTIME_SHA256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"

# fetch <url> <sha256> <dest>: download unless dest already matches, verify, keep.
fetch() {
  if [[ -f "$3" ]] && echo "$2  $3" | sha256sum -c --status; then
    return
  fi
  echo "downloading $(basename "$1")…"
  curl -fsSL -o "$3.part" "$1"
  if ! echo "$2  $3.part" | sha256sum -c --status; then
    echo "checksum mismatch for $1" >&2
    rm -f "$3.part"
    exit 1
  fi
  mv "$3.part" "$3"
}

TOOL="${APPIMAGETOOL:-}"
if [[ -z "$TOOL" ]]; then
  TOOL="dist/appimagetool-1.9.1"
  fetch "$APPIMAGETOOL_URL" "$APPIMAGETOOL_SHA256" "$TOOL"
  chmod +x "$TOOL"
fi
RUNTIME="dist/runtime-x86_64-20251108"
fetch "$RUNTIME_URL" "$RUNTIME_SHA256" "$RUNTIME"

# Extract-and-run avoids needing FUSE (GitHub runners don't have it).
export APPIMAGE_EXTRACT_AND_RUN=1
ARCH=x86_64 VERSION="$VERSION" "$TOOL" --runtime-file "$RUNTIME" \
  "$APPDIR" "dist/Kyde-x86_64.AppImage"
echo "built dist/Kyde-x86_64.AppImage"
