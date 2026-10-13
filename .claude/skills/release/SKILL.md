---
name: release
description: How Virtual Audio Mixer's versions and GitHub releases work (release-please, Conventional Commits, the Windows exe), and how to cut, check or fix a release. Use when asked to release, bump the version, or why a release or changelog looks wrong.
---

# Releases

Releases are automated by release-please (`.github/workflows/release.yml`,
`release-please-config.json`, `.release-please-manifest.json`).

## How a release happens

1. Every push to `main` makes release-please update one open **release PR**
   ("chore(main): release x.y.z"). It bumps `version` in `Cargo.toml` (and
   `Cargo.lock`) and adds the commits since the last release to
   `CHANGELOG.md`.
2. **Merging that PR is the release.** release-please tags `vx.y.z` and
   makes a draft GitHub Release with that changelog section. The workflow's
   Windows job builds `virtual-audio-mixer.exe`, and the publish job attaches it
   and publishes the release, so a published release always has the exe.
3. `releases/latest/download/virtual-audio-mixer.exe` always points at the newest,
   and the README links there.

To cut a release: check the release PR's changelog reads well, and merge it
(rebase or merge both work). Then check the Release workflow run went green
and the release has `virtual-audio-mixer.exe` attached.

## Versions

- SemVer from the commit types on `main`: `fix` and `perf` → patch, `feat` →
  minor, `!` or a `BREAKING CHANGE:` footer → major (minor while below 1.0).
  `docs`, `style`, `refactor`, `test` and `chore` don't trigger a release on
  their own.
- So commit messages matter: a user-visible change must be `feat` or `fix`, with
  a subject that reads well in a changelog.
- The first release is 0.1.0: the manifest starts at 0.0.0 and there is no
  `CHANGELOG.md` until that release PR makes it. Until it's merged, the
  README's download link has nothing to point at.
- To force a version, push a commit to `main` with a `Release-As: 1.0.0`
  footer.
- The app reports `env!("CARGO_PKG_VERSION")` (`crate::VERSION`): in the tray
  menu, at the bottom of Help, and as `version` in `/api/state`. The
  "as Administrator" copy in Program Files records its version in
  `version.txt`, and Setup check offers to update it when it's older.
  Opening a newer exe while an older copy runs offers to close the old one.

## Fixing things

- **No release PR appears**: the repository must allow it under
  **Settings → Actions → General → Workflow permissions** ("Allow GitHub Actions
  to create and approve pull requests"). Check the Release workflow's log,
  including for a permission error on the `autorelease:` labels.
- **Wrong changelog entry**: edit the release PR's `CHANGELOG.md` before merging
  (release-please keeps manual edits to its PR until the next push to `main`),
  or fix it on the GitHub Release afterwards.
- **The release stayed a draft**: the build or publish job failed. Re-run
  the failed jobs in that workflow run (the build checks out the tag), or
  upload by hand with `gh release upload vx.y.z virtual-audio-mixer.exe --clobber`
  and publish with `gh release edit vx.y.z --draft=false --latest`.
- Never hand-edit `version` in `Cargo.toml` or the manifest outside a release
  PR: release-please owns them.
- Release PRs opened by the workflow's token don't trigger the CI workflow;
  `main` was already green, and the release PR only changes the version and
  changelog. If branch protection ever requires CI checks, the release PR
  can't merge until release-please runs with a personal access token or a
  GitHub App token instead.
