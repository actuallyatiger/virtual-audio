# AGENTS.md

Guidance for anyone changing this repository, human or AI. User-facing
documentation is in the [README](README.md); this file is about working on the
code.

## What this is

Virtual Audio Mixer: a Windows tray app in Rust that mixes a microphone,
per-app audio (WASAPI process loopback), a soundboard and a music playlist into
a virtual cable (e.g. VB-CABLE) that Discord and other apps use as their mic. A
local web UI (axum, plain HTML/CSS/JS in `web/`, embedded with `include_str!`)
talks to it over JSON and a WebSocket. See
[docs/architecture.md](docs/architecture.md) for how the pieces fit and
[docs/api.md](docs/api.md) for the API.

| Area | Where |
| --- | --- |
| Real-time mixing core (unit tested) | `src/mixer.rs` |
| Engine: config, supervisor, capture/render/monitor threads | `src/engine.rs` |
| Mic processing (RNNoise, automatic level, noise gate) | `src/dsp.rs` |
| Soundboard, music, decoding | `src/soundboard.rs`, `src/music.rs`, `src/decode.rs` |
| Windows audio (unsafe COM) / demo backend | `src/backend/wasapi.rs` / `src/backend/dummy.rs` |
| HTTP API and WebSocket | `src/server.rs` |
| Hotkeys, tray, autostart and elevation | `src/hotkeys.rs`, `src/tray.rs`, `src/platform.rs` |
| Updates (signed releases) | `src/update.rs` |
| Config file (`%APPDATA%\VirtualAudioMixer\config.json`) | `src/config.rs` |
| Web UI | `web/index.html`, `web/app.js`, `web/style.css` |

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings  # the Windows-only code
cargo test
```

All four must pass before pushing. CI runs fmt, clippy and the tests on
Windows and Linux (Windows' own clippy covers the Windows-only code there) and
builds the release `.exe`. On Linux, `cargo run` starts the demo backend (fake
devices and apps that play tones) with the real engine and UI: see the `run`
skill (`.claude/skills/run/SKILL.md`) for running it in isolation and driving
the UI in a headless browser.

## Code

- Match the surrounding code: its naming, comment density and idioms. Comments
  say why, not what.
- **Real-time paths** (`Mixer::process`, capture threads, `dsp::Processor`, the
  soundboard and music streams) must not allocate, block, or wait on a lock
  another thread can hold for long: pre-size buffers, use `rtrb` rings and
  atomics, and `try_lock` taps (skipping a block beats waiting). When holding
  two locks, take the engine's `inner` before the mixer.
- The one exception: echo cancellation's AEC3 (the sonora crate) allocates
  small buffers on the mic's capture thread while it's on (`src/dsp.rs`).
  Don't add others.
- Avoid clicks: every start, stop and jump ramps the gain (a few ms) instead
  of cutting mid-waveform.
- **No breaking changes without a major version** (from 1.0): every
  `config.json` an earlier version saved must load (`src/testdata/config/`
  has one from each release, and the test loading them must keep passing),
  and the API ([docs/api.md](docs/api.md)) only grows. A change that can't
  avoid breaking either is a `feat!` with a `BREAKING CHANGE:` footer.
  When releasing, add the new version's config there (the `release` skill).
- New config fields go in structs with `#[serde(default)]`, so older
  `config.json` files still load; an enum that may grow falls back to its
  default rather than making the file unreadable. Invalid API input gets a
  plain message: 400 from the handlers (a body that doesn't fit the types
  gets axum's 422). Anything stored is size-limited.
- `unsafe` is for Win32 calls (mostly `src/backend/wasapi.rs` and
  `src/platform.rs`, plus a few in `src/hotkeys.rs` and `src/tray.rs`), kept
  in small blocks. Check windows-rs semantics against the crate source in the
  cargo registry, not from memory.
- Tests sit next to the code (`#[cfg(test)]`). A bug fix comes with a test that
  fails without it.
- British spelling in docs and UI text (colour, behaviour).

## Web UI

- Icons: `<button class="icon" data-icon="…">` with SVGs from `ICONS` in
  `app.js`, drawn by `drawIcons()` (`el()` draws them for created elements).
  Text buttons with an icon use `button.with-icon`. Also `button.small`,
  `button.bad` (red), `.ghost`, `.primary`.
- Explanations go in an ⓘ info pop next to a heading, with the details in
  Help (`#help-…` sections); problems the user can fix go in Setup check
  (`setupChecks()` in `app.js`), with a fix button where possible.
- Dialogs close on a backdrop click and Esc; focus returns sensibly.
- Colours are CSS variables on `:root`. The light theme exists twice in
  `style.css` (for the system setting and for the theme button), and the two
  blocks must stay the same.
- Check layout at 320 px and 390 px wide, in light and dark, and keyboard
  access (labels, `aria-*`, tab order).
- For visual changes, take screenshots and show them before committing
  (`.claude/skills/run/SKILL.md`).
- User-facing text is plain and short, and leaves out implementation
  details users don't need.

## Git and pull requests

- [Conventional Commits](https://www.conventionalcommits.org): `feat`, `fix`,
  `docs`, `style`, `refactor`, `test`, `chore`, with a scope where useful
  (`feat(music): …`). The body says why, and `Closes #N` links the issue.
  These drive releases and the changelog (see the `release` skill).
- PRs are merged by **rebase**, not squash, so each commit should stand on its
  own. Squash fix-ups of unreleased work on your own branch before merging.
- Several related issues go in **stacked PRs** (each based on the previous
  branch), merged bottom-up.
- Before merging, run the `adversarial-review` skill
  (`.claude/skills/adversarial-review/SKILL.md`) and fix what survives.
- Windows-only behaviour (UAC, file pickers, real devices) can't run in CI or
  Linux sandboxes: say in the PR what needs testing on Windows.
- How to handle CI and review comments on PRs: the `steward` skill.
