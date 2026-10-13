---
name: run
description: Run Virtual Audio Mixer's demo backend in isolation and drive its web UI in a headless browser, to see a change working or take screenshots. Use when asked to run, start or screenshot the app, or before committing a UI change.
---

# Run the app

On Linux (or macOS) the app builds with a demo backend: fake microphones,
outputs and apps that play tones, behind the real engine, mixer and web UI.
Windows-only behaviour (WASAPI, UAC, file pickers, the tray) can't run here.

## Start it, isolated

Never use the real config. Give each run its own config folder and port:

```sh
cargo build
S=<scratch dir>/run1
mkdir -p "$S/cfg/VirtualAudioMixer"
echo '{"port": 47990}' > "$S/cfg/VirtualAudioMixer/config.json"
XDG_CONFIG_HOME="$S/cfg" ./target/debug/virtual-audio-mixer --background > "$S/log" 2>&1 &
echo $! > "$S/pid"
sleep 2
```

`--background` stops it opening a browser. The UI is at
`http://127.0.0.1:47990`. Stop it with `kill $(cat "$S/pid")`. Never `pkill`:
other runs, or the user's, may be going.

## Set it up through the API

Requests must carry the matching `Host` (the server rejects others):

```sh
H=(-H 'Host: 127.0.0.1:47990' -H 'Content-Type: application/json')
curl -s -X POST http://127.0.0.1:47990/api/sources "${H[@]}" -d '{"kind":"device","device":null}'   # a mic
curl -s -X POST 'http://127.0.0.1:47990/api/sounds?filename=a.mp3' -H 'Host: 127.0.0.1:47990' --data-binary @a.mp3
curl -s -X POST http://127.0.0.1:47990/api/music/items "${H[@]}" -d '{"path":"/abs/folder"}'
```

`ffmpeg` makes test audio, e.g.
`ffmpeg -f lavfi -i sine=frequency=440:duration=3 -metadata title=Test a.mp3`.
The full API is in `docs/api.md`.

## Drive the UI

Playwright is installed globally; Chromium is at `/opt/pw-browsers/chromium`
(don't run `playwright install`):

```js
// NODE_PATH=$(npm root -g) node shot.js
const { chromium } = require("playwright");
(async () => {
  const b = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium" });
  const p = await b.newPage({ viewport: { width: 390, height: 900 }, colorScheme: "dark", deviceScaleFactor: 2 });
  const errors = [];
  p.on("pageerror", (e) => errors.push(e.message));
  await p.goto("http://127.0.0.1:47990/");
  await p.waitForTimeout(800); // first state over the WebSocket
  await p.screenshot({ path: "shot.png", fullPage: true });
  console.log(errors);
  await b.close();
})();
```

- `state`, `render()` and `$()` are globals in the page. `p.evaluate(() =>
  render({ ...state, platform_supported: true }))` shows Windows-only
  controls; override other fields the same way to see rare states.
- Check light and dark (`colorScheme`), 320 px and 390 px wide, keyboard use
  (`p.keyboard`), and that `pageerror` stays empty.
- The server embeds `web/` at build time: `cargo build` after editing it.

## Showing the result

Look at screenshots yourself before sharing them. For visual changes, show the
user the pictures and get an OK before committing, rather than iterating
through commits. Crop to the part that changed, and put variants side by side
(e.g. with Python's PIL).
