# HTTP and WebSocket API

The UI's server listens on `http://127.0.0.1:47800` by default (`port` in
`config.json`).

| Method | Path | Body |
| --- | --- | --- |
| GET | `/api/state` | Everything the UI shows, including `version` (with `-dev.<commit>` for a test build), `admin_copy_version` (the Program Files copy's version if there is one, `"unknown"` if it predates versions) and `admin_copy_outdated`, and `update` (`{state, version, notes_url, message}`; `state` is `idle`, `checking`, `up_to_date`, `available`, `downloading`, `ready`, `error` or `unsupported`) with `updates`, the update setting. Each sound has `even_db`, how much even out volumes (`even_sounds`) turns it up or down. |
| GET | `/api/devices` | |
| GET | `/api/apps` | |
| GET | `/api/bluetooth` | `{"devices":[{"id","name"}]}`: paired devices (phones) that can play to this PC. Takes a few seconds. |
| POST | `/api/sources` | `{"kind":"app","exe":"chrome.exe"}`, `{"kind":"device","device":null}` or `{"kind":"bluetooth","device":{"id","name"}}` (from `/api/bluetooth`), plus an optional `"name"`; or `{"kind":"soundboard"}` / `{"kind":"music"}` (one of each) |
| PUT | `/api/sources/order` | `{"ids":["…","…"]}` (every source id, in the new order) |
| PATCH | `/api/channels/{id or "master"}` | `{"gain_db":-6,"muted":true,"name":"…"}` (`name` applies to sources only). Input devices also take `noise_suppression`, `noise_gate` (booleans), `gate_mode` (`"auto"` or `"manual"`), `gate_db` (−60 to 0, for manual), `extra_strength` and `keep_in_time` (booleans, for automatic), `auto_level` and `echo_cancellation` (booleans). Each source in the state has `echo_status`: `null` unless echo cancellation is on, else `{state, message}` with `state` `waiting`, `running` (`message` is the output it listens to) or `error` (`message` says why). |
| DELETE | `/api/channels/{id}` | Removing the soundboard keeps its sounds, and removing the music keeps its playlist (and pauses it); add them back with `POST /api/sources` `{"kind":"soundboard"}` or `{"kind":"music"}`. |
| PUT | `/api/output` | `{"device":{"id":"…","name":"…"}}` or `{"device":null}` |
| POST | `/api/sounds?filename=horn.mp3&name=…` | The raw file. Adds the soundboard to the mix if it isn't there. |
| PATCH | `/api/sounds/{id}` | `{"name":"…","gain_db":-6,"start_ms":120,"end_ms":1500,"fade_in_ms":0,"fade_out_ms":50}` (`"end_ms":null` plays to the end) |
| GET | `/api/sounds/{id}/waveform` | Peaks for drawing, and where the silence at either end starts and stops. |
| POST | `/api/sounds/{id}/preview` | Plays a sound only to you (through Hear sounds), not into the mix. |
| DELETE | `/api/sounds/{id}` | |
| POST | `/api/sounds/{id}/play` | Plays (or restarts) a sound. |
| POST | `/api/sounds/stop` | Stops all sounds. |
| PUT | `/api/sounds/order` | `{"ids":["…","…"]}` (every sound id, in the new order) |
| PATCH | `/api/soundboard` | `{"hear_sounds":true,"hear_sounds_db":-6,"even_sounds":true}` |
| POST | `/api/music/items` | `{"path":"C:\\Users\\you\\Music"}`: a folder or audio file. Adds the music player to the mix if it isn't there. |
| DELETE | `/api/music/items/{id}` | |
| DELETE | `/api/music/items` | Clears the playlist (the files stay) |
| POST | `/api/music/control` | `{"action":"toggle"}`, `pause`, `next`, `previous`, `{"action":"play","track":3}`, `{"action":"seek","position_ms":60000}` |
| PATCH | `/api/music` | `{"shuffle":true,"repeat":"all","duck":true,"duck_db":-12,"hear":true,"hear_db":-6}` (`repeat`: `off`, `all`, `one`) |
| POST | `/api/music/tracks/remove` | `{"track":3,"name":"Song"}` (its index and name in the list): takes it out of the playlist, never off the disk |
| POST | `/api/music/tracks/restore` | Puts every removed track back |
| POST | `/api/music/browse` | `{"folders":true}`: shows the Windows file picker; returns `{"paths":[…]}` |
| PUT | `/api/hotkeys` | `[{"keys":"Ctrl+Shift+M","action":"push_to_talk","target":"…"}]`. Actions: `toggle_mute`, `push_to_talk`, `push_to_mute` (target: `master` or a source id), `play_sound` (target: a sound id), `stop_sounds`, `music_play_pause`, `music_next`, `music_previous`. |
| PUT | `/api/settings` | `{"autostart":true,"autostart_admin":false,"theme":"system","updates":"auto"}` (`theme`: `"system"`, `"light"` or `"dark"`; `updates`: `"auto"` to download what's found, `"notify"` to offer it, `"off"` to check only when asked). Changing `autostart_admin` shows a Windows permission prompt and waits for it. Turning Start with Windows on, from a copy that isn't the installed one, then quits so the installed copy takes over (the WebSocket reconnects to it). |
| POST | `/api/quit` | Stops the app. |
| POST | `/api/update/check` | Checks for an update now (and downloads it, with `updates` on `auto`). |
| POST | `/api/update/download` | Downloads the update that's out (or retries a failed download). |
| POST | `/api/update/install` | Installs the downloaded update and starts it; this copy then quits. |
| GET | `/ws` | WebSocket: pushes `state` on change (including `monitor`, Listen's `{state, message}`) and `meters` about 30 times a second (with `playing`, the sounds playing now, each mic's `mic_level` and `mic_gate_open`, `mic_auto_level_db` for mics with automatic level on, and `music_position_ms`). Send `{"type":"listen","on":true}` to monitor the mix; it plays while any connection has it on, and a connection's request ends when it closes. |

Requests whose `Host` or `Origin` isn't `127.0.0.1`/`localhost` on our port are
rejected, and the UI can't be embedded in another site's frame. This stops
other websites from controlling the mixer. Invalid input gets a 400 with a
message (a body that doesn't fit the expected types gets a 422); names are limited to 100 characters, sources and hotkeys to 64
each, and the playlist to 200 files and folders (10,000 tracks; a folder
scan also stops after 200,000 entries).
