# Changelog

All notable changes to Kyde are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
See [RELEASING.md](RELEASING.md) for how releases are cut.

## Unreleased

### Features

* **linux:** ship a Linux AppImage (`kyde-linux-x86_64.AppImage`) with every release again
* **linux:** the menu bar's items (Open, Recent Projects, Settings, Plugins, What's New, FPS monitor, Clear Data & Restart, Quit) behind a ☰ button in the title strip
* **linux:** agent alerts play their chime, and show a desktop notification while Kyde is in the background
* **terminal:** drop files on a terminal or agent tab to type in their paths, quoted for the shell (Claude Code takes a dropped image as pasted)
* **linux:** a File / View / Help menu bar where the window manager draws the title bar, in one row with the project switcher; F10 opens it, Alt + the underlined letter opens a menu, and Settings → Appearance → Menu bar picks the menu bar or the ☰ button
* **linux:** Match desktop (Settings → Appearance → Theme): the terminal takes your terminal app's font, the editor and UI your desktop's fonts, and the colours your desktop's dark preference, accent and GTK theme or KDE colour scheme; whatever Kyde can't read or draw stays its own
* **linux:** Settings → Appearance → Terminal → Your terminal: the terminal and agent tabs take your terminal app's background, text, 16 ANSI colours, bold and cursor colours and font, under any theme (GNOME Terminal, MATE Terminal, Xfce Terminal, Konsole, kitty, Alacritty, Ptyxis palettes of your own); Match desktop turns it on, and Kyde (the default) keeps Kyde's terminal
* **linux:** Settings → Appearance → Text → Your desktop's: text drawn as your desktop draws it, antialiased or crisp and with its hinting (from the X resources your desktop publishes, else its gsettings, else fontconfig), from the next start; Match desktop turns it on, and Kyde (the default) keeps Kyde's text exactly as before. Kyde now carries a small patch to cosmic-text, gpui's Linux text renderer, in `vendor/`

### Bug Fixes

* **linux:** group Kyde's windows under its own dock icon (window app id matches the `.desktop` entry)
* **linux:** no second, empty title strip under the window manager's title bar on X11; on Wayland desktops that don't draw one (GNOME), Kyde draws its own with move, resize and window buttons
* **linux:** Kyde's icon on its windows on X11, even when the AppImage isn't installed
* **linux:** the `ky` shell command installed from the AppImage keeps working after Kyde quits
* **linux:** the update banner's Download opens the release page with the system URL handler instead of fetching a macOS zip
* **linux:** shortcuts are on Ctrl instead of the Super key the desktop takes; in the terminal Ctrl + a letter goes to the shell (`Ctrl+C` interrupts) and the terminal's own keys take `Ctrl+Shift`; `Ctrl+←` / `Ctrl+→` move by word in the editor
* **linux:** Ctrl+click follows imports, multi-selects files for Compare and opens terminal links
* **linux:** tooltips, hints and Settings → Keymap write shortcuts as `Ctrl+Shift+O`, not `⌘⇧O`
* **linux:** Back / Forward, next / previous agent, agent tabs and Sort Lines take keys Linux desktops leave free (`Alt+←/→`, `Ctrl+Alt+PageDown/PageUp`, `Ctrl+PageDown/PageUp`, `F9`); the terminal leaves `Ctrl+Shift+K` to Push
* **linux:** shortcuts written with Shift + a symbol bind on every keyboard layout, read from the active layout
* **linux:** a dead key (´ ¨ ¸ or the grave accent, e.g. AltGr+= on a UK layout) no longer crashes Kyde on X11 with ibus; Kyde composes it itself when ibus only carries plain layouts
* **linux:** Reveal in File Manager and Open in Terminal work; the terminal is yours (`$TERMINAL`, then your desktop's preferred terminal)
* **linux:** paste files copied in a file manager into the tree, and middle-click paste the primary selection in the editor and terminal
* **terminal:** fall back to your login shell, not a missing `/bin/zsh`, when `SHELL` is unset
* **linux:** keep the single-instance socket in `$XDG_RUNTIME_DIR`, not the shared `/tmp`
* show "Live refresh off" in the status bar when the OS won't watch the project (e.g. Linux's inotify watch limit)
* say when the folder picker can't open (no XDG desktop portal) instead of doing nothing
* a second Open (or Add Project) while the folder picker is open waits for it, instead of opening another picker that can hide behind the first
* **linux:** a Kyde built from a shell inside the AppImage links `ky` to itself, not the AppImage
* relay Enter to the embedded terminal instead of consuming it as an app confirmation shortcut
* keep Windows builds compiling when Unix-only single-instance IPC is unavailable
* use the system command shell when opening the embedded terminal on Windows
* **linux:** the AppImage starts on glibc 2.35 and newer (Ubuntu 22.04, Debian 12, Linux Mint 21), not only on 2.39+

