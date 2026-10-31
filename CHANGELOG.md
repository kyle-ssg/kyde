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
