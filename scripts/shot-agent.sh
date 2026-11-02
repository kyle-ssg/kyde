#!/usr/bin/env bash
#
# The stand-in "agent" for the agents screenshot (scripts/screenshots.sh agents). Prints a
# Claude Code-style session for the worktree it runs in, so the shot shows agents at work
# without spending a real session: the on-screen worktree's diff is the fixture's real edit.
#
# Each worktree's folder picks its scene, which decides its dot in the sidebar:
#   *-ease-lerp, *-tree-review  working (the spinner keeps printing)
#   *-find-word                 done (finishes, then quiet; the shot marks it unseen)
#   *-push-upstream             needs input (a permission prompt; the shot seeds the hook)
#   anything else               idle at the input box
set -u

E=$'\e'
fg() { printf '%s[38;2;%sm' "$E" "$1"; }
bg() { printf '%s[48;2;%sm' "$E" "$1"; }
R="$E[0m"
ORANGE=$(fg "215;119;87")
GREEN=$(fg "78;186;101")
GREY=$(fg "153;153;153")
DIMRULE=$(fg "88;88;88")
PURPLE=$(fg "175;135;255")
BOLD="$E[1m"
ADD=$(bg "28;62;36")
DEL=$(bg "86;30;38")
PROMPT_BG=$(bg "55;55;55")

clear_screen() { printf '%s[H%s[2J%s[3J' "$E" "$E" "$E"; }
title() { printf '%s]0;%s\a' "$E" "$1"; }
# "rows cols" of the terminal (asked of the tty: `tput cols` in a $(…) answers 80 on macOS).
size() { stty size </dev/tty 2>/dev/null || echo "24 80"; }
rule() {
    local rows cols
    read -r rows cols <<<"$(size)"
    printf '%s' "$DIMRULE"
    printf '─%.0s' $(seq 1 "$cols")
    printf '%s\n' "$R"
}
# A prompt you sent, echoed on its tint (Kyde highlights these and jumps between them).
you() { printf '%s❯ %s %s\n\n' "$PROMPT_BG" "$1" "$R"; }
say() { printf '⏺ %s\n\n' "$1"; }
tool() { printf '%s⏺%s %s%s%s(%s)\n' "$GREEN" "$R" "$BOLD" "$1" "$R" "$2"; }
out() { printf '  %s⎿  %s%s\n' "$GREY" "$1" "$R"; }
# A diff row: line number, sign, text, on the add/remove tint.
row() {
    local n="$1" sign="$2" text="$3" tint=""
    case "$sign" in +) tint="$ADD" ;; -) tint="$DEL" ;; esac
    printf '      %s%4s %s %s%s\n' "$tint" "$n" "$sign" "$text" "$R"
}
# The input box, with the cursor parked after its "❯" (a no-break space, as Claude's is).
input_box() {
    rule
    printf '❯\xc2\xa0\n'
    rule
    printf '  %s⏵⏵ accept edits on%s %s(shift+tab to cycle)%s' "$PURPLE" "$R" "$GREY" "$R"
    printf '%s[2A%s[3G' "$E" "$E"
}

# The spinner line, three rows above the input box's "❯" (where the cursor waits).
SPIN_GLYPHS=('·' '✢' '✳' '✶' '✻' '✽' '✻' '✶' '✳' '✢')
SPIN_I=0
spin_line() {
    printf '%s7%s[3A\r%s%s %s…%s %s(esc to interrupt · %ss · ↓ %s tokens)%s%s[K%s8' \
        "$E" "$E" "$ORANGE" "${SPIN_GLYPHS[SPIN_I % 10]}" "$VERB" "$R" "$GREY" "$SECS" "$TOKS" \
        "$R" "$E" "$E"
}
# Draw scene `$1` (ending above a spinner), then keep its spinner busy forever: the tab reads
# as working. `redraw` repaints it whenever the terminal's size changes (the on-screen tab is
# resized once Kyde lays it out, maybe after the first paint).
spin() {
    local scene="$1" redraw="$2" drawn now
    VERB="$3" SECS="$4" TOKS="$5"
    paint() { clear_screen; "$scene"; printf '\n\n\n'; input_box; spin_line; }
    drawn=$(size)
    paint
    while :; do
        sleep 0.12
        SPIN_I=$((SPIN_I + 1))
        [ $((SPIN_I % 8)) -eq 0 ] && SECS=$((SECS + 1))
        now=$(size)
        if [ "$redraw" = redraw ] && [ "$now" != "$drawn" ]; then
            drawn=$now
            paint
        else
            spin_line
        fi
    done
}

ease_lerp() {
    title "✳ Ease the shimmer"
    you "Pull the latest main first."
    tool Bash "git pull --ff-only"
    out "Already up to date."
    printf '\n'
    say "main is already up to date."
    you "The welcome shimmer looks mechanical. Ease it with smoothstep, not a linear blend."
    say "The shimmer's colours come from lerp_rgb in kyde-ui, so easing t there fixes every caller."
    tool Search 'pattern: "lerp_rgb"'
    out "Found 3 files"
    printf '\n'
    tool Read "crates/kyde-ui/src/color.rs"
    out "Read 15 lines"
    printf '\n'
    # The fixture's real edit (scripts/screenshots.sh), so the worktree's diff matches.
    tool Update "crates/kyde-ui/src/color.rs"
    out "Updated crates/kyde-ui/src/color.rs with 2 additions"
    row 5 ' ' 'pub fn lerp_rgb(a: u32, b: u32, t: f32) -> gpui::Rgba {'
    row 6 ' ' '    let t = t.clamp(0.0, 1.0);'
    row 7 + '    // Smoothstep: ease in and out instead of a constant rate.'
    row 8 + '    let t = t * t * (3.0 - 2.0 * t);'
    row 9 ' ' '    let chan = |hex: u32, shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;'
    printf '\n'
    tool Bash "cargo test -p kyde-ui"
    out "Running…"
}

tree_review() {
    title "✳ Review tree rebuilds"
    you "Review this branch: the file tree rebuild moved off the UI thread."
    tool Bash "git diff main --stat"
    out "crates/kyde-tree/src/lib.rs | 64 +++++++++++------"
}

# Off-screen scenes print once and go quiet (a redraw would read as fresh work).
idle() { while :; do sleep 1; done; }

clear_screen
case "$(basename "$PWD")" in
*-ease-lerp) spin ease_lerp redraw Easing 41 "2.3k" ;;
*-tree-review) spin tree_review once Reviewing 73 "4.8k" ;;
*-find-word)
    title "Whole-word find"
    you "Add a whole-word toggle to the find bar."
    tool Update "crates/kyde-ui/src/select.rs"
    out "Updated crates/kyde-ui/src/select.rs with 1 addition"
    printf '\n'
    say "Done: whole-word matching is in and committed on feat/find-whole-word."
    input_box
    idle
    ;;
*-push-upstream)
    title "✳ Push without upstream"
    you "Push fails when the branch has no upstream. Set it on first push."
    tool Bash "git push --set-upstream origin fix/push-upstream"
    printf '\n%sBash command%s\n\n  git push --set-upstream origin fix/push-upstream\n\n' "$BOLD" "$R"
    printf 'Do you want to proceed?\n%s❯ 1. Yes%s\n  2. No, and tell Claude what to do differently\n' "$PURPLE" "$R"
    idle
    ;;
*)
    title "Claude Code"
    input_box
    idle
    ;;
esac
