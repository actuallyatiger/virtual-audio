# How it works

| Piece | Where |
| --- | --- |
| Mixing core: per-source SPSC ring buffers, gain ramps, meters, soft limiter | `src/mixer.rs` |
| Mic processing: RNNoise noise suppression, automatic level (speech-driven gain), and a noise gate driven by RNNoise's voice detector or a level | `src/dsp.rs` |
| Audio file decoding (Symphonia) and streaming resampling to 48 kHz | `src/decode.rs` |
| Soundboard: sounds in memory, trims, playback | `src/soundboard.rs` |
| Music: playlist, a decode thread streaming tracks from disk, ducking | `src/music.rs` |
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
pitch change of about 0.2% for that block. A source's target can be raised
to delay it, keeping it in time with a mic whose processing runs late (see
`Engine::apply_delays`).

The web UI is plain HTML/CSS/JS with no build step. It is compiled into the
executable with `include_str!`, so the release is a single `.exe`.

## Threads

- **Capture threads**, one per source: WASAPI capture (a device, or an app's
  process loopback), the soundboard player or the music player. Each pushes
  48 kHz stereo into its own lock-free SPSC ring (`rtrb`). A mic's capture
  thread also runs its processing (`dsp::Processor`).
- **Render thread**: pulls from every ring through `Mixer::process` under the
  mixer's mutex and writes the mix to the output device.
- **Monitor threads** (Listen, Hear sounds and music): play taps of the mix,
  the soundboard or the music on the default output.
- **Supervisor**: every 500 ms it (re)starts captures, the output and the
  monitors as devices and apps come and go, and applies source delays.
- **Music decoder**: streams the current track into a half-second feed.
- **Hotkeys**: polls key state every 10 ms.
- The axum server runs on a Tokio runtime; blocking work goes through
  `spawn_blocking`.

Rules that keep the audio glitch-free: nothing on the render or capture
paths allocates, blocks or waits on a lock another thread can hold for long
(taps use `try_lock` and skip a block rather than wait). Where two locks are
held, the engine's `inner` is always taken before the mixer.

## Mic processing delay

Noise suppression delays the mic by 10 ms (RNNoise's output lags its input
by a frame). The gate's *extra strength* looks 30 ms ahead. Gathering 10 ms
frames adds nothing while Windows delivers 10 ms blocks, as it usually does,
and automatic level, which only needs the voice detector's verdict, adds
nothing either.
The other sources are delayed to match suppression's 10 ms always, and
extra strength's 30 ms with *keep everything else in time*
(`Engine::apply_delays`, `Mixer::set_delay`).

## Web UI

Plain HTML, CSS and JavaScript in `web/`, with no build step, compiled into
the executable with `include_str!`. The page talks to the server over the
JSON API and a WebSocket that pushes state and meters (see [api.md](api.md)).
