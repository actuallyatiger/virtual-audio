# Development

## Building

```sh
cargo build --release            # on Windows → target/release/virtual-audio-mixer.exe
```

CI builds the Windows release `.exe` for pull requests and pushes to `main`.
Download it from the workflow run's artifacts. Building needs Rust 1.88 or
newer.

On Linux and macOS the same binary builds with a **demo backend**: fake
devices and apps that generate tones. You can develop and test the engine and
web UI anywhere with `cargo run`, then open `http://127.0.0.1:47800`.

Cross-compiling for Windows from Linux also works:

```sh
rustup target add x86_64-pc-windows-gnu   # plus the mingw-w64 linker
cargo build --target x86_64-pc-windows-gnu
```

## Checks

Run these before pushing. CI runs fmt, clippy and the tests on Windows and
Linux; the Windows-target clippy is the Linux stand-in for Windows' own
clippy run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings  # the Windows-only code
cargo test
```

`cargo test -- --ignored` also runs the tests that need the internet (the
update check against GitHub).

## Releases

Versions follow [SemVer](https://semver.org) and come from the
[Conventional Commits](https://www.conventionalcommits.org) on `main`, via
[release-please](https://github.com/googleapis/release-please)
(`.github/workflows/release.yml`):

1. Every push to `main` updates a release PR that bumps the version in
   `Cargo.toml` and adds the new `feat`, `fix` and other changes to
   `CHANGELOG.md`. A `feat` raises the minor version (0.1 → 0.2) and a
   `fix` the patch; breaking changes raise the major once past 1.0.
2. Merging that PR tags the version (`v0.2.0`) and drafts a GitHub Release
   with the changelog. The Windows `virtual-audio-mixer.exe` is built,
   signed with minisign (the `MINISIGN_SECRET_KEY` secret, plus
   `MINISIGN_PASSWORD` if the key has one; the signature is appended to the
   exe) and attached, and then the release is published. Installed copies
   update themselves from it (see [architecture](architecture.md#updates)).

The app shows its version in the tray menu and at the bottom of Help, and
`GET /api/state` reports it as `version`. For release-please to open its PR,
the repository's **Settings → Actions → General → Workflow permissions** must
allow GitHub Actions to create pull requests.

## Test builds

Only the release workflow builds with `VAM_RELEASE` set. Any other build is a
test build: its version reads like `0.2.1-dev.010e2aa` (the commit, from
`VAM_COMMIT`, which CI sets) and it never updates itself.

To try a PR on Windows, download `virtual-audio-mixer-windows-x64` from the
**Windows release build** job's artifacts (Checks tab, needs a GitHub login)
and open it:

- If the mixer is running as another build, it asks to replace it.
- With Start with Windows on, it installs itself over the installed copy and
  runs from there (as Administrator, Setup check offers to install it in
  Program Files). Whichever build was opened last is the one installed.
- To go back, open a release: it replaces the test build the same way, and
  updates itself from then on.

## Testing the UI

The demo backend serves the real UI, so it can be driven with Playwright in a
headless browser: see `.claude/skills/run/SKILL.md`. Check new UI at 320 px
and 390 px wide, in light and dark.
