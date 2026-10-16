#!/bin/bash
# Sets up every feature of a running demo build through its API, then quits
# it, so its config.json can become src/testdata/config/<version>.json (see
# SKILL.md). Usage: save-config.sh PORT. Needs curl, python3 and ffmpeg.
set -u
P=$1
U=http://127.0.0.1:$P
M=$(mktemp -d)
trap 'rm -rf "$M"' EXIT
mkdir "$M/music"
ffmpeg -loglevel error -f lavfi -i sine=frequency=440:duration=2 -metadata title=Horn "$M/horn.mp3"
ffmpeg -loglevel error -f lavfi -i sine=frequency=330:duration=3 -metadata title=Song "$M/music/song.mp3"
H=(-H "Host: 127.0.0.1:$P" -H 'Content-Type: application/json')
j() { python3 -c "import sys,json;d=json.load(sys.stdin);print(eval(sys.argv[1]))" "$1"; }
req() { curl -s -o /dev/null -w "%{http_code} $1 $2\n" -X "$1" "$U$2" "${H[@]}" -d "$3"; }
devs=$(curl -s $U/api/devices -H "Host: 127.0.0.1:$P")
in=$(echo "$devs" | j 'json.dumps({"id":d["inputs"][-1]["id"],"name":d["inputs"][-1]["name"]})')
out=$(echo "$devs" | j 'json.dumps({"id":d["outputs"][0]["id"],"name":d["outputs"][0]["name"]})')
exe=$(curl -s $U/api/apps -H "Host: 127.0.0.1:$P" | j 'd[0]["exe"] if isinstance(d,list) else d["apps"][0]["exe"]')
mic=$(curl -s -X POST $U/api/sources "${H[@]}" -d '{"kind":"device","device":null}' | j 'd["id"]')
mic2=$(curl -s -X POST $U/api/sources "${H[@]}" -d "{\"kind\":\"device\",\"device\":$in,\"name\":\"Second mic\"}" | j 'd["id"]')
app=$(curl -s -X POST $U/api/sources "${H[@]}" -d "{\"kind\":\"app\",\"exe\":\"$exe\"}" | j 'd["id"]')
req PUT /api/output "{\"device\":$out}"
req PATCH /api/channels/master '{"gain_db":-1.5}'
req PATCH /api/channels/$app '{"gain_db":-6,"muted":true,"name":"Game"}'
# One field at a time, so one an older build doesn't know fails alone.
for f in '"gain_db":3' '"noise_suppression":true' '"noise_gate":true' '"gate_mode":"auto"' \
  '"extra_strength":true' '"keep_in_time":true' '"auto_level":true' '"echo_cancellation":true' '"hear":true'; do
  req PATCH /api/channels/$mic "{$f}"
done
for f in '"noise_gate":true' '"gate_mode":"manual"' '"gate_db":-42'; do req PATCH /api/channels/$mic2 "{$f}"; done
snd=$(curl -s -X POST "$U/api/sounds?filename=horn.mp3&name=Horn" -H "Host: 127.0.0.1:$P" --data-binary @"$M/horn.mp3" | j 'd["id"]')
req PATCH /api/sounds/$snd '{"gain_db":-3,"start_ms":100,"end_ms":1500,"fade_in_ms":10,"fade_out_ms":50}'
for f in '"hear_sounds":false' '"hear_sounds_db":-4' '"even_sounds":true'; do req PATCH /api/soundboard "{$f}"; done
req POST /api/music/items "{\"path\":\"$M/music\"}"
for f in '"shuffle":true' '"repeat":"one"' '"duck":true' '"duck_db":-9' '"hear":true' '"hear_db":-5'; do req PATCH /api/music "{$f}"; done
req PUT /api/hotkeys "[{\"id\":\"hk1\",\"keys\":\"Ctrl+Shift+M\",\"action\":\"toggle_mute\",\"target\":\"master\"},{\"id\":\"hk2\",\"keys\":\"F13\",\"action\":\"push_to_talk\",\"target\":\"$mic\"},{\"id\":\"hk3\",\"keys\":\"Ctrl+Alt+1\",\"action\":\"play_sound\",\"target\":\"$snd\"}]"
req PUT /api/settings '{"theme":"dark"}'
req PUT /api/settings '{"updates":"notify"}'
req POST /api/quit ''
