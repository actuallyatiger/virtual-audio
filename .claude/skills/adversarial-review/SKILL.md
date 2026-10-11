---
name: adversarial-review
description: Adversarial multi-agent code review of a PR or branch diff. Use before merging, or when asked to "adversarially review", "red-team the code", or "double-check a PR". Fans out reviewer subagents (one per lens - correctness, unsafe/FFI, real-time audio and concurrency, security, web UI, style and docs) that try to break the change, then a skeptical verification pass that tries to refute each finding. Only findings that survive verification get fixed.
---

# Adversarial review

The goal is to find real defects before merge, not to produce a long list.
Reviewers try to **break** the change. Verifiers then try to **refute** each
finding. Only findings with a concrete, reproducible failure path survive.

## 1. Scope the diff

- Default target: the current branch against its base, e.g.
  `git diff --stat origin/main...HEAD`. For a PR, use its base branch.
- List the changed files and note which lenses below apply. Skip a lens that
  has nothing to look at (e.g. no `web/` changes means no web UI reviewer).
- Run the repo's fast checks first, so reviewers don't rediscover what tooling
  already catches:
  `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`,
  plus `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings`
  for the Windows-only code.

## 2. Fan out reviewers (in parallel)

Spawn one subagent per applicable lens with the Agent tool, all in a single
message so they run concurrently. Use `model: "sonnet"`. Every reviewer
prompt must be self-contained and include:

- the repo path, branch, and base (e.g. `origin/main...HEAD`);
- **read-only**: do not edit, commit, push, or comment anywhere;
- what the project is (see "Project context" below);
- its lens (below) and an explicit "out of scope" list (the other lenses),
  so reviewers don't pad their reports with each other's findings;
- the required output format (below);
- "Report at most 8 findings, most severe first. Zero findings is a valid,
  good answer. Do not report style preferences as bugs, or speculation
  without a concrete failure path."

### Lenses

1. **Correctness**: logic errors, wrong conditions, off-by-one, unhandled
   error paths, state that can get out of sync (config vs runtime, UI vs
   server), restart/retry loops that never recover or spin.
2. **Unsafe and FFI**: every `unsafe` block and Win32/COM call. Ownership and
   drop semantics of windows-rs types (e.g. `PROPVARIANT` has `Drop`), buffer
   sizes against formats, pointer lifetimes across async callbacks, handle
   leaks, and COM apartment and threading rules.
3. **Real-time audio and concurrency**: anything that can block, allocate or
   lock on the audio threads; lock ordering and deadlocks; races between the
   supervisor, capture, render, monitor and hotkey threads; ring-buffer
   misuse; clock drift and underrun behaviour; clicks and pops.
4. **Security**: the localhost HTTP/WebSocket API (Host/Origin checks,
   DNS rebinding, CSRF), input validation, path handling, registry writes,
   and anything a malicious web page or local process could abuse.
5. **Web UI**: JS logic bugs, event-handler edge cases (focus, keyboard,
   drag and drop), state sync with the server, accessibility (labels, roles,
   keyboard reachability), and layout breaking at narrow widths.
6. **Style, docs and consistency**: deviations from the surrounding code's
   idioms and naming, dead code, misleading comments, README and API docs
   that no longer match the code, Conventional Commit messages, and missing
   tests for changed behaviour.

### Required finding format

```
### <short title>
- Severity: critical | high | medium | low
- Confidence: high | medium | low
- Location: path/to/file.rs:LINE (and others)
- Failure scenario: concrete inputs/state -> wrong behaviour/crash
- Evidence: the code (quote the lines) and why it fails
- Suggested fix: minimal change
```

## 3. Verify adversarially

Collect all findings and de-duplicate them. Then spawn verifier subagents
(`model: "sonnet"`, read-only, batched about 5 findings per verifier). Tell
each verifier to **assume each finding is wrong** and to try to refute it:

- Read the actual code, and trace whether the failure path is reachable from
  a real caller or input.
- Check library and OS semantics against documentation or the crate source
  under `~/.cargo/registry/src/` instead of from memory (e.g. what
  `std::fs::rename` does on Windows).
- Where possible, prove it: write a scratch test, or run the demo backend
  (`cargo run` on Linux serves the UI at http://127.0.0.1:47800) and
  reproduce it. Never leave scratch files in the repo.
- Verdict per finding: **CONFIRMED** (with the reproduction or exact trace),
  **PLAUSIBLE** (real path, but it can't be demonstrated here, e.g. it needs
  Windows hardware) or **REFUTED** (with the reason).

## 4. Triage and fix

- Fix CONFIRMED findings. Fix PLAUSIBLE ones when the fix is small and in
  proportion to the risk. Note REFUTED ones with their reason, so they aren't
  re-raised.
- Keep each fix minimal, add a test when the behaviour is testable, and run
  the fast checks from step 1 before committing. Use one Conventional Commit
  per logical fix.
- Report back a table: finding, lens, verdict, action (fixed in `<sha>` /
  declined because …).

## Project context (give this to every subagent)

Virtual Audio Mixer: a Windows tray app in Rust that mixes a microphone and
per-app audio (WASAPI process loopback) into a virtual cable (e.g. VB-CABLE)
that Discord uses as a mic. A local web UI (axum, plain HTML/JS in `web/`,
embedded via `include_str!`) talks to it over JSON and a WebSocket.

- `src/mixer.rs`: lock-free real-time mixing core (unit tested).
- `src/engine.rs`: config, supervisor, and the capture/render/monitor threads.
- `src/backend/wasapi.rs`: Windows audio (unsafe COM).
- `src/backend/dummy.rs`: demo backend for non-Windows hosts.
- `src/hotkeys.rs`, `src/server.rs`, `src/tray.rs`, `src/platform.rs`,
  `src/config.rs`, `src/main.rs`.

Windows-only code can't run in the review environment. Reviewers must reason
about it from the code and from the windows-rs sources in the cargo registry.
