# Releasing Kyde

Releases are automated by [release-please](https://github.com/googleapis/release-please)
(`.github/workflows/release.yml`). You don't bump versions, edit the changelog, or
push tags by hand — you just write good commit messages and merge a PR.

Kyde follows [Semantic Versioning](https://semver.org/) — `MAJOR.MINOR.PATCH`.

## How it works

1. You merge normal PRs into `main` using **Conventional Commits** (below).
2. release-please watches `main` and keeps a standing **"release PR"** open that
   bumps `Cargo.toml` + `Cargo.lock`, updates `CHANGELOG.md`, and computes the next
   version from the commits since the last release.
3. When you're ready to ship, **merge the release PR**. release-please then creates
   the `vX.Y.Z` git tag and the GitHub Release.
4. In the same workflow run, the `build` job packages the macOS artifacts — an
   Apple-Silicon `kyde-macos.zip` and an Intel `kyde-macos-x86_64.zip`, each with a
   `.sha256` — Developer ID signs, notarizes and staples them (a no-op that leaves the
   ad-hoc signature if the signing secrets aren't set), and attaches them to the release.
   Linux and Windows packaging exists in `scripts/`, but its matrix entries and package
   steps are **commented out** in `release.yml`; uncomment both to revive a platform.

That's it. Cutting a release = merging one PR.

## Conventional Commits (this is the only discipline required)

The commit messages on `main` decide the version bump:

| Commit prefix | Bump | Example |
|---|---|---|
| `fix:` | PATCH | `fix: stop crash on empty diff` |
| `feat:` | MINOR | `feat: add Go language pack` |
| `feat!:` / `fix!:` or a `BREAKING CHANGE:` footer | MAJOR | `feat!: change keymap.json schema` |
| `chore:`, `docs:`, `refactor:`, `test:`, `ci:` | none | housekeeping, no release |

A breaking change is anything that breaks a config format (theme/keymap/plugins/
projects/history JSON), the `ky` CLI, or removes a feature. Kyde is past `1.0`, so a
`feat!:` / `BREAKING CHANGE:` really does mean the next MAJOR — reach for it only when a
config format or the `ky` CLI genuinely changes shape.

release-please owns the version in two places, kept in lockstep: the root `Cargo.toml`
`[package].version` (its `rust` strategy needs a literal string there) and
`[workspace.package].version`, which the member crates inherit — the latter updated via
the `extra-files` entry in `release-please-config.json`. The version is mirrored into the
binary through `CARGO_PKG_VERSION`, the macOS `Info.plist`, and the Windows `.exe`
metadata. The git tag always matches it.

## After a release

Don't move or delete a published tag — they're immutable once people may have
pulled them. A broken release is fixed by landing a `fix:` commit and shipping the
next PATCH (release-please will offer it in the next release PR).

## Manual fallback

If you ever need to release without release-please (e.g. the action is down), note
that pushing a tag by hand will **not** trigger the build — the `build` job in
`release.yml` is gated on release-please's `release_created` output, and tags created
with the default token don't fire it. So a manual release means doing the packaging
yourself too: bump both `version` fields in `Cargo.toml` (and `Cargo.lock`), update
`CHANGELOG.md`, push a `vX.Y.Z` tag and create the GitHub Release yourself, then build
the artifacts locally (`TARGET=aarch64-apple-darwin ./scripts/bundle-macos.sh`, then
again for `x86_64-apple-darwin`, zipping each `dist/Kyde.app` as `kyde-macos.zip` /
`kyde-macos-x86_64.zip`) and `gh release upload` each one. Locally built apps are
ad-hoc-signed, not notarized. The automated path is strongly preferred.
