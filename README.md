# Virtual Audio Mixer

Mix your real microphone with audio from other apps (Chrome, a soundboard, a
music player, …) and send the result to a virtual microphone. Discord, OBS,
Teams or any other app then hears all of it through that one mic.

```
 Microphone ─┐
 chrome.exe ─┼─► mixer (volume, mute, push-to-talk) ─► CABLE Input ══► CABLE Output ─► Discord
 Soundpad   ─┘                                        (virtual cable)    (virtual mic)
```

It runs as a small tray app and is controlled from a web page served on
`http://127.0.0.1:47800`. The page is only reachable from your own PC.

## Features

- **Per-app capture**: pick a running app and capture everything it plays,
  including its child processes (e.g. all of Chrome's tabs). There are no extra
  virtual devices to set up, and the app still plays to your speakers as normal.
  Apps are matched by executable name, so capture resumes when the app restarts.
- **Any input device** as a source, or "default microphone" to follow the
  Windows default.
- Volume (−60 to +12 dB), mute and a live level meter for every source and the
  master output. A soft limiter on the master stops the summed mix from
  clipping harshly.
- **Global hotkeys**: toggle mute, push-to-talk or push-to-mute on any channel.
  Keyboard combos and mouse side buttons (Mouse4/Mouse5) both work. Hotkeys
  don't swallow the key, so it still reaches the game you're in.
- Start with Windows (runs in the background, tray icon only).
- Settings are saved to `%APPDATA%\VirtualAudioMixer\config.json`.

## Requirements

- Windows 10 version 2004 or newer, or Windows 11. Per-app capture uses the
  process loopback API added in 2004.
- A virtual audio cable to act as the microphone. The free
  [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) works well, and the
  mixer auto-selects it on first run. Any other virtual cable works too:
  pick its *playback* side as the output.

## Setup

1. Install VB-Audio Virtual Cable and reboot.
2. Run `virtual-audio.exe`. The mixer opens in your browser, and a tray icon
   appears (left-click it to reopen the page, right-click for Quit).
3. Under **Output**, select **CABLE Input (VB-Audio Virtual Cable)**.
4. Your default microphone is added automatically. Click **+ Add source** to
   add apps (e.g. `chrome.exe`) or other input devices.
5. In Discord: **Settings → Voice & Video → Input Device → CABLE Output
   (VB-Audio Virtual Cable)**. Turn off Discord's own noise suppression or
   "Krisp" if it cuts out your soundboard.

> Tip: if you capture an app, don't also route the cable back to your
> speakers, or you'll hear that app twice.

## Building

```sh
cargo build --release            # on Windows → target/release/virtual-audio.exe
```

CI builds the Windows release `.exe` on every push. Download it from the
workflow run's artifacts.

On Linux and macOS the same binary builds with a **demo backend**: fake
devices and apps that generate tones. You can develop and test the engine and
web UI anywhere with `cargo run`, then open `http://127.0.0.1:47800`.

Cross-compiling for Windows from Linux also works:

```sh
rustup target add x86_64-pc-windows-gnu   # plus the mingw-w64 linker
cargo build --target x86_64-pc-windows-gnu
```

## How it works

| Piece | Where |
| --- | --- |
| Mixing core: per-source SPSC ring buffers, gain ramps, meters, soft limiter | `src/mixer.rs` |
| Engine: config, capture/render threads, supervisor that restarts captures as apps/devices come and go | `src/engine.rs` |
| WASAPI backend: shared-mode event-driven streams, process loopback, device and session enumeration | `src/backend/wasapi.rs` |
| Demo backend for non-Windows hosts | `src/backend/dummy.rs` |
| Global hotkeys (polled, non-blocking) | `src/hotkeys.rs` |
| HTTP/WebSocket API (axum) and embedded UI | `src/server.rs`, `web/` |
| Tray icon, autostart, browser launch | `src/tray.rs`, `src/platform.rs` |

All audio runs at 48 kHz stereo float. Windows converts each device's native
format (`AUTOCONVERTPCM`). Each source keeps about 20 ms of buffer, so total
added latency is roughly 30–50 ms.

The web UI is plain HTML/CSS/JS with no build step. It is compiled into the
executable with `include_str!`, so the release is a single `.exe`.

### API

| Method | Path | Body |
| --- | --- | --- |
| GET | `/api/state` | |
| GET | `/api/devices` | |
| GET | `/api/apps` | |
| POST | `/api/sources` | `{"kind":"app","exe":"chrome.exe"}` or `{"kind":"device","device":null}` |
| PATCH | `/api/channels/{id or "master"}` | `{"gain_db":-6,"muted":true,"name":"…"}` |
| DELETE | `/api/channels/{id}` | |
| PUT | `/api/output` | `{"device":{"id":"…","name":"…"}}` or `{"device":null}` |
| PUT | `/api/hotkeys` | `[{"keys":"Ctrl+Shift+M","action":"push_to_talk","target":"…"}]` |
| PUT | `/api/settings` | `{"autostart":true}` |
| GET | `/ws` | WebSocket: pushes `state` on change and `meters` about 30 times a second |

Requests whose `Host` or `Origin` isn't `127.0.0.1`/`localhost` on our port are
rejected. This stops other websites from controlling the mixer.

## Roadmap

- Our own virtual microphone driver, so the device can be called "Virtual
  Mixer Mic" and no third-party cable is needed. The output is just a device
  setting, so this slots in without engine changes. It needs Microsoft driver
  signing (or test-signing mode) to install.
- Built-in soundboard (play files from the UI or hotkeys).
- Monitor the mix on headphones.
- Follow changes of the Windows default microphone live (currently picked up
  only when the capture restarts).
- Adaptive resampling to absorb clock drift between devices. Today a source
  that drifts too far is trimmed, causing a tiny glitch every few minutes at
  worst.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
