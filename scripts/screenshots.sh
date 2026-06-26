#!/usr/bin/env bash
#
# Regenerate every screenshot referenced in README.md by launching the real Kyde app in
# each documented UI state and capturing its window with macOS `screencapture`.
#
#   ./scripts/screenshots.sh            # all shots, release build
#   ./scripts/screenshots.sh git-diff   # one shot by name
#   PROFILE=debug ./scripts/screenshots.sh
#
# How it works: the app reads KYDE_SHOT=<name> at launch (see apply_shot in src/main.rs) and
# drives itself straight into the right view. We run it against THIS repo under a throwaway
# XDG_CONFIG_HOME (so the user's real ~/.config/kyde is untouched), find its window via
# scripts/winid.swift, screencapture it, then kill it.
#
# REQUIREMENT: the terminal running this needs macOS Screen Recording permission
# (System Settings → Privacy & Security → Screen Recording), or screencapture silently
# produces black/empty images.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"

OUT="$ROOT/assets/screenshots"
WINID="$ROOT/scripts/winid.swift"
FIX="$ROOT/target/shot-fixtures"
LOCK="$FIX/package-lock.json"
CFG_ROOT="$(mktemp -d)"
CFG="$CFG_ROOT/config"
PROFILE="${PROFILE:-release}"

# Throwaway config: seed a keymap so first-run onboarding doesn't cover the shot.
mkdir -p "$CFG/kyde"
printf '{\n  "preset": "webstorm",\n  "overrides": {}\n}\n' > "$CFG/kyde/keymap.json"

# `region`-mode shots grab raw screen pixels in the window's rect, so the Dock OR any other
# app window overlapping that rect (your terminal, editor, …) lands in the shot. Defend on two
# fronts: (1) auto-hide the Dock, (2) hide every other app and bring Kyde frontmost before each
# capture (see `focus_only_kyde`). Both are restored on exit (incl. error / Ctrl-C).
DOCK_AUTOHIDE_WAS="$(osascript -e 'tell application "System Events" to get autohide of dock preferences' 2>/dev/null || echo true)"
osascript -e 'tell application "System Events" to set autohide of dock preferences to true' >/dev/null 2>&1 || true
# Apps visible right now → restore exactly these (un-hide) when we're done.
PRIOR_VISIBLE="$(osascript -e 'tell application "System Events" to get name of (every process whose visible is true and background only is false)' 2>/dev/null || true)"
cleanup() {
    rm -rf "$CFG_ROOT"
    osascript -e "tell application \"System Events\" to set autohide of dock preferences to $DOCK_AUTOHIDE_WAS" >/dev/null 2>&1 || true
    if [ -n "${PRIOR_VISIBLE:-}" ]; then
        local oldIFS="$IFS"; IFS=','
        for app in $PRIOR_VISIBLE; do
            app="${app# }" # AppleScript joins names with ", " → trim the leading space
            osascript -e "tell application \"System Events\" to set visible of (every process whose name is \"$app\") to true" >/dev/null 2>&1 || true
        done
        IFS="$oldIFS"
    fi
}
trap cleanup EXIT

# Bring Kyde (its own pid) frontmost and hide every other foreground app, so a region grab
# can only ever contain Kyde's windows over the empty desktop. Kyde keeps BOTH its windows
# (main + modal) — they share the process, which stays frontmost.
focus_only_kyde() {
    local pid="$1"
    osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $pid) to true" >/dev/null 2>&1 || true
    osascript -e 'tell application "System Events" to set visible of (every process whose visible is true and frontmost is false and background only is false) to false' >/dev/null 2>&1 || true
}

echo "==> building ($PROFILE)"
if [ "$PROFILE" = release ]; then
    cargo build --release >/dev/null
    BIN="$ROOT/target/release/kyde"
else
    cargo build >/dev/null
    BIN="$ROOT/target/debug/kyde"
fi

# Only the (manual) `fps` shot uses the big package-lock fixture; skip it for the automated set.
if [ "${1:-all}" = fps ]; then
    echo "==> generating FPS fixture"
    mkdir -p "$FIX"
    python3 "$ROOT/scripts/gen_lock.py" "$LOCK" 37000 >/dev/null
    LOCK_REL="${LOCK#$ROOT/}"
fi

