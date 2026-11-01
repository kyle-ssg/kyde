<p align="center">
  <img src="assets/logo.png" alt="Kyde logo" width="112" height="112">
</p>

<h1 align="center">Kyde</h1>

<p align="center">
  <b>A fast, native Git client and side-by-side diff editor for macOS and Linux.</b><br>
  Stage hunks, resolve merge conflicts, browse history and run Claude Code agents across git worktrees — GPU-rendered, written in Rust, no Electron.
</p>

<p align="center">
  <a href="https://kyle-ssg.github.io/kyde/"><b>Website</b></a> · <a href="https://github.com/kyle-ssg/kyde/releases"><b>Download</b></a>
</p>

<p align="center">
  <a href="https://github.com/kyle-ssg/Kyde/actions/workflows/build.yml"><img src="https://github.com/kyle-ssg/Kyde/actions/workflows/build.yml/badge.svg" alt="build"></a>
  <a href="https://github.com/kyle-ssg/Kyde/actions/workflows/cargo-deny.yml"><img src="https://github.com/kyle-ssg/Kyde/actions/workflows/cargo-deny.yml/badge.svg" alt="cargo-deny"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
</p>

<p align="center">
  <img src="assets/screenshots/welcome.gif" alt="Kyde's animated welcome screen — a 3D KYDE logo with a diagonal shimmer sweep, over New Project / Open Folder buttons" width="720">
</p>

I don't know Rust.

These days I barely open my full IDE — pre-AI I was doing thousands of commits a year, and now I mostly live in its commit and diff view, one of the few things heavy enough to make me wait for a JVM to start.

So Kyde is just that: the commit view, side-by-side diff, merge tool and git log I used to start a whole IDE for, in an app that opens instantly.

<p align="center">
  <img src="assets/screenshots/git-diff.png" alt="Kyde commit view — side-by-side Git diff with word-level highlighting and a center gutter to stage or revert individual hunks" width="900">
</p>

## Why Kyde

* **Instant.** Native and GPU-rendered on [gpui](https://gpui.rs), the framework behind Zed. No JVM, no Electron — it opens in a blink and scrolls a 37k-line file at 120fps.
* **An IDE's git tools, without the IDE.** IntelliJ-style commit view, per-hunk staging, 3-way merge, history graph and local history: the parts you actually open an IDE for.
* **Built for running agents.** Many Claude Code agents, each in its own git worktree, with live status, GitHub issues and PRs one click from a briefed agent, and cleanup when the work lands.
* **Feels familiar.** WebStorm or VSCode keymap, an IDE-style dark theme (plus light and colour-blind palettes), keyboard everywhere.

## What it does

### Git

* **Commit** — changed files beside an editable side-by-side diff with word-level highlighting. Stage or revert any hunk from the center gutter: `git add -p`, made visual.
* **Merge** — a conflicts list, then a 3-pane yours | result | theirs resolver.
* **History** — branches sidebar, commit graph, and each commit's diff against its parent, the latest, or your working tree.
* **Local History** — snapshots independent of git, taken on save and before every destructive action, with per-hunk restore.
* **The rest** — push, pull and fetch from the toolbar, branch and worktree switchers, rollback, and compare any two files.

<p align="center">
  <img src="assets/screenshots/merge.png" alt="3-pane merge view — yours, result, and theirs aligned side by side; red unresolved conflicts with apply/ignore gutter controls, applied changes in blue, plus apply-non-conflicting, Compare Contents, and whitespace options in the toolbar" width="900">
</p>

<p align="center">
  <img src="assets/screenshots/history.png" alt="History view — branches sidebar, commit list with a branch and merge graph, the selected commit's changed files, and a read-only side-by-side diff comparable against the parent, latest, or local working tree" width="900">
</p>

### Agents

A home for Claude Code (or any agent CLI): projects → worktrees → tabs, each tab an agent.

* **Live status** — working, done, or needs you, with a sound when one finishes and a key to jump to it.
* **Issue to agent in one click** — start a GitHub issue or review a PR and get a fresh worktree with an agent already briefed. Issues and PRs render in-app, comments and all.
* **New task, new worktree** — ask a busy agent for something unrelated and it moves itself to a clean worktree.
* **Outlives the window** — quit Kyde and agents keep working; reopen and they're back.
* **Cleans up** — spots merged or finished work and closes its tabs and worktrees.
* **Scriptable** — `ky agent …` drives agents from the shell, so agents can drive each other.

<p align="center">
  <img src="assets/screenshots/agents.png" alt="Agents view — a project's worktrees in the sidebar with status dots and git badges, and the selected worktree's agent running in its tab" width="900">
</p>

### Editor

* Folder tree, tabs, file management, and fuzzy Go to File, Find Action and Find in Files.
* Tree-sitter highlighting for 16 languages, installed on demand, with folding, error squiggles and ⌘-click to definition.
* Markdown and image preview, and a built-in terminal. Drop a file on it, or on an agent, to paste its path.

<p align="center">
  <img src="assets/screenshots/plugins.png" alt="Browse view — folder tree, editor tabs, and an opt-in language-pack install banner" width="900">
</p>

## Install

Grab the latest build from [Releases](../../releases).

* **macOS** — `kyde-macos.zip` (Apple Silicon) or `kyde-macos-x86_64.zip` (Intel). Unzip and drag **Kyde.app** to `/Applications`.
* **Linux** — `kyde-linux-x86_64.AppImage`. `chmod +x` it and run it. Needs glibc 2.35+ (Ubuntu 22.04 or newer) and a Vulkan driver.

First launch offers to install a `ky` command: `ky` opens your projects, `ky /path/to/repo` opens a repo.

## Build

```sh
cargo build --release
cargo test --workspace
```

Needs Rust 1.96+. On macOS, gpui also needs Apple's Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`). On Linux, install the dev libraries first:

```sh
sudo apt install pkg-config libssl-dev libzstd-dev libwayland-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libfontconfig-dev libfreetype-dev libx11-dev libxcb1-dev \
  libvulkan-dev clang cmake
```

Each language is a Cargo feature, so a smaller build can leave grammars out: `cargo build --release --no-default-features --features rust,json,toml`.

## FAQ

**Is Kyde free and open source?**
Yes — Apache-2.0.

**Does it replace my IDE?**
It replaces the part I kept opening one for: commit, diff, merge, history and quick edits. There's no language server, so no autocomplete or refactoring.

**Does it run on Windows?**
Not as a download yet, but `cargo build --release` works. Packaging it is a good first issue.

**Which AI coding agents does it work with?**
Claude Code by default, with live status from a hook Kyde installs. The start command is configurable, so other agent CLIs run too, with a simpler status dot.

## Contributing

If you open a PR, please be kind — explain the idiomatic way, don't just dunk on my `.clone()`s. I'll read every comment and quietly Google what a lifetime is.

If the feature adds a lot of bloat, it should be a plugin.
