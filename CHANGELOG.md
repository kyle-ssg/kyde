# Changelog

All notable changes to Kyde are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
See [RELEASING.md](RELEASING.md) for how releases are cut.

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