# git-diff fixture: a CLONE of this real Kyde repo (committed HEAD → full real tree + history),
# with a working-tree diff so the git-diff / light shots render a genuine Kyde side-by-side diff
# — real paths, real Rust, syntax-highlighted — instead of a toy hello-world. Two changes:
#   1. a curated edit to crates/kyde-ui/src/color.rs (the showcase the diff pane opens — one
#      in-place word change + one added line → exercises word-level highlight + the gutter), and
#   2. select.rs reverted to the commit before its last change → a second, real, zero-maintenance
#      diff so the changed-files list reads like real WIP.
# Passed to the shot via KYDE_SHOT_REPO; apply_shot opens it + selects color.rs.
case "${1:-all}" in
all | git-diff | light)
    echo "==> generating git-diff fixture (clone of this repo)"
    # Clone into a dir literally named `kyde` so the commit tree's root row reads authentically.
    DIFF_REPO="$FIX/kyde"
    rm -rf "$DIFF_REPO"
    mkdir -p "$FIX"
    git clone --quiet --no-hardlinks "$ROOT" "$DIFF_REPO"

    # 1. Curated showcase edit on a real, stable file.
    COLOR="$DIFF_REPO/crates/kyde-ui/src/color.rs"
    perl -0pi -e 's/Linearly interpolate/Smoothly interpolate/' "$COLOR"
    perl -0pi -e 's/(\n    let t = t\.clamp\(0\.0, 1\.0\);\n)/$1    let t = t * t * (3.0 - 2.0 * t); \/\/ smoothstep so the shimmer eases\n/' "$COLOR"
    if git -C "$DIFF_REPO" diff --quiet -- crates/kyde-ui/src/color.rs; then
        echo "    !! color.rs showcase edit did not apply (anchors moved?) — fix screenshots.sh" >&2
        exit 1
    fi

    # 2. A second, real diff: reproduce select.rs's most recent change as a working-tree edit.
    PREV_SELECT="$(git -C "$DIFF_REPO" log -n1 --skip=1 --format=%H -- crates/kyde-ui/src/select.rs)"
    [ -n "$PREV_SELECT" ] && git -C "$DIFF_REPO" checkout -q "$PREV_SELECT" -- crates/kyde-ui/src/select.rs
    ;;
esac

# shoot <shot-name> <output-file> <capture-mode> [extra env KEY=VAL ...]
#   capture-mode: window  → screencapture -l<frontmost window>   (full-bleed window)
#                 region  → screencapture -R<main-window bounds + margin>  (window over desktop)
shoot() {
    local name="$1" outfile="$2" mode="$3"; shift 3
    local out="$OUT/$outfile"
    echo "==> $name → $outfile"

    env XDG_CONFIG_HOME="$CFG" KYDE_SHOT="$name" "$@" "$BIN" "$ROOT" >/dev/null 2>&1 &
    local pid=$!

    # rollback / plugins-window open a second (modal) window; everything else has just one.
    local need=1
    case "$name" in rollback|plugins-window) need=2 ;; esac

    local tries=0 count=0
    while [ $tries -lt 80 ]; do
        count=$(swift "$WINID" "$pid" 2>/dev/null | grep -c . || true)
        [ "$count" -ge "$need" ] && break
        sleep 0.25; tries=$((tries + 1))
    done
    if [ "$count" -lt "$need" ]; then
        echo "    !! window never appeared (pid $pid) — skipping"
        kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true
        return 1
    fi
    focus_only_kyde "$pid" # Kyde frontmost + every other app hidden → nothing else in frame
    sleep 1  # let the hide settle + async layout / modal open / first paint settle

    # Fix the capture target now the window exists (window id for full-bleed; largest-window
    # rect for a modal-over-main shot).
    local lines; lines="$(swift "$WINID" "$pid" 2>/dev/null)"
    local id rect
    id="$(printf '%s\n' "$lines" | head -1 | awk '{print $1}')"
    rect="$(printf '%s\n' "$lines" | awk '
        { area = $4 * $5; if (area > max) { max = area; x = $2; y = $3; w = $4; h = $5 } }
        END { printf "%d,%d,%d,%d", x, y, w, h }')"
    grab() {
        # window: frontmost window edge-to-edge (no shadow). region: the main window's rect
        # (a modal floats centered inside it → grabs the modal over the content, full-bleed).
        # Capture to a temp first and only promote it on success — screencapture sometimes
        # returns "could not create image from window" (window not yet on-screen), and we must
        # not leave a stale $out looking like it succeeded. Returns non-zero on failure.
        local tmp="$out.tmp.png"; rm -f "$tmp"
        if [ "$mode" = window ]; then screencapture -x -o -l"$id" "$tmp" 2>/dev/null
        else screencapture -x -R"$rect" "$tmp" 2>/dev/null; fi
        if [ -s "$tmp" ]; then mv -f "$tmp" "$out"; return 0; fi
        rm -f "$tmp"; return 1
    }

    # Grab the frame. `screencapture` can briefly fail with "could not create image from window"
    # right after the window appears, so retry a few times before giving up on this launch.
    local got=0 a=0
    while [ $a -lt 20 ]; do
        if grab; then got=1; break; fi
        sleep 0.2; a=$((a + 1))
    done
    if [ $got -eq 0 ]; then
        echo "    !! capture failed — retrying"
        kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true
        return 1
    fi

    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    [ -f "$out" ] && echo "    wrote $out ($(du -h "$out" | awk '{print $1}'))"
}

