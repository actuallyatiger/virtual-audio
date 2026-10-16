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
- **Mic processing** for input devices (a mic card's settings ⚙):
  - *Echo cancellation* (WebRTC's AEC3, as in browsers and Discord) is for
    speakers: it removes what plays on the Windows default output from the
    mic, so others don't hear themselves. It adds 9 ms of delay, which the
    other sources are delayed to match. Not needed with headphones.
  - *Noise suppression* (RNNoise) removes steady background noise such as
    fans, hum and keyboard clatter. With it on, you can turn off Discord's own
    noise suppression, which tends to cut out the soundboard and music.
  - *Automatic level* keeps your voice at a steady speaking level, the same
    level WebRTC's gain control (used by browsers and most voice chat)
    aims for, with room left for loud peaks. It turns the mic up or down
    slowly while you talk and holds still between words, and shows how much
    it's adjusting by.
  - A *noise gate* silences the mic while you aren't talking. *Automatic*
    mode (the default) opens when RNNoise's voice detector hears speech
    that's louder than the room's background noise. *Manual* mode opens above a level you set against a
    live meter. Either way it stays open for 200 ms after you stop talking,
    so word endings aren't clipped, and it only gates the mic: the
    soundboard and apps always get through.
  - *Extra strength* (automatic mode) makes the gate stricter, so sudden
    sounds like a glass put down or a finger snap don't open it. It listens
    30 ms ahead so it doesn't cut off your words' starts, which adds 30 ms
    delay to the mic; *Keep everything else in time* adds the same delay to
    the other sources, e.g. to sing along to music in the mix. (Noise
    suppression's own 10 ms is always matched, without a setting.)

  Either one makes the mic mono and delays it by up to 10 ms (extra
  strength: 30 ms more); the other sources are delayed to match noise
  suppression's 10 ms automatically.
- **Soundboard**: add MP3, WAV, OGG, FLAC or M4A files (up to 60 seconds and
  20 MB each, 64 sounds) with **+ Add sounds** or by dropping them on the
  soundboard. Click a sound to play it into the mix; clicking it again
  restarts it. Drag sounds to reorder them. Each sound has its own volume,
  and the soundboard is a mixer channel with its own volume, mute and
  hotkeys.
- **Even out volumes** (on by default): each sound is measured (perceived
  loudness, as in EBU R128) and turned up or down to a similar level a
  little below a speaking voice, without clipping. A sound's own volume
  applies on top.
- **Trim sounds** in a sound's settings ⚙: drag the edges of its waveform (or type
  times), add fades, or **Trim silence** off both ends in one click.
  Trimming doesn't change the file, so **Reset** brings the whole sound
  back. **Preview** plays it only to you, through Hear sounds.
- **Music**: add folders (searched, with subfolders) or files by path, or
  with the Windows file picker. Tracks stream from disk one after another,
  with play/pause, next/previous, seeking, shuffle and repeat (all or one),
  and hotkeys for play/pause, next and previous. A track's × takes it out
  of the playlist and **Clear** empties it (the files stay). **Turn the
  music down while I talk** ducks it while your mic hears you. Music is its
  own mixer channel, and **Hear music** plays it to you at your own volume.
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
  channel, play a sound, stop all sounds, or play/pause, skip or go back
  in the music.
  Keyboard combos and mouse side buttons (Mouse4/Mouse5) both work. Hotkeys
  work in other apps, including games. While an app running as
  administrator is focused (some games do this), Windows hides its keys
  from other apps, so run the mixer as administrator too
  (Settings → Start with Windows → as Administrator).
- **Setup check**, **Help** and **Settings** in the top bar, and an ⓘ next
  to each section explaining it. The theme follows your system's light or
  dark setting, or Settings picks one.
- **Settings** has Start with Windows, the theme, the soundboard's even
  out volumes, and updates.
- Start with Windows (runs in the background, tray icon only). It installs
  the mixer in `%LOCALAPPDATA%\Programs\Virtual Audio Mixer` and switches
  to that copy straight away, so you can delete the file you downloaded.
  Opening a newer download later updates that copy. Optionally
  **as Administrator** so hotkeys also work in games that run as
  administrator. That uses a Task Scheduler task with highest privileges,
  so Windows asks for permission once rather than at every sign-in. The
  task starts a copy of the mixer in `C:\Program Files\Virtual Audio Mixer`,
  where only administrators can change it: starting the downloaded file
  as administrator would let any program that can replace it run as
  administrator too. Turning it on switches to that copy straight away.
  When you open another version, it offers to close the one that's
  running, then to make it the copy that starts as administrator (one
  Windows prompt), and switches to that copy so the file you opened can be
  deleted. Say no, and **Setup check** offers it later. It needs
  your account to be an administrator.
- **Updates itself**: new versions download in the background and are
  checked to be genuine (signed) releases. **Restart to update** in the bar
  at the top (or the tray menu) installs one now; otherwise it's installed
  the next time the mixer starts. In Settings → Updates you can choose to
  be told first, or not to check at all.
  The first version with updates, 0.2, has to be downloaded by hand.
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
2. Download `virtual-audio-mixer.exe` from the
   [latest release](https://github.com/actuallyatiger/virtual-audio/releases/latest)
   and run it. The mixer opens in your browser, and a tray icon appears.
   Left-click it to reopen the page; right-click it for **Open mixer**,
   **Open log file** and **Quit** (its first line shows the version).
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
| `config.json` | Your sources, output, hotkeys, sounds, music playlist and volumes. |
| `sounds\` | Copies of the files you added to the soundboard. |
| `config.<timestamp>.json.bad` | A `config.json` that could not be read. It is moved aside and the app starts with defaults, so you can repair it. If it can't be moved aside, the app doesn't start rather than overwrite it. |
| `log.txt` | This run's log: every source and output it opens, errors, and crashes. |
| `log.previous.txt` | The previous run's log. If the app vanished, look here. |
| `update\` | A downloaded update waiting to be installed. (The copy that starts as administrator keeps its own in its Program Files folder.) |

The app understands these command-line flags:

- `--background`: start without opening the browser. Start with Windows uses
  this.
- `--verbose`: log extra debug detail.
- `--replacing`: used by updates: wait for the copy being replaced to quit.
- `--handed-over`: used by Start with Windows when another copy starts the
  installed one: don't hand over again.

**Listen says "Your Windows default output is …, which the mix already goes
to"**: Listen plays on the Windows default output, so that must be your
headphones, not the virtual cable. Change it in Settings → System → Sound →
Output; Listen picks up the change by itself.

Logs go to these files only in Windows release builds. Debug builds and demo
mode print them to the terminal instead.

## Building and contributing

```sh
cargo build --release            # on Windows → target/release/virtual-audio-mixer.exe
cargo run                        # elsewhere: a demo backend at http://127.0.0.1:47800
```

- [docs/development.md](docs/development.md): building, checks, testing and
  releases.
- [docs/architecture.md](docs/architecture.md): how it works.
- [docs/api.md](docs/api.md): the HTTP and WebSocket API.
- [AGENTS.md](AGENTS.md): conventions for contributors, human or AI.

## Roadmap

- Our own virtual microphone driver, so the device can be called "Virtual
  Mixer Mic" and no third-party cable is needed. The output is just a device
  setting, so this slots in without engine changes. It needs Microsoft driver
  signing (or test-signing mode) to install.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
