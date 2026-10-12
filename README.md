# Virtual Audio Mixer

Mix your real microphone with audio from other apps (Chrome, a music player,
…) and a built-in soundboard, and send the result to a virtual microphone.
Discord, OBS, Teams or any other app then hears all of it through that one mic.

```
 Microphone ─┐
 chrome.exe ─┼─► mixer (volume, mute, push-to-talk) ─► CABLE Input ══► CABLE Output ─► Discord
 Soundboard ─┘                                        (virtual cable)    (virtual mic)
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
- **Mic processing** for input devices (the ⚙ on a mic's card):
  - *Noise suppression* (RNNoise) removes steady background noise such as
    fans, hum and keyboard clatter. With it on, you can turn off Discord's own
    noise suppression, which tends to cut out the soundboard and music.
  - A *noise gate* silences the mic while you aren't talking. *Automatic*
    mode (the default) opens when RNNoise's voice detector hears speech
    that's louder than the room's background noise, like Discord's automatic
    input sensitivity. *Manual* mode opens above a level you set against a
    live meter. Either way it stays open for 200 ms after you stop talking,
    so word endings aren't clipped, and it only gates the mic: the
    soundboard and apps always get through.

  Either one adds up to 10 ms of delay and makes the mic mono.
- **Soundboard**: add MP3, WAV, OGG, FLAC or M4A files (up to 60 seconds and
  20 MB each, 64 sounds) with **+ Add sounds** or by dropping them on the
  soundboard. Click a sound to play it into the mix; clicking it again
  restarts it. Each sound has its own volume, and the soundboard is a mixer
  channel with its own volume, mute and hotkeys.
- **Hear sounds** (on by default): the soundboard also plays on your Windows
  default output, at its own volume, so you hear your own sounds without
  hearing your own voice. It pauses while Listen is on, since Listen
  already includes the soundboard.
- Volume (−60 to +12 dB, drag the slider or click the value to type one),
  mute and a live level meter for every source and the master output. A soft
  limiter on the master stops the summed mix from clipping harshly. Drag
  cards by their handle (or focus it and use the arrow keys) to reorder them.
- **Listen**: hear the final mix on your Windows default output (use
  headphones) to check what others hear. It's shared by every open mixer
  page and switches off when the last page that turned it on is closed. It
  follows changes of the default output, and is refused (with a message) if
  the default output is the device the mix already goes to. Expect about
  100 ms of delay on your own voice.
- **Global hotkeys**: toggle mute, push-to-talk or push-to-mute on any
  channel, play a sound, or stop all sounds.
  Keyboard combos and mouse side buttons (Mouse4/Mouse5) both work. Hotkeys
  don't swallow the key, so it still reaches the game you're in.
- **Setup check** and **Help** in the top bar, and an ⓘ next to each
  section explaining it.
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
5. In the app you talk in, choose **CABLE Output (VB-Audio Virtual Cable)**
   as the microphone, and turn off its own automatic sensitivity and noise
   suppression, which tend to cut out the soundboard and music. The mixer's
   **Help** has the exact settings for Discord, OBS, Teams and Zoom.
6. **Setup check** in the top bar confirms all of this (the cable, the
   output, your Windows default devices, and that your mic picks up sound)
   and offers one-click fixes.

> Tip: if you capture an app, don't also route the cable back to your
> speakers, or you'll hear that app twice.

## Troubleshooting

Everything the app knows is in `%APPDATA%\VirtualAudioMixer\` (paste that
into Explorer's address bar, or use the tray's **Open log file**):

| File | What it is |
| --- | --- |
| `config.json` | Your sources, output, hotkeys, sounds and volumes. |
| `sounds\` | Copies of the files you added to the soundboard. |
| `config.<timestamp>.json.bad` | A `config.json` that could not be read. It is moved aside and the app starts with defaults, so you can repair it. If it can't be moved aside, the app doesn't start rather than overwrite it. |
| `log.txt` | This run's log: every source and output it opens, errors, and crashes. |
| `log.previous.txt` | The previous run's log. If the app vanished, look here. |

The app understands two command-line flags:

- `--background`: start without opening the browser. Start with Windows uses
  this.
- `--verbose`: log extra debug detail.

**Listen says "Your Windows default output is …, which the mix already goes
to"**: Listen plays on the Windows default output, so that must be your
headphones, not the virtual cable. Change it in Settings → System → Sound →
Output; Listen picks up the change by itself.

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
| Mic processing: RNNoise noise suppression, and a noise gate driven by RNNoise's voice detector or a level | `src/dsp.rs` |
| Soundboard: file decoding (Symphonia), resampling to 48 kHz, playback | `src/soundboard.rs` |
| Engine: config, capture/render/monitor threads, supervisor that restarts captures, output and Listen as apps/devices come and go | `src/engine.rs` |
| WASAPI backend: shared-mode event-driven streams, process loopback, device and session enumeration | `src/backend/wasapi.rs` |
| Demo backend for non-Windows hosts | `src/backend/dummy.rs` |
| Global hotkeys (polled, non-blocking) | `src/hotkeys.rs` |
| HTTP/WebSocket API (axum) and embedded UI | `src/server.rs`, `web/` |
| Tray icon, autostart, browser launch | `src/tray.rs`, `src/platform.rs` |

All audio runs at 48 kHz stereo float. Windows converts each device's native
format (`AUTOCONVERTPCM`), and sounds are resampled to 48 kHz when they're
loaded. Each source keeps about 30 ms of buffer, so total added latency is
roughly 40–60 ms. Sources and the output run on separate clocks, so the mixer
corrects drift continuously: when a source's buffer strays from its target it
takes one frame more or less per 10 ms block and stretches the block to fit, a
pitch change of about 0.2% for that block.

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
| PATCH | `/api/channels/{id or "master"}` | `{"gain_db":-6,"muted":true,"name":"…"}` (`name` applies to sources only). Input devices also take `noise_suppression`, `noise_gate` (booleans), `gate_mode` (`"auto"` or `"manual"`) and `gate_db` (−60 to 0, for manual). |
| DELETE | `/api/channels/{id}` | Removing the soundboard keeps its sounds; add it back with `POST /api/sources` `{"kind":"soundboard"}`. |
| PUT | `/api/output` | `{"device":{"id":"…","name":"…"}}` or `{"device":null}` |
| POST | `/api/sounds?filename=horn.mp3&name=…` | The raw file. Adds the soundboard to the mix if it isn't there. |
| PATCH | `/api/sounds/{id}` | `{"name":"…","gain_db":-6}` |
| DELETE | `/api/sounds/{id}` | |
| POST | `/api/sounds/{id}/play` | Plays (or restarts) a sound. |
| POST | `/api/sounds/stop` | Stops all sounds. |
| PATCH | `/api/soundboard` | `{"hear_sounds":true,"hear_sounds_db":-6}` |
| PUT | `/api/hotkeys` | `[{"keys":"Ctrl+Shift+M","action":"push_to_talk","target":"…"}]`. Actions: `toggle_mute`, `push_to_talk`, `push_to_mute` (target: `master` or a source id), `play_sound` (target: a sound id), `stop_sounds`. |
| PUT | `/api/settings` | `{"autostart":true}` |
| POST | `/api/quit` | Stops the app. |
| GET | `/ws` | WebSocket: pushes `state` on change (including `monitor`, Listen's `{state, message}`) and `meters` about 30 times a second (with `playing`, the sounds playing now, and each mic's `mic_level` and `mic_gate_open`). Send `{"type":"listen","on":true}` to monitor the mix; it plays while any connection has it on, and a connection's request ends when it closes. |

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

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