# shoot_welcome_gif — the README hero. Launches Kyde with NO repo arg (→ Projects landing) in
# the welcome state (animated 3D KYDE logo + diagonal shimmer), then grabs a rapid burst of
# window frames and assembles them into a looping GIF with ImageMagick `convert`. The shimmer
# is driven by `request_animation_frame`, so consecutive grabs catch successive phases.
# Output: assets/screenshots/welcome.gif. Run manually (like `fps`):  ./scripts/screenshots.sh welcome-gif
shoot_welcome_gif() {
    local out="$OUT/welcome.gif"
    local tmp; tmp="$(mktemp -d)"
    local frames="${GIF_FRAMES:-40}" delay="${GIF_DELAY:-5}" width="${GIF_WIDTH:-960}"
    echo "==> welcome-gif → welcome.gif (${frames} frames)"

    env XDG_CONFIG_HOME="$CFG" KYDE_SHOT=welcome "$BIN" >/dev/null 2>&1 &
    local pid=$!

    local tries=0 count=0
    while [ $tries -lt 80 ]; do
        count=$(swift "$WINID" "$pid" 2>/dev/null | grep -c . || true)
        [ "$count" -ge 1 ] && break
        sleep 0.25; tries=$((tries + 1))
    done
    if [ "$count" -lt 1 ]; then
        echo "    !! window never appeared (pid $pid)"; kill "$pid" 2>/dev/null || true
        rm -rf "$tmp"; return 1
    fi
    focus_only_kyde "$pid"
    sleep 1
    local id; id="$(swift "$WINID" "$pid" 2>/dev/null | head -1 | awk '{print $1}')"

    # Burst-capture the shimmer. Zero-padded names so the glob sorts in capture order.
    local i
    for i in $(seq -w 1 "$frames"); do
        screencapture -x -o -l"$id" "$tmp/f$i.png" 2>/dev/null || true
        sleep 0.05
    done
    kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true

    if ! ls "$tmp"/f*.png >/dev/null 2>&1; then
        echo "    !! no frames captured (Screen Recording permission?)"; rm -rf "$tmp"; return 1
    fi
    # Assemble: infinite loop, downscaled to README width, palette-optimized to keep size down.
    convert -delay "$delay" -loop 0 "$tmp"/f*.png -resize "${width}x" -layers Optimize -fuzz 4% "$out"
    rm -rf "$tmp"
    [ -f "$out" ] && echo "    wrote $out ($(du -h "$out" | awk '{print $1}'))"
}

# Map each README screenshot to its shot. (file ← state)
# `fps` and `welcome-gif` are intentionally NOT in the automated set — they're heavier / special
# (FPS counter, GIF burst) and run manually on demand:
#   ./scripts/screenshots.sh fps
#   ./scripts/screenshots.sh welcome-gif
declare -a ALL=(git-diff light plugins plugins-window markdown-support go-to-file find-in-files rollback history terminal)
want="${1:-all}"

MAX_TRIES="${MAX_TRIES:-5}"

# shoot_until <shoot args…> — capture, relaunching if a launch's window never appears or the
# grab keeps failing, up to MAX_TRIES.
shoot_until() {
    local n=0
    until shoot "$@"; do
        n=$((n + 1))
        if [ "$n" -ge "$MAX_TRIES" ]; then
            echo "    !! gave up after $n tries"
            return 1
        fi
        echo "    retry $n/${MAX_TRIES}..."
        sleep 1
    done
}

run_one() {
    case "$1" in
        git-diff)         shoot_until git-diff         git-diff.png         window KYDE_SHOT_REPO="$DIFF_REPO" ;;
        light)            shoot_until light            light.png            window KYDE_SHOT_REPO="$DIFF_REPO" ;;
        plugins)          shoot_until plugins          plugins.png          window ;;
        plugins-window)   shoot_until plugins-window   plugins-window.png   region ;;
        markdown-support) shoot_until markdown-support markdown-support.png window ;;
        go-to-file)       shoot_until go-to-file       go-to-file.png       window ;;
        find-in-files)    shoot_until find-in-files    find-in-files.png    window ;;
        rollback)         shoot_until rollback         rollback.png         region ;;
        history)          shoot_until history          history.png          window ;;
        terminal)         shoot_until terminal         terminal.png         window ;;
        fps)              shoot_until fps              fps.png              window KYDE_SHOT_FILE="$LOCK_REL" ;;
        welcome-gif)      shoot_welcome_gif ;;
        *) echo "unknown shot: $1"; exit 2 ;;
    esac
}

if [ "$want" = all ]; then
    for s in "${ALL[@]}"; do run_one "$s"; done
else
    run_one "$want"
fi

echo "==> done"