## [2.16.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.15.1...kyde-v2.16.0) (2026-11-12)


### Features

* **agents:** fold Add project / New group into a ⊕ menu ([#192](https://github.com/kyle-ssg/kyde/issues/192)) ([49f53a0](https://github.com/kyle-ssg/kyde/commit/49f53a03382814c11b1645ff43c6d7045826a390))
* **agents:** Issues / PRs / CI on a worktree's right-click menu ([#191](https://github.com/kyle-ssg/kyde/issues/191)) ([7a78611](https://github.com/kyle-ssg/kyde/commit/7a78611ffa717e38a98e8df41b6f67c65ee26a54))


### Bug Fixes

* **agents:** closing a tab ends everything its agent started ([#196](https://github.com/kyle-ssg/kyde/issues/196)) ([76e5422](https://github.com/kyle-ssg/kyde/commit/76e54222dc5055c1bd27e3bdc241a15936a4cd90))
* **agents:** tabs that don't fit the strip go behind a ▾ menu ([#194](https://github.com/kyle-ssg/kyde/issues/194)) ([c9727bd](https://github.com/kyle-ssg/kyde/commit/c9727bd7bc1a0eec0e122dd77fcdd858f13044ee))
* **terminal:** the prompt pill steps back through your prompts ([#195](https://github.com/kyle-ssg/kyde/issues/195)) ([0e1c612](https://github.com/kyle-ssg/kyde/commit/0e1c612c778a2d9d055f9bf9cb9af28e3b36554c))

## [2.15.1](https://github.com/kyle-ssg/kyde/compare/kyde-v2.15.0...kyde-v2.15.1) (2026-11-12)


### Bug Fixes

* **agents:** tell `ky agent new` agents how to move worktrees ([#189](https://github.com/kyle-ssg/kyde/issues/189)) ([286d6d1](https://github.com/kyle-ssg/kyde/commit/286d6d176f08271b7e96ee31bf43a30f23766cd7))

## [2.15.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.14.1...kyde-v2.15.0) (2026-11-11)


### Features

* **agents:** group sidebar projects into foldable, draggable groups ([#187](https://github.com/kyle-ssg/kyde/issues/187)) ([478acf9](https://github.com/kyle-ssg/kyde/commit/478acf9fb8ef4276925a47956cf629c8a03b8f44))
* **agents:** jump from a project to its issues, PRs and CI ([#186](https://github.com/kyle-ssg/kyde/issues/186)) ([0975540](https://github.com/kyle-ssg/kyde/commit/09755402485e26a025b09399515f14b1b13e3b3d))
* **agents:** pin Scratch to the bottom of the sidebar ([#188](https://github.com/kyle-ssg/kyde/issues/188)) ([1ebe903](https://github.com/kyle-ssg/kyde/commit/1ebe90310ba6428cd33ab9f34711f9a7bfed2a44))
* **agents:** pin worktrees, tab close menu, and a close confirmation ([#181](https://github.com/kyle-ssg/kyde/issues/181)) ([479398d](https://github.com/kyle-ssg/kyde/commit/479398d0aec635f26e5c76b440c5cbf68e3f322e))
* drag editor and agent tabs to reorder them ([#185](https://github.com/kyle-ssg/kyde/issues/185)) ([966c769](https://github.com/kyle-ssg/kyde/commit/966c7694a69de24c831859dd392a5bfd9574a65d))


### Bug Fixes

* **agents:** round the corners of the agent panel ([#180](https://github.com/kyle-ssg/kyde/issues/180)) ([d891ee8](https://github.com/kyle-ssg/kyde/commit/d891ee8a0a59013c72401c031d1a8ed5e9e80377))
* **agents:** scroll the tab strip when its tabs outgrow the pane ([#184](https://github.com/kyle-ssg/kyde/issues/184)) ([c9226fc](https://github.com/kyle-ssg/kyde/commit/c9226fc967c40fa00e0958e86920fd89dc57c82b))
* **agents:** tell an agent how to leave a worktree it entered before the next one ([#183](https://github.com/kyle-ssg/kyde/issues/183)) ([e6635a5](https://github.com/kyle-ssg/kyde/commit/e6635a5cade4d6f4db2637f822a32a08e8ee4602))

## [2.14.1](https://github.com/kyle-ssg/kyde/compare/kyde-v2.14.0...kyde-v2.14.1) (2026-11-10)


### Bug Fixes

* **agents:** start a seeded agent on a clean terminal ([#178](https://github.com/kyle-ssg/kyde/issues/178)) ([e733f48](https://github.com/kyle-ssg/kyde/commit/e733f48c5666714d5b417cdc311e4b404c4b8aaa))

## [2.14.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.13.0...kyde-v2.14.0) (2026-11-09)


### Features

* **agents:** Agents first, and one active worktree across every view ([#175](https://github.com/kyle-ssg/kyde/issues/175)) ([53d02bf](https://github.com/kyle-ssg/kyde/commit/53d02bfc664cd70b0dea112cfd426b2a55f3a0d4))
* **agents:** CI tab — GitHub Actions runs, retry failed ones, see why they failed ([#171](https://github.com/kyle-ssg/kyde/issues/171)) ([4254a8f](https://github.com/kyle-ssg/kyde/commit/4254a8fbb4ef0d5f1f9c6a9e1464d068db234416))
* **update:** recheck for a new release on window focus once 15 min stale ([#173](https://github.com/kyle-ssg/kyde/issues/173)) ([a2bfdcb](https://github.com/kyle-ssg/kyde/commit/a2bfdcbd823d854dedcbd563de089e5ea5a27d44))


### Bug Fixes

* **agents:** hand work in another project to a new agent there ([#174](https://github.com/kyle-ssg/kyde/issues/174)) ([095bf34](https://github.com/kyle-ssg/kyde/commit/095bf34c21082016d6f1f0303e4adb5c921eea2b))

## [2.13.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.12.1...kyde-v2.13.0) (2026-11-08)


### Features

* **terminal:** URLs and file paths are links — ⌘-click opens them ([#170](https://github.com/kyle-ssg/kyde/issues/170)) ([84d7feb](https://github.com/kyle-ssg/kyde/commit/84d7feb820d6ab3b15441bb3524c5934dd1ef369))


### Bug Fixes

* **agents:** a bot's PR comment has no Action comment button ([#167](https://github.com/kyle-ssg/kyde/issues/167)) ([c895509](https://github.com/kyle-ssg/kyde/commit/c8955091bf22ffa856fc19c5533fa9be23c5c6d6))
* **agents:** Claude Code's own helpers no longer show as background agent tabs ([#169](https://github.com/kyle-ssg/kyde/issues/169)) ([deda788](https://github.com/kyle-ssg/kyde/commit/deda788355ef7801821edf62c326ced9fb556d0f))
* **agents:** cleanup closes finished background agent tabs ([#163](https://github.com/kyle-ssg/kyde/issues/163)) ([#165](https://github.com/kyle-ssg/kyde/issues/165)) ([6c13b68](https://github.com/kyle-ssg/kyde/commit/6c13b680517544634400f6ecb183d536af77a0fa))
* **agents:** issue agents start on the fix, and shells get a UTF-8 locale ([#166](https://github.com/kyle-ssg/kyde/issues/166)) ([c429f04](https://github.com/kyle-ssg/kyde/commit/c429f043c5079812df2ca68a2ad222cd7146684f))
* **ui:** Enter confirms the Remove Worktree and Push windows ([#164](https://github.com/kyle-ssg/kyde/issues/164)) ([97aca78](https://github.com/kyle-ssg/kyde/commit/97aca788512d1dd561d2e5486f4d5944233ad023)), closes [#162](https://github.com/kyle-ssg/kyde/issues/162)

## [2.12.1](https://github.com/kyle-ssg/kyde/compare/kyde-v2.12.0...kyde-v2.12.1) (2026-11-04)


### Bug Fixes

* **agents:** Action comment buttons are standard blue buttons ([#159](https://github.com/kyle-ssg/kyde/issues/159)) ([4552b96](https://github.com/kyle-ssg/kyde/commit/4552b96422fb6acee6024d6e7f4adfed89758713))
* **ui:** a click outside an open dropdown closes it ([#160](https://github.com/kyle-ssg/kyde/issues/160)) ([3255ddc](https://github.com/kyle-ssg/kyde/commit/3255ddc40678c706e66652f69fc30a52855f529f))

## [2.12.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.11.1...kyde-v2.12.0) (2026-11-04)


### Features

* **agents:** hand a PR's review comments to an agent from its pull request view ([#154](https://github.com/kyle-ssg/kyde/issues/154)) ([48b9d62](https://github.com/kyle-ssg/kyde/commit/48b9d62e503e52d4426c8657a477497c2e4c7236))
* **agents:** Scratch — ad-hoc agent tabs, each in a throwaway folder of its own ([#157](https://github.com/kyle-ssg/kyde/issues/157)) ([47cb583](https://github.com/kyle-ssg/kyde/commit/47cb583c1e3b0cf78d64b2c583f3615bbc9fe7d7))


### Bug Fixes

* **agents:** a background agent's tab follows it, beside its live work ([#156](https://github.com/kyle-ssg/kyde/issues/156)) ([60aadb3](https://github.com/kyle-ssg/kyde/commit/60aadb3efc6f0fa5c0c94fb1c657cd55b3e2bd4c))
* **terminal:** resizing the window no longer scrambles an agent's text ([#153](https://github.com/kyle-ssg/kyde/issues/153)) ([cf43c47](https://github.com/kyle-ssg/kyde/commit/cf43c47d61d78967d14c2f6a90e8568c770916f3))

## [2.11.1](https://github.com/kyle-ssg/kyde/compare/kyde-v2.11.0...kyde-v2.11.1) (2026-11-02)


### Bug Fixes

* **agents:** a new worktree no longer shows up twice in the sidebar ([#144](https://github.com/kyle-ssg/kyde/issues/144)) ([d52ba6f](https://github.com/kyle-ssg/kyde/commit/d52ba6f996b94b1288fb81e9ec207f06a3c7fc18))
* **agents:** an agent's tab follows it into the worktree it edits, with no blank agent there ([#146](https://github.com/kyle-ssg/kyde/issues/146)) ([bf1ce5a](https://github.com/kyle-ssg/kyde/commit/bf1ce5a26f7706876d2bb4cd4c453b386c513285))

## [2.11.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.10.0...kyde-v2.11.0) (2026-11-02)


### Features

* **agents:** approve and merge pull requests from the PR page ([#140](https://github.com/kyle-ssg/kyde/issues/140)) ([3807b8f](https://github.com/kyle-ssg/kyde/commit/3807b8f8c2fb54ac597763e06bd31f7dd1bfa254))


### Bug Fixes

* a folder full of untracked files no longer freezes Kyde or eats all its memory ([#139](https://github.com/kyle-ssg/kyde/issues/139)) ([5f4860e](https://github.com/kyle-ssg/kyde/commit/5f4860e15f4c7ee08884dbf87258e3434fdd2b2a))
* **agents:** agent tabs draw at the panel's width without a window resize ([#143](https://github.com/kyle-ssg/kyde/issues/143)) ([76de8d2](https://github.com/kyle-ssg/kyde/commit/76de8d234776640f94913fa0f3acec86414b269a))

## [2.10.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.9.1...kyde-v2.10.0) (2026-11-01)


### Features

* **linux:** Match desktop with your terminal's look and your desktop's text, a File / View / Help menu bar, file drops on the terminal, and an AppImage for glibc 2.35+ ([#131](https://github.com/kyle-ssg/kyde/issues/131)) ([4dd3c17](https://github.com/kyle-ssg/kyde/commit/4dd3c17f6a0d8fbed0f1e6319e2c2a33eca99480))
* local builds show the Ky logo upside down ([#132](https://github.com/kyle-ssg/kyde/issues/132)) ([93f3922](https://github.com/kyle-ssg/kyde/commit/93f392235094b166bd55246f2c4e51d15e208156))


### Bug Fixes

* **agents:** Issues and Pull Requests find gh when Kyde is opened from the Dock ([#134](https://github.com/kyle-ssg/kyde/issues/134)) ([9c30353](https://github.com/kyle-ssg/kyde/commit/9c303530a2238d45fa6f03d3ec183b4968efca68))
* release-please skips the vendored cosmic-text, and dropped paths stay quoted in fish ([#135](https://github.com/kyle-ssg/kyde/issues/135)) ([35c50b9](https://github.com/kyle-ssg/kyde/commit/35c50b9e53b079053623393e554336110697cee9))

## [2.9.1](https://github.com/kyle-ssg/kyde/compare/kyde-v2.9.0...kyde-v2.9.1) (2026-11-01)


### Bug Fixes

* **agents:** every agent tab learns EnterWorktree moves it, a shell cd doesn't ([#129](https://github.com/kyle-ssg/kyde/issues/129)) ([17f57d3](https://github.com/kyle-ssg/kyde/commit/17f57d377ba10c2bf01dc8462b4ce4d9acb1969d))

## [2.9.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.8.0...kyde-v2.9.0) (2026-11-01)


### Features

* **agents:** a new task in a busy worktree moves to its own ([#118](https://github.com/kyle-ssg/kyde/issues/118)) ([fb20d19](https://github.com/kyle-ssg/kyde/commit/fb20d194ea03ad136ae12135a7defcdbeacc6881))
* **agents:** Claude's prompt suggestion acts as typed text in agent tabs ([#121](https://github.com/kyle-ssg/kyde/issues/121)) ([4941f78](https://github.com/kyle-ssg/kyde/commit/4941f78a97d203a05e0d60f656f80584b059899b))
* **agents:** the Clean Up window lists finished tabs, the main checkout's too ([#126](https://github.com/kyle-ssg/kyde/issues/126)) ([11b4f48](https://github.com/kyle-ssg/kyde/commit/11b4f4873ee1dc43478ff19a872b34963d352076))
* **find:** the terminal's find bar everywhere, and text that stays inside its box ([#128](https://github.com/kyle-ssg/kyde/issues/128)) ([9b940bc](https://github.com/kyle-ssg/kyde/commit/9b940bc1902434c2b20a37caa92aa20c951bf6c4))
* **linux:** a working Linux build — AppImage releases, Ctrl shortcuts, window chrome and desktop integration ([#115](https://github.com/kyle-ssg/kyde/issues/115)) ([722dda2](https://github.com/kyle-ssg/kyde/commit/722dda208ace2bca1d71ca7257339bf56292cd21))
* **terminal:** less lag under streaming output, find in scrollback (⌘F) and a scrollbar ([#119](https://github.com/kyle-ssg/kyde/issues/119)) ([533334b](https://github.com/kyle-ssg/kyde/commit/533334bd502743fda40406ae6565a11f534688d2))
* **terminal:** your prompts to Claude stand out, ⌘↑ / ⌘↓ jump between them ([#123](https://github.com/kyle-ssg/kyde/issues/123)) ([cbf1e1b](https://github.com/kyle-ssg/kyde/commit/cbf1e1b21bd3efea0610c334e805b675b8ff7e8c))


### Bug Fixes

* **agents:** a related task in a shared worktree leaves the other agent's work alone ([#122](https://github.com/kyle-ssg/kyde/issues/122)) ([3dfa47e](https://github.com/kyle-ssg/kyde/commit/3dfa47e6ab3c794b756c9ed90f4a9c5a1f0c6282))
* **agents:** Backspace clears Claude's prompt suggestion in agent tabs ([#127](https://github.com/kyle-ssg/kyde/issues/127)) ([a2cc9b3](https://github.com/kyle-ssg/kyde/commit/a2cc9b35788eae3f8333139a0db67754d6ea0328))
* **agents:** Enter sends Claude's prompt suggestion in agent tabs ([#125](https://github.com/kyle-ssg/kyde/issues/125)) ([a7bb945](https://github.com/kyle-ssg/kyde/commit/a7bb9451605ba04c60ad0603005ea1b1cd9ac8b7))
* **onboarding:** the keymap picker fits the window ([#120](https://github.com/kyle-ssg/kyde/issues/120)) ([45ec500](https://github.com/kyle-ssg/kyde/commit/45ec500d9928405574ce1aa03c6a495eb58feec6))
* **terminal:** Claude's prompt suggestion shows fainter than your typed text ([#124](https://github.com/kyle-ssg/kyde/issues/124)) ([f5558d6](https://github.com/kyle-ssg/kyde/commit/f5558d6d4b6d647202649e192f85cbe4c6097bce))

## [2.8.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.7.0...kyde-v2.8.0) (2026-10-30)


### Features

* **agents:** an Agents view for Claude Code — projects, worktrees, GitHub, agents that outlive Kyde ([#113](https://github.com/kyle-ssg/kyde/issues/113)) ([a8accec](https://github.com/kyle-ssg/kyde/commit/a8accecc998d46fa7d00f5c4983fe58b46f15050))

## [2.7.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.6.0...kyde-v2.7.0) (2026-10-14)


### Features

* **find:** ⌘F / ⌘R in every diff view, with visible match highlights ([#97](https://github.com/kyle-ssg/kyde/issues/97)) ([7a7fac0](https://github.com/kyle-ssg/kyde/commit/7a7fac077f9e9a9fae29990f0f5f3e5d8f2c02e1))
* IntelliJ-style git log (graph + branches sidebar), Java pack, git toolbar ([#101](https://github.com/kyle-ssg/kyde/issues/101)) ([3d48016](https://github.com/kyle-ssg/kyde/commit/3d48016cc2090a9a1f9080dda39d4c44b36bcea3))
* **keyboard:** navigate menus, submenus, trees, lists and popups by keyboard ([#100](https://github.com/kyle-ssg/kyde/issues/100)) ([f37a090](https://github.com/kyle-ssg/kyde/commit/f37a090d427ad8c76cecbb0fd0dacf4c264b6bf2))
* **keymap:** Push / Pull shortcuts that follow the keymap preset ([#105](https://github.com/kyle-ssg/kyde/issues/105)) ([5d61750](https://github.com/kyle-ssg/kyde/commit/5d6175066cf0d5e5a66cf27fe284518a6b313fee))
* **keymap:** shortcut editor in Settings → Keymap ([#106](https://github.com/kyle-ssg/kyde/issues/106)) ([4e81bd1](https://github.com/kyle-ssg/kyde/commit/4e81bd1246917bc42e1593aee5d8e2afe78119e4))
* **plugins:** offer a missing language pack over diffs, not just the editor ([#104](https://github.com/kyle-ssg/kyde/issues/104)) ([94abdbd](https://github.com/kyle-ssg/kyde/commit/94abdbd8ba82c2642eb43d398a3e8221a58eb116))


### Bug Fixes

* **ci:** get main green and roll up pending dependency bumps ([#95](https://github.com/kyle-ssg/kyde/issues/95)) ([c200a20](https://github.com/kyle-ssg/kyde/commit/c200a20bd12a51881d7b833f2e3d1e4197832821))
* **commit:** don't fail a commit when an untracked file is left unticked ([#96](https://github.com/kyle-ssg/kyde/issues/96)) ([104271f](https://github.com/kyle-ssg/kyde/commit/104271f4c22e8d0314003746744267c984c36c66))

## [2.6.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.5.0...kyde-v2.6.0) (2026-08-06)


### Features

* **changelog:** add a What's New window mirroring GitHub releases ([#74](https://github.com/kyle-ssg/kyde/issues/74)) ([f39bb3d](https://github.com/kyle-ssg/kyde/commit/f39bb3d96acb544b2d3749194c768dbcdfd3fa7e))
* **instance:** restrict Kyde to one instance, opening later launches as tabs ([#75](https://github.com/kyle-ssg/kyde/issues/75)) ([8de700e](https://github.com/kyle-ssg/kyde/commit/8de700e8d75d373ef078ab10ed99e0a3388dd52d))

## [2.5.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.4.0...kyde-v2.5.0) (2026-07-27)

All shipped together in [#69](https://github.com/kyle-ssg/kyde/pull/69) ([588e364](https://github.com/kyle-ssg/kyde/commit/588e364481bb7daf730e664583d26991122382a9)).

### Features

* Cut / Copy / Paste in the file tree — context menu + ⌘X/⌘C/⌘V, paste files copied in Finder, paste clipboard image data as an image file, and drag files in from Finder (target folder highlighted); grouped context menu with icons + dividers ([#67](https://github.com/kyle-ssg/kyde/issues/67))
* Drag files/folders onto folders in the tree, with the target folder highlighted ([#65](https://github.com/kyle-ssg/kyde/issues/65))
* Rename folders, not just files — repoints open tabs, selection and expansion across the moved subtree ([#68](https://github.com/kyle-ssg/kyde/issues/68))
* Non-git projects hide git actions and explain why, with an "Initialize Git Repository" button ([#66](https://github.com/kyle-ssg/kyde/issues/66))


### Refactor

* Extract reusable modal / dialog / empty-state UI helpers, removing duplicated hand-rolled `div` chains ([#64](https://github.com/kyle-ssg/kyde/issues/64))

## [2.4.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.3.0...kyde-v2.4.0) (2026-07-25)


### Features

* ⌘⌥←/⌘⌥→ back-forward file navigation ([#54](https://github.com/kyle-ssg/kyde/issues/54)) ([2800c38](https://github.com/kyle-ssg/kyde/commit/2800c38ece6d2bf45ca81c395e55e7063a5e368e))
* Local History — per-file snapshots independent of git ([#7](https://github.com/kyle-ssg/kyde/issues/7)) ([#63](https://github.com/kyle-ssg/kyde/issues/63)) ([972842b](https://github.com/kyle-ssg/kyde/commit/972842bd72076a3b873a0951b2178084ba1d8116))


### Bug Fixes

* rollback of added files, finder git-status colors, cmd+backspace delete ([#62](https://github.com/kyle-ssg/kyde/issues/62)) ([cf44415](https://github.com/kyle-ssg/kyde/commit/cf4441542d78b8f02e279f77c7450f9d04717d9f))

## [2.3.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.2.0...kyde-v2.3.0) (2026-07-17)


### Features

* ⌘-click imports to open the referenced file ([#52](https://github.com/kyle-ssg/kyde/issues/52)) ([9f818fa](https://github.com/kyle-ssg/kyde/commit/9f818fa8309dd5655332e20bbab31c82d1386649))
* compare two files with a bidirectional apply gutter ([#42](https://github.com/kyle-ssg/kyde/issues/42)) ([#51](https://github.com/kyle-ssg/kyde/issues/51)) ([3d4c867](https://github.com/kyle-ssg/kyde/commit/3d4c86768ed4f9b9dc460fe58be77b1c71067dfc))
* error highlighting — parse-error squiggles, on by default per pack ([#48](https://github.com/kyle-ssg/kyde/issues/48)) ([444dafd](https://github.com/kyle-ssg/kyde/commit/444dafdb1893d86034d75e517f905b49167da733))
* Sort Lines + Sort Object Keys ([#50](https://github.com/kyle-ssg/kyde/issues/50)) ([3605506](https://github.com/kyle-ssg/kyde/commit/36055069824be62535dfd9f12a7a0d853e2e5c84))

## [2.2.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.1.0...kyde-v2.2.0) (2026-07-17)


### Features

* merge view — branch merging with IntelliJ-style conflict resolution ([#46](https://github.com/kyle-ssg/kyde/issues/46)) ([f3ff30b](https://github.com/kyle-ssg/kyde/commit/f3ff30b7df1c010b5f2ecad8e632152262778334))


### Bug Fixes

* keep history/push diff panes open across window refocus ([#40](https://github.com/kyle-ssg/kyde/issues/40)) ([4d43bd6](https://github.com/kyle-ssg/kyde/commit/4d43bd62bca358c2ad92f308ce1ed756406bd3ae))

## [2.1.0](https://github.com/kyle-ssg/kyde/compare/kyde-v2.0.1...kyde-v2.1.0) (2026-07-14)


### Features

* partial commits — stage selected hunks from the diff gutter ([#33](https://github.com/kyle-ssg/kyde/issues/33)) ([ed00c1a](https://github.com/kyle-ssg/kyde/commit/ed00c1a27d886772875b335b53e45255599e644e))
* show +/− line stats — change-set total on the tree root, per-file in the diff pill ([#36](https://github.com/kyle-ssg/kyde/issues/36)) ([9a2d84b](https://github.com/kyle-ssg/kyde/commit/9a2d84bef075722c380d2d50cdc56bdb123a367e))
* worktree switcher — jump between worktrees without leaving the app ([#37](https://github.com/kyle-ssg/kyde/issues/37)) ([bcd3222](https://github.com/kyle-ssg/kyde/commit/bcd3222aaea425d36ce4efa117f0936f2dd111cf))

## [2.0.1](https://github.com/kyle-ssg/kyde/compare/kyde-v2.0.0...kyde-v2.0.1) (2026-07-13)


### Bug Fixes

* reliability, async git IO, god-struct decomposition, and CI/supply-chain fixes ([#31](https://github.com/kyle-ssg/kyde/issues/31)) ([060dcf9](https://github.com/kyle-ssg/kyde/commit/060dcf96dbb51b5ccfa14c8817676394f4e3c5ea))

## [2.0.0](https://github.com/kyle-ssg/kyde/compare/kyde-v1.3.0...kyde-v2.0.0) (2026-06-27)


### ⚠ BREAKING CHANGES

* cargo workspace and UX improvements ([#23](https://github.com/kyle-ssg/kyde/issues/23))

### Features

* cargo workspace and UX improvements ([#23](https://github.com/kyle-ssg/kyde/issues/23)) ([dafd38b](https://github.com/kyle-ssg/kyde/commit/dafd38b18b5894a3c1a2dd5e85e0b94f82a4253a))


### Bug Fixes

* unblock release-please on the Cargo workspace refactor ([416eb0f](https://github.com/kyle-ssg/kyde/commit/416eb0fe8af48b424accc4431a9e61c7cc05d4fe))

## [1.3.0](https://github.com/kyle-ssg/kyde/compare/kyde-v1.2.0...kyde-v1.3.0) (2026-06-24)


### Features

* jump between changes ([c7c8f48](https://github.com/kyle-ssg/kyde/commit/c7c8f482fc07908f3875088c02446470c66bf22e))
* terminal full screen ([1fc849a](https://github.com/kyle-ssg/kyde/commit/1fc849adf008b0493198cea3323208328b86d2a9))

## [1.2.0](https://github.com/kyle-ssg/kyde/compare/kyde-v1.1.0...kyde-v1.2.0) (2026-06-23)


### Features

* Intel Mac Support ([f5d895d](https://github.com/kyle-ssg/kyde/commit/f5d895d7ae769d1afa4903657b9d5acd56866fc4))
* Intel Mac Support ([4973472](https://github.com/kyle-ssg/kyde/commit/4973472a8e118ac749e43813942c566391ec2e5a))

## [1.1.0](https://github.com/kyle-ssg/kyde/compare/kyde-v1.0.0...kyde-v1.1.0) (2026-06-23)


### Features

* Add collapse feature to Git history ([1ecb421](https://github.com/kyle-ssg/kyde/commit/1ecb421fe6f8c54943915675564e5ebdd8cd2471))
* add commit and push tabs ([01202c4](https://github.com/kyle-ssg/kyde/commit/01202c4aff68ed1230984cc5880b3cbbd1e924e7))
* git history ([0b8d7b1](https://github.com/kyle-ssg/kyde/commit/0b8d7b1ab5326a4c2ab35eb4596d44d82ffd4e8e))
* Git History ([f691824](https://github.com/kyle-ssg/kyde/commit/f691824db4d326ca992f6d54952a0938ffbf8de3))
* self-update ([f394b5a](https://github.com/kyle-ssg/kyde/commit/f394b5ade94cb23829a47bbc7de0bc4aa7b3f078))
* self-update ([35d84e3](https://github.com/kyle-ssg/kyde/commit/35d84e30f738020dba436e104e117ad27370b553))


### Bug Fixes

* new branch upstrea ([c38f729](https://github.com/kyle-ssg/kyde/commit/c38f7294e7609207550c0e165d1d0282bc39aa9c))
* new branch upstrea ([53b84d6](https://github.com/kyle-ssg/kyde/commit/53b84d653b6c77e2158ccace532e4d31a6303c14))

## [1.0.0](https://github.com/kyle-ssg/kyde/compare/kyde-v0.3.0...kyde-v1.0.0) (2026-06-22)


### ⚠ BREAKING CHANGES

* terminal

### Features

* terminal ([ddffbf5](https://github.com/kyle-ssg/kyde/commit/ddffbf5b61d2857ce4c20cf69bf22f723e9c3de1))

## [0.3.0](https://github.com/kyle-ssg/kyde/compare/kyde-v0.2.0...kyde-v0.3.0) (2026-06-22)


### Features

* pull and fetch ([5f91eab](https://github.com/kyle-ssg/kyde/commit/5f91eabb22675386e5d35102600a86c34c89a1dd))


### Bug Fixes

* auto recover git pushes ([945d3eb](https://github.com/kyle-ssg/kyde/commit/945d3eb85200f4a7f7dd20f6552b400bde67356d))
* fmt ([47b61c7](https://github.com/kyle-ssg/kyde/commit/47b61c7e46b51924be83f04a48b11dc97a3afc37))
* prevent UI freeze on Find in Files in large repos ([07d1528](https://github.com/kyle-ssg/kyde/commit/07d1528d38f7691962439af81821cc7ce917beef))

## [0.2.0](https://github.com/kyle-ssg/kyde/compare/kyde-v0.1.1...kyde-v0.2.0) (2026-06-22)


### Features

* Kyde - A fast native commit and diff code editor ([28038bc](https://github.com/kyle-ssg/kyde/commit/28038bccef15f3d81da4b5e18e26d5c8f5fa2e89))
* release signing secret names and notarize via Apple ID ([9b402ee](https://github.com/kyle-ssg/kyde/commit/9b402eee75c4e782637a8ee49cbc4dd0028628c7))

## [0.1.1](https://github.com/kyle-ssg/kyde/compare/kyde-v0.1.0...kyde-v0.1.1) (2026-06-21)


### Bug Fixes

* **macos:** ad-hoc codesign the .app bundle so downloads aren't flagged damaged ([530dd4a](https://github.com/kyle-ssg/kyde/commit/530dd4acb0b548fceff6178bb27b392b468041c4))

## 0.1.0 (2026-06-21)


### Features

* Kyde A fast native commit and diff code editor ([786e740](https://github.com/kyle-ssg/kyde/commit/786e740eec57949476a511d8e52aae7ec8854665))


### Bug Fixes

* cargo lock ([e1fb959](https://github.com/kyle-ssg/kyde/commit/e1fb95903cd36de597ebe3080984694ed3314097))
* clippy ([6125697](https://github.com/kyle-ssg/kyde/commit/6125697f2d93182e76a102675c589dbbc6d834d2))

## [0.1.0] - 2026-06-20

Initial release.

[0.1.0]: https://github.com/kyle-ssg/Kyde/releases/tag/v0.1.0
