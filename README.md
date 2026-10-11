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
- **Any input device** as a source, or "default microphone", which follows the
  Windows default live. Sources that would capture the recording end of the
  output cable (a feedback loop) are refused with an error.
- Volume (−60 to +12 dB, drag the slider or click the value to type one),
  mute and a live level meter for every source and the master output. Drag
  cards by their handle (or focus it and use the arrow keys) to reorder them.
- **Listen**: hear the final mix on your Windows default output (headphones)
  to check what others hear. It switches off by itself when you close the page. A soft limiter on the master stops the summed mix from
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
   appears. Left-click it to reopen the page; right-click it for **Open
   mixer**, **Open log file** and **Quit**.
3. Check that **Output** is **CABLE Input (VB-Audio Virtual Cable)**. It is
   selected automatically on first run if the cable is installed.
4. Your default microphone is added automatically. Click **+ Add source** to
   add apps (e.g. `chrome.exe`) or other input devices.
5. In Discord: **Settings → Voice & Video → Input Device → CABLE Output
   (VB-Audio Virtual Cable)**. Turn off Discord's own noise suppression or
   "Krisp" if it cuts out your soundboard.

> Tip: if you capture an app, don't also route the cable back to your
> speakers, or you'll hear that app twice.

## Troubleshooting

Everything the app knows is in `%APPDATA%\VirtualAudioMixer\` (paste that
into Explorer's address bar, or use the tray's **Open log file**):

| File | What it is |
| --- | --- |
| `config.json` | Your sources, output, hotkeys and volumes. |
| `config.<timestamp>.json.bad` | A `config.json` that could not be read. It is moved aside and the app starts with defaults, so you can repair it. If it can't be moved aside, the app doesn't start rather than overwrite it. |
| `log.txt` | This run's log: every source and output it opens, errors, and crashes. |
| `log.previous.txt` | The previous run's log. If the app vanished, look here. |

The app understands two command-line flags:

- `--background`: start without opening the browser. Start with Windows uses
  this.
- `--verbose`: log extra debug detail.

Logs go to these files only in Windows release builds. Debug builds and demo
mode print them to the terminal instead.

## Building

```sh
cargo build --release            # on Windows → target/release/virtual-audio.exe
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
format (`AUTOCONVERTPCM`). Each source keeps about 30 ms of buffer, so total
added latency is roughly 40–60 ms. Sources and the output run on separate
clocks, so the mixer corrects drift continuously: when a source's buffer
strays from its target it takes one frame more or less per 10 ms block and
stretches the block to fit, a pitch change of about 0.2% for that block.

The web UI is plain HTML/CSS/JS with no build step. It is compiled into the
executable with `include_str!`, so the release is a single `.exe`.

### API

| Method | Path | Body |
| --- | --- | --- |
| GET | `/api/state` | |
| GET | `/api/devices` | |
| GET | `/api/apps` | |
| POST | `/api/sources` | `{"kind":"app","exe":"chrome.exe"}` or `{"kind":"device","device":null}`, plus an optional `"name"` |
| PUT | `/api/sources/order` | `{"ids":["…","…"]}` (every source id, in the new order) |
| PATCH | `/api/channels/{id or "master"}` | `{"gain_db":-6,"muted":true,"name":"…"}` (`name` applies to sources only) |
| DELETE | `/api/channels/{id}` | |
| PUT | `/api/output` | `{"device":{"id":"…","name":"…"}}` or `{"device":null}` |
| PUT | `/api/hotkeys` | `[{"keys":"Ctrl+Shift+M","action":"push_to_talk","target":"…"}]` |
| PUT | `/api/settings` | `{"autostart":true}` |
| POST | `/api/quit` | Stops the app. |
| GET | `/ws` | WebSocket: pushes `state` on change and `meters` about 30 times a second. Send `{"type":"listen","on":true}` to monitor the mix; it stops when the socket closes. |

Requests whose `Host` or `Origin` isn't `127.0.0.1`/`localhost` on our port are
rejected, and the UI can't be embedded in another site's frame. This stops
other websites from controlling the mixer. Invalid input gets a 400 with a
message; names are limited to 100 characters, and sources and hotkeys to 64
each.

## Roadmap

- Our own virtual microphone driver, so the device can be called "Virtual
  Mixer Mic" and no third-party cable is needed. The output is just a device
  setting, so this slots in without engine changes. It needs Microsoft driver
  signing (or test-signing mode) to install.
- Built-in soundboard (play files from the UI or hotkeys).
- Monitor the mix on headphones.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
