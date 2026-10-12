"use strict";

const $ = (sel, root = document) => root.querySelector(sel);

let state = null;
let outputs = [];
const strips = new Map(); // channel id -> { el, parts }
let hotkeyDrafts = []; // rows added in the UI that have no keys yet
let recording = null; // { key, cancel, stop }
let socket = null;
let listening = false; // whether this page has Listen on
let dragId = null; // source being dragged
let playing = new Set(); // ids of the soundboard sounds playing now

// ---------------------------------------------------------------- utilities

async function api(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || `${res.status} ${res.statusText}`);
  }
  return res.status === 204 ? null : res.json();
}

let toastTimer;
function toast(message) {
  const el = $("#toast");
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 5000);
}

function attempt(promise) {
  promise.catch((e) => toast(e.message));
}

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    // Hyphenated names (aria-label, data-*) only exist as attributes.
    if (key.includes("-")) node.setAttribute(key, value);
    else node[key] = value;
  }
  node.append(...children);
  return node;
}

function formatDb(db) {
  if (db <= -60) return "−∞ dB";
  const sign = db > 0 ? "+" : db < 0 ? "−" : "";
  return `${sign}${Math.abs(db).toFixed(1)} dB`;
}

function throttle(fn, ms) {
  let last = 0, timer = null, pending;
  return (...args) => {
    pending = args;
    const wait = last + ms - Date.now();
    if (wait <= 0) {
      last = Date.now();
      fn(...pending);
    } else if (!timer) {
      timer = setTimeout(() => {
        timer = null;
        last = Date.now();
        fn(...pending);
      }, wait);
    }
  };
}

// ---------------------------------------------------------------- live updates

function connect() {
  const conn = $("#conn");
  const ws = new WebSocket(`ws://${location.host}/ws`);
  socket = ws;
  ws.onopen = () => {
    conn.textContent = "connected";
    conn.className = "conn up";
  };
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.type === "state") render(msg.state);
    else if (msg.type === "meters") {
      updateMeters(msg.meters);
      updatePlaying(msg.playing || []);
    }
  };
  ws.onclose = () => {
    conn.textContent = "disconnected";
    conn.className = "conn down";
    // The server stops Listen when our connection drops.
    listening = false;
    renderListen();
    setTimeout(connect, 1000);
  };
}

function render(next) {
  state = next;
  $("#demo-banner").hidden = state.platform_supported;
  $("#autostart-wrap").hidden = !state.platform_supported;
  $("#autostart").checked = state.autostart;
  renderOutput();
  renderStrips();
  renderListen();
  renderSounds();
  renderHotkeys();
}

// ---------------------------------------------------------------- output

async function loadOutputs() {
  try {
    outputs = (await api("GET", "/api/devices")).outputs;
  } catch (e) {
    toast(e.message);
  }
  renderOutput();
}

function renderOutput() {
  if (!state) return;
  const select = $("#output");
  const current = state.output;
  const options = [el("option", { value: "", textContent: "None (meters only)" })];
  const known = outputs.some((d) => current && d.id === current.id);
  for (const d of outputs) {
    options.push(el("option", { value: d.id, textContent: d.name + (d.is_default ? "  (Windows default)" : "") }));
  }
  if (current && !known) {
    options.push(el("option", { value: current.id, textContent: `${current.name} (not found)` }));
  }
  if (document.activeElement !== select) {
    select.replaceChildren(...options);
    select.value = current ? current.id : "";
  }

  const status = $("#out-status");
  status.className = `status ${state.output_status.state}`;
  status.textContent = state.output_status.state === "running" ? "Running" : state.output_status.message;

  const hint = $("#output-hint");
  const cable = outputs.find((d) => /cable input/i.test(d.name));
  if (current && /cable input/i.test(current.name)) {
    hint.innerHTML = "In Discord, open <b>Settings → Voice &amp; Video</b> and set the input device to <b>CABLE Output (VB-Audio Virtual Cable)</b>.";
  } else if (!cable && state.platform_supported) {
    hint.innerHTML = 'Install the free <a href="https://vb-audio.com/Cable/" target="_blank" rel="noreferrer">VB-Audio Virtual Cable</a>, then pick <b>CABLE Input</b> here. Other apps then use <b>CABLE Output</b> as their microphone.';
  } else {
    hint.innerHTML = "Pick the playback side of a virtual cable (e.g. <b>CABLE Input</b>). Other apps then select its recording side (<b>CABLE Output</b>) as their microphone. Don't send the mix to your speakers if one of the sources is an app you're listening to.";
  }
}

$("#output").addEventListener("change", (e) => {
  const id = e.target.value;
  const dev = outputs.find((d) => d.id === id) || (state.output && state.output.id === id ? state.output : null);
  attempt(api("PUT", "/api/output", { device: dev ? { id: dev.id, name: dev.name } : null }));
  e.target.blur();
});
$("#refresh-outputs").addEventListener("click", loadOutputs);

// ---------------------------------------------------------------- strips

function channelList() {
  const list = [{
    id: "master",
    name: "Master",
    detail: "Final mix sent to the output",
    icon: "🎚",
    gain_db: state.master.gain_db,
    muted: state.master.muted,
    status: null,
  }];
  for (const s of state.sources) {
    const [detail, icon] = {
      app: [s.exe, "🗔"],
      device: [s.device ? s.device.name : "Default microphone (follows Windows)", "🎙"],
      soundboard: ["Sounds from the soundboard below", "🔊"],
    }[s.kind];
    list.push({ ...s, detail, icon });
  }
  return list;
}

function createStrip(ch) {
  const node = $("#strip-tpl").content.firstElementChild.cloneNode(true);
  const parts = {
    icon: $(".kind-icon", node),
    name: $(".name", node),
    detail: $(".detail", node),
    status: $(".status", node),
    mask: $(".meter-mask", node),
    gain: $(".gain", node),
    gainValue: $(".gain-value", node),
    mute: $(".mute", node),
    remove: $(".remove", node),
    handle: $(".handle", node),
    listen: $(".listen", node),
    monitorStatus: $(".monitor-status", node),
    dragging: false,
  };
  const id = ch.id;
  node.dataset.id = id;
  if (id === "master") {
    node.classList.add("master");
    parts.name.readOnly = true;
    parts.remove.remove();
    parts.status.remove();
    parts.handle.remove();
    parts.listen.hidden = false;
    parts.listen.addEventListener("click", () => {
      if (!socket || socket.readyState !== WebSocket.OPEN) return;
      listening = !listening;
      socket.send(JSON.stringify({ type: "listen", on: listening }));
      renderListen();
    });
  } else {
    setUpReorder(node, parts.handle, id);
  }
  if (ch.kind === "device") setUpProcessing(node, parts, id);

  parts.gainValue.addEventListener("click", () => editGain(id, parts));

  const sendGain = throttle((db) => attempt(api("PATCH", `/api/channels/${id}`, { gain_db: db })), 60);
  parts.gain.addEventListener("pointerdown", () => (parts.dragging = true));
  parts.gain.addEventListener("pointerup", () => (parts.dragging = false));
  parts.gain.addEventListener("input", () => {
    const db = Number(parts.gain.value);
    parts.gainValue.textContent = formatDb(db);
    parts.gain.setAttribute("aria-valuetext", formatDb(db));
    sendGain(db);
  });
  parts.gain.addEventListener("dblclick", () => {
    parts.gain.value = 0;
    parts.gainValue.textContent = formatDb(0);
    sendGain(0);
  });
  parts.mute.addEventListener("click", () => {
    const muted = parts.mute.getAttribute("aria-pressed") !== "true";
    attempt(api("PATCH", `/api/channels/${id}`, { muted }));
  });
  parts.name.addEventListener("change", () => attempt(api("PATCH", `/api/channels/${id}`, { name: parts.name.value })));
  parts.name.addEventListener("keydown", (e) => e.key === "Enter" && parts.name.blur());
  parts.remove.addEventListener("click", () => {
    if (confirm(`Remove “${parts.name.value}” from the mix?`)) attempt(api("DELETE", `/api/channels/${id}`));
  });
  return { el: node, parts };
}

/** Wires up a microphone's noise suppression and noise gate controls. */
function setUpProcessing(node, parts, id) {
  const proc = $(".processing", node);
  proc.hidden = false;
  Object.assign(parts, {
    proc,
    procSummary: $(".proc-summary", node),
    suppress: $(".suppress", node),
    gateOn: $(".gate-on", node),
    gateControls: $(".gate-controls", node),
    micMask: $(".mic-meter .meter-mask", node),
    thresholdMark: $(".threshold-mark", node),
    threshold: $(".threshold", node),
    thresholdValue: $(".threshold-value", node),
    gateState: $(".gate-state", node),
  });
  const patch = (body) => attempt(api("PATCH", `/api/channels/${id}`, body));
  parts.suppress.addEventListener("change", () => patch({ noise_suppression: parts.suppress.checked }));
  parts.gateOn.addEventListener("change", () => {
    parts.gateControls.hidden = !parts.gateOn.checked;
    patch({ noise_gate: parts.gateOn.checked });
  });
  const sendThreshold = throttle((db) => patch({ gate_db: db }), 60);
  parts.threshold.addEventListener("input", () => {
    const db = Number(parts.threshold.value);
    showThreshold(parts, db);
    sendThreshold(db);
  });
}

function showThreshold(parts, db) {
  const text = `${db < 0 ? "−" : ""}${Math.abs(db)} dB`;
  parts.thresholdValue.textContent = text;
  parts.threshold.setAttribute("aria-valuetext", text);
  parts.thresholdMark.style.left = `${((db + 60) / 60) * 100}%`;
}

/** Parses a typed gain: accepts "−6", "-6,5", "-6 dB". Returns null if invalid. */
function parseDb(text) {
  const normalised = text.trim().replace(/\s*db$/i, "").replace("\u2212", "-").replace(",", ".");
  if (normalised === "") return null;
  const db = Number(normalised);
  return Number.isFinite(db) ? Math.max(-60, Math.min(12, Math.round(db * 2) / 2)) : null;
}

/** Swaps the dB label for a text field; Enter or blur applies, Esc cancels. */
function editGain(id, parts) {
  const opened = parts.gain.value;
  const input = el("input", {
    type: "text",
    inputMode: "decimal",
    className: "gain-input",
    value: opened,
    "aria-label": `${parts.name.value} volume in dB`,
  });
  parts.gainValue.replaceWith(input);
  input.focus();
  input.select();
  let done = false;
  const finish = (apply, refocus) => {
    if (done) return;
    done = true;
    const db = parseDb(input.value);
    // Only send a change the user made: blurring an untouched field must not
    // overwrite a newer gain that arrived meanwhile.
    if (apply && db !== null && input.value !== opened) {
      parts.gain.value = db;
      parts.gainValue.textContent = formatDb(db);
      attempt(api("PATCH", `/api/channels/${id}`, { gain_db: db }));
    }
    input.replaceWith(parts.gainValue);
    // Refocus only for keyboard exits; a click or Tab elsewhere keeps its target.
    if (refocus) parts.gainValue.focus();
  };
  input.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" && e.key !== "Escape") return;
    // Otherwise the same keypress "clicks" the label we refocus, reopening this.
    e.preventDefault();
    finish(e.key === "Enter", true);
  });
  input.addEventListener("blur", () => finish(true, false));
}

function renderListen() {
  const master = strips.get("master");
  if (!master || !state) return;
  const { listen, monitorStatus } = master.parts;
  listen.setAttribute("aria-pressed", String(listening));
  listen.textContent = listening ? "🎧 Listening" : "🎧 Listen";
  const m = state.monitor;
  monitorStatus.hidden = m.state === "idle";
  monitorStatus.className = `status monitor-status ${m.state}`;
  const text = m.state === "running" ? `Listening on ${m.message}` : m.message;
  // Listen is shared: say so when another window turned it on.
  monitorStatus.textContent = !listening && m.state !== "idle" ? `${text} (turned on in another window)` : text;
}

function sourceOrder() {
  return state.sources.map((s) => s.id);
}

/** Applies a new order locally right away (so quick moves build on each
 * other) and saves it, restoring the previous order if the save fails. */
function saveOrder(ids) {
  if (JSON.stringify(ids) === JSON.stringify(sourceOrder())) return;
  const previous = state.sources;
  state.sources = ids.map((id) => previous.find((s) => s.id === id));
  renderStrips();
  api("PUT", "/api/sources/order", { ids }).catch((e) => {
    state.sources = previous;
    renderStrips();
    toast(e.message);
  });
}

let dropped = false; // whether the current drag ended in a drop on the mixer

function setUpReorder(node, handle, id) {
  // Only the handle starts a drag, so text in the card stays selectable.
  handle.addEventListener("pointerdown", () => (node.draggable = true));
  node.addEventListener("dragstart", (e) => {
    dragId = id;
    dropped = false;
    node.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", id);
  });
  node.addEventListener("dragend", () => {
    node.draggable = false;
    node.classList.remove("dragging");
    dragId = null;
    if (dropped) {
      saveOrder([...document.querySelectorAll("#strips .strip:not(.master)")].map((n) => n.dataset.id));
    } else {
      renderStrips(); // cancelled (Esc, or dropped elsewhere): put the cards back
    }
  });
  handle.addEventListener("keydown", (e) => {
    const step = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const ids = sourceOrder();
    const from = ids.indexOf(id);
    const to = from + step;
    if (to < 0 || to >= ids.length) return;
    [ids[from], ids[to]] = [ids[to], ids[from]];
    saveOrder(ids);
  });
}

// A handle press that didn't become a drag must not leave its card draggable.
for (const type of ["pointerup", "pointercancel"]) {
  document.addEventListener(type, () => {
    if (dragId) return;
    for (const strip of strips.values()) strip.el.draggable = false;
  });
}

$("#strips").addEventListener("dragover", (e) => {
  if (!dragId) return;
  e.preventDefault();
  const dragged = strips.get(dragId).el;
  const target = e.target.closest(".strip");
  if (!target || target === dragged || target.classList.contains("master")) return;
  // Moving towards the end of the list lands after the target, towards the
  // start before it, which works whether the cards wrap in rows or stack.
  const cards = [...target.parentNode.children];
  const after = cards.indexOf(dragged) < cards.indexOf(target);
  target.parentNode.insertBefore(dragged, after ? target.nextSibling : target);
});
$("#strips").addEventListener("drop", (e) => {
  e.preventDefault();
  if (dragId) dropped = true;
});

function renderStrips() {
  const container = $("#strips");
  const channels = channelList();
  const seen = new Set();
  const ordered = [];
  for (const ch of channels) {
    seen.add(ch.id);
    let strip = strips.get(ch.id);
    if (!strip) {
      strip = createStrip(ch);
      strips.set(ch.id, strip);
    }
    const p = strip.parts;
    p.icon.textContent = ch.icon;
    strip.el.setAttribute("aria-label", ch.name);
    p.gain.setAttribute("aria-label", `${ch.name} volume`);
    p.mute.setAttribute("aria-label", `Mute ${ch.name}`);
    if (ch.id !== "master") {
      p.remove.setAttribute("aria-label", `Remove ${ch.name}`);
      p.handle.setAttribute("aria-label", `Reorder ${ch.name}`);
    }
    if (document.activeElement !== p.name) p.name.value = ch.name;
    p.detail.textContent = ch.detail;
    p.detail.title = ch.detail;
    if (ch.status && p.status.isConnected !== false) {
      p.status.className = `status ${ch.status.state}`;
      p.status.textContent = ch.status.state === "running" ? `Live · ${ch.status.message}` : ch.status.message;
      p.status.title = ch.status.message;
    }
    if (!p.dragging && document.activeElement !== p.gain) {
      p.gain.value = ch.gain_db;
      p.gainValue.textContent = formatDb(ch.gain_db);
      p.gain.setAttribute("aria-valuetext", formatDb(ch.gain_db));
      p.gainValue.setAttribute("aria-label", `${ch.name} volume ${formatDb(ch.gain_db)}. Click to type a value`);
    }
    if (p.proc) {
      p.suppress.checked = ch.noise_suppression;
      p.gateOn.checked = ch.noise_gate;
      p.gateControls.hidden = !ch.noise_gate;
      if (document.activeElement !== p.threshold) {
        p.threshold.value = ch.gate_db;
        showThreshold(p, ch.gate_db);
      }
      p.threshold.setAttribute("aria-label", `${ch.name} noise gate level`);
      const on = [ch.noise_suppression && "noise suppression", ch.noise_gate && "gate"].filter(Boolean);
      p.procSummary.textContent = `· ${on.length ? on.join(", ") : "off"}`;
    }
    p.mute.setAttribute("aria-pressed", String(ch.muted));
    p.mute.textContent = ch.muted ? "Muted" : "Mute";
    strip.el.classList.toggle("is-muted", ch.muted);
    ordered.push(strip.el);
  }
  for (const [id, strip] of strips) {
    if (!seen.has(id)) {
      strip.el.remove();
      strips.delete(id);
    }
  }
  // Only touch the DOM order when it changed (and not mid-drag), and keep
  // focus on whatever had it (e.g. a reorder handle moved by keyboard).
  const same = ordered.length === container.children.length && ordered.every((n, i) => container.children[i] === n);
  if (!same && !dragId) {
    const focused = document.activeElement;
    container.replaceChildren(...ordered);
    if (focused && container.contains(focused)) focused.focus();
  }
}

/** Shows a linear peak on a meter scaled −60…0 dB. */
function setMeter(mask, peak) {
  const db = peak > 0 ? 20 * Math.log10(peak) : -Infinity;
  const pct = Math.max(0, Math.min(1, (db + 60) / 60));
  mask.style.width = `${(1 - pct) * 100}%`;
}

function updateMeters(meters) {
  for (const [id, m] of Object.entries(meters)) {
    const strip = strips.get(id);
    if (!strip) continue;
    setMeter(strip.parts.mask, m.peak);
    strip.el.classList.toggle("gated", !m.open);
    const p = strip.parts;
    if (p.micMask && m.mic_level !== undefined && !p.gateControls.hidden) {
      setMeter(p.micMask, m.mic_level);
      p.gateState.textContent = m.mic_gate_open ? "Open" : "Closed";
      p.gateState.className = `gate-state ${m.mic_gate_open ? "open" : "closed"}`;
    }
  }
}

// ---------------------------------------------------------------- add source

// `fromList` says whether the selection came from a list (vs. typed).
const addState = { tab: "app", selection: null, fromList: false };
let listRequests = 0; // ignore responses to superseded list loads

function openAddDialog() {
  addState.selection = null;
  addState.fromList = false;
  $("#exe-input").value = "";
  $("#add-error").hidden = true;
  selectTab("app");
  updateAddButton();
  $("#add-dialog").showModal();
  loadApps();
  loadInputs();
}

function selectTab(tab) {
  addState.tab = tab;
  addState.selection = null;
  addState.fromList = false;
  $("#add-error").hidden = true;
  for (const b of document.querySelectorAll("#add-dialog [role=tab]")) b.setAttribute("aria-selected", String(b.dataset.tab === tab));
  for (const p of document.querySelectorAll("#add-dialog .tab-panel")) p.hidden = p.dataset.panel !== tab;
  for (const b of document.querySelectorAll("#add-dialog .pick-list button")) b.setAttribute("aria-pressed", "false");
  // A name still typed in the app tab is the selection again.
  if (tab === "app") selectTypedExe();
  updateAddButton();
}

function selectTypedExe() {
  let exe = $("#exe-input").value.trim();
  if (exe && !/\.exe$/i.test(exe)) exe += ".exe";
  addState.selection = exe ? { kind: "app", exe } : null;
  addState.fromList = false;
}

function sameSelection(a, b) {
  return !!a && !!b && JSON.stringify(a) === JSON.stringify(b);
}

function updateAddButton() {
  $("#add-confirm").disabled = !addState.selection;
}

function pickButton(label, sub, selection) {
  const btn = el("button", { type: "button" }, el("span", { textContent: label }), el("span", { className: "sub", textContent: sub }));
  btn.setAttribute("aria-pressed", String(addState.fromList && sameSelection(selection, addState.selection)));
  btn.addEventListener("click", () => {
    for (const b of btn.closest(".pick-list").querySelectorAll("button")) b.setAttribute("aria-pressed", "false");
    btn.setAttribute("aria-pressed", "true");
    $("#exe-input").value = "";
    $("#add-error").hidden = true;
    addState.selection = selection;
    addState.fromList = true;
    updateAddButton();
  });
  btn.addEventListener("dblclick", () => $("#add-confirm").click());
  return el("li", {}, btn);
}

async function loadApps() {
  const list = $("#app-list");
  const request = ++listRequests;
  list.replaceChildren(el("li", { className: "empty", textContent: "Loading…" }));
  try {
    const apps = await api("GET", "/api/apps");
    if (request !== listRequests) return;
    const selections = apps.map((a) => ({ kind: "app", exe: a.exe }));
    if (addState.fromList && addState.selection?.kind === "app" && !selections.some((s) => sameSelection(s, addState.selection))) {
      addState.selection = null;
      addState.fromList = false;
      updateAddButton();
    }
    const items = apps.map((a, i) => pickButton(a.exe, a.active ? "playing" : "idle", selections[i]));
    list.replaceChildren(...(items.length ? items : [el("li", { className: "empty", textContent: "No apps are playing audio right now. Start playback and refresh, or type the name below." })]));
  } catch (e) {
    if (request === listRequests) list.replaceChildren(el("li", { className: "error", textContent: e.message }));
  }
}

let inputRequests = 0;
async function loadInputs() {
  const list = $("#device-list");
  const request = ++inputRequests;
  list.replaceChildren(el("li", { className: "empty", textContent: "Loading…" }));
  try {
    const { inputs } = await api("GET", "/api/devices");
    if (request !== inputRequests) return;
    list.replaceChildren(
      pickButton("Default microphone", "follows Windows", { kind: "device", device: null }),
      ...inputs.map((d) => pickButton(d.name, d.is_default ? "default" : "", { kind: "device", device: { id: d.id, name: d.name } })),
    );
  } catch (e) {
    list.replaceChildren(el("li", { className: "error", textContent: e.message }));
  }
}

for (const b of document.querySelectorAll("#add-dialog [role=tab]")) b.addEventListener("click", () => selectTab(b.dataset.tab));
$("#refresh-apps").addEventListener("click", loadApps);
$("#exe-input").addEventListener("input", () => {
  for (const b of document.querySelectorAll("#app-list button")) b.setAttribute("aria-pressed", "false");
  $("#add-error").hidden = true;
  selectTypedExe();
  updateAddButton();
});
$("#add-cancel").addEventListener("click", () => $("#add-dialog").close());
$("#add-confirm").addEventListener("click", async (e) => {
  e.preventDefault();
  if (!addState.selection) return;
  try {
    await api("POST", "/api/sources", addState.selection);
    $("#add-dialog").close();
  } catch (err) {
    $("#add-error").textContent = err.message;
    $("#add-error").hidden = false;
  }
});
$("#add-source").addEventListener("click", openAddDialog);

// ---------------------------------------------------------------- soundboard

const MAX_SOUND_BYTES = 20 * 1024 * 1024;
let uploading = null; // { done, total } while files are being added
let editingSound = null; // id of the sound open in the edit dialog
let lastSoundsRender = "";

function renderSounds() {
  if (!state) return;
  const hint = $("#sb-hint");
  const inMix = state.sources.some((s) => s.kind === "soundboard");
  if (uploading) {
    hint.textContent = `Adding sounds… ${uploading.done} of ${uploading.total} done`;
  } else if (!state.sounds.length) {
    hint.textContent = "Add MP3, WAV, OGG, FLAC or M4A files up to 60 seconds long, or drop them here. Then click a sound to play it into the mix.";
  } else if (!inMix) {
    const add = el("button", { type: "button", className: "link", textContent: "Add it back" });
    add.addEventListener("click", () => attempt(api("POST", "/api/sources", { kind: "soundboard" })));
    hint.replaceChildren("The soundboard isn't in the mix, so nobody hears it. ", add);
  } else {
    hint.textContent = "Click a sound to play it into the mix (again to restart it). Turn on 🎧 Listen on Master to hear it yourself.";
  }

  const signature = JSON.stringify(state.sounds);
  if (signature !== lastSoundsRender) {
    lastSoundsRender = signature;
    const container = $("#sounds");
    // Keep focus on the same tile's button across the rebuild.
    const focused = document.activeElement;
    const tile = focused && container.contains(focused) ? focused.closest(".sound") : null;
    const refocus = tile && { id: tile.dataset.id, cls: focused.classList[0] };
    container.replaceChildren(...state.sounds.map(soundTile));
    if (refocus) {
      const again = container.querySelector(`.sound[data-id="${CSS.escape(refocus.id)}"] .${refocus.cls}`);
      if (again) again.focus();
    }
  }
  updatePlaying([...playing]);

  if (editingSound) {
    const sound = state.sounds.find((s) => s.id === editingSound);
    if (!sound) {
      $("#sound-dialog").close();
    } else {
      $("#sound-preview").disabled = sound.state !== "ready";
      if (document.activeElement !== $("#sound-gain")) showSoundGain(sound.gain_db);
    }
  }
}

function soundTile(sound) {
  const ready = sound.state === "ready";
  const play = el("button", {
    type: "button",
    className: "play",
    disabled: !ready,
    title: ready ? `Play ${sound.name}` : sound.state === "error" ? sound.message : "Loading…",
  }, el("span", { className: "play-icon", "aria-hidden": "true", textContent: "▶" }), el("span", { className: "sound-name", textContent: sound.name }));
  play.addEventListener("click", () => attempt(api("POST", `/api/sounds/${sound.id}/play`)));
  const edit = el("button", { type: "button", className: "edit ghost icon", textContent: "⋯", title: "Edit or delete", "aria-label": `Edit ${sound.name}` });
  edit.addEventListener("click", () => openSoundDialog(sound.id));
  const tile = el("div", { className: `sound ${sound.state}`, "data-id": sound.id }, play, edit);
  if (sound.state === "error") tile.append(el("span", { className: "sound-error", textContent: "Couldn't load this file", title: sound.message }));
  else if (sound.state === "loading") tile.append(el("span", { className: "sound-error", textContent: "Loading…" }));
  return tile;
}

function updatePlaying(ids) {
  playing = new Set(ids);
  for (const tile of document.querySelectorAll("#sounds .sound")) tile.classList.toggle("playing", playing.has(tile.dataset.id));
  $("#stop-sounds").disabled = playing.size === 0;
}

async function addSounds(files) {
  const list = [...files];
  if (!list.length || uploading) return;
  uploading = { done: 0, total: list.length };
  renderSounds();
  const failures = [];
  for (const file of list) {
    try {
      if (file.size > MAX_SOUND_BYTES) throw new Error("the file is too big (max 20 MB)");
      const res = await fetch(`/api/sounds?filename=${encodeURIComponent(file.name)}`, {
        method: "POST",
        headers: { "Content-Type": "application/octet-stream" },
        body: file,
      });
      if (!res.ok) throw new Error((await res.text()) || `${res.status} ${res.statusText}`);
    } catch (e) {
      failures.push(`${file.name}: ${e.message}`);
    }
    uploading.done++;
    renderSounds();
  }
  uploading = null;
  renderSounds();
  if (failures.length) toast(failures.join("\n"));
}

$("#add-sound").addEventListener("click", () => $("#sound-files").click());
$("#sound-files").addEventListener("change", (e) => {
  addSounds(e.target.files);
  e.target.value = "";
});
$("#stop-sounds").addEventListener("click", () => attempt(api("POST", "/api/sounds/stop")));

// Drop files on the soundboard to add them. Elsewhere, a dropped file would
// make the browser navigate away from the mixer to open it.
const board = $("#soundboard");
const hasFiles = (e) => e.dataTransfer && [...e.dataTransfer.types].includes("Files");
board.addEventListener("dragover", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  e.dataTransfer.dropEffect = "copy";
  board.classList.add("drop-target");
});
board.addEventListener("dragleave", (e) => {
  if (!board.contains(e.relatedTarget)) board.classList.remove("drop-target");
});
board.addEventListener("drop", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  board.classList.remove("drop-target");
  addSounds(e.dataTransfer.files);
});
for (const type of ["dragover", "drop"]) {
  window.addEventListener(type, (e) => {
    if (!hasFiles(e) || board.contains(e.target)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "none";
  });
}

function showSoundGain(db) {
  $("#sound-gain").value = db;
  $("#sound-gain-value").textContent = formatDb(db);
  $("#sound-gain").setAttribute("aria-valuetext", formatDb(db));
}

function openSoundDialog(id) {
  const sound = state.sounds.find((s) => s.id === id);
  if (!sound) return;
  editingSound = id;
  $("#sound-name").value = sound.name;
  showSoundGain(sound.gain_db);
  $("#sound-preview").disabled = sound.state !== "ready";
  $("#sound-dialog").showModal();
}

function saveSoundName() {
  const sound = state && state.sounds.find((s) => s.id === editingSound);
  const name = $("#sound-name").value.trim();
  if (sound && name && name !== sound.name) attempt(api("PATCH", `/api/sounds/${sound.id}`, { name }));
}

const sendSoundGain = throttle((id, db) => attempt(api("PATCH", `/api/sounds/${id}`, { gain_db: db })), 60);
$("#sound-gain").addEventListener("input", () => {
  const db = Number($("#sound-gain").value);
  $("#sound-gain-value").textContent = formatDb(db);
  $("#sound-gain").setAttribute("aria-valuetext", formatDb(db));
  sendSoundGain(editingSound, db);
});
$("#sound-name").addEventListener("change", saveSoundName);
$("#sound-preview").addEventListener("click", () => attempt(api("POST", `/api/sounds/${editingSound}/play`)));
$("#sound-delete").addEventListener("click", () => {
  const sound = state.sounds.find((s) => s.id === editingSound);
  if (!sound || !confirm(`Delete “${sound.name}” from the soundboard?`)) return;
  editingSound = null;
  $("#sound-dialog").close();
  attempt(api("DELETE", `/api/sounds/${sound.id}`));
});
$("#sound-dialog").addEventListener("close", () => {
  saveSoundName();
  editingSound = null;
});

// ---------------------------------------------------------------- hotkeys

const ACTIONS = [
  ["toggle_mute", "Toggle mute"],
  ["push_to_talk", "Push to talk"],
  ["push_to_mute", "Push to mute"],
  ["play_sound", "Play sound"],
  ["stop_sounds", "Stop all sounds"],
];

/** [value, label] choices for a hotkey's target, which depend on its action. */
function targetsFor(action) {
  if (action === "play_sound") return state.sounds.map((s) => [s.id, s.name]);
  if (action === "stop_sounds") return [["", "All sounds"]];
  return [["master", "Master"], ...state.sources.map((s) => [s.id, s.name])];
}

/** Keeps `current` if it suits `action`, otherwise picks a sensible target. */
function targetFor(action, current) {
  const targets = targetsFor(action);
  if (targets.some(([v]) => v === current)) return current;
  if (action === "play_sound" || action === "stop_sounds") return targets.length ? targets[0][0] : "";
  return state.sources[0] ? state.sources[0].id : "master";
}

const CODE_NAMES = {
  Space: "Space", Enter: "Enter", Tab: "Tab", Backspace: "Backspace", Insert: "Insert", Delete: "Delete",
  Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown", ArrowUp: "Up", ArrowDown: "Down",
  ArrowLeft: "Left", ArrowRight: "Right", Pause: "Pause", ScrollLock: "ScrollLock", CapsLock: "CapsLock",
  Backquote: "Backquote", Minus: "Minus", Equal: "Equal", BracketLeft: "BracketLeft", BracketRight: "BracketRight",
  Backslash: "Backslash", Semicolon: "Semicolon", Quote: "Quote", Comma: "Comma", Period: "Period", Slash: "Slash",
  NumpadMultiply: "NumpadMultiply", NumpadAdd: "NumpadAdd", NumpadSubtract: "NumpadSubtract",
  NumpadDecimal: "NumpadDecimal", NumpadDivide: "NumpadDivide",
};
const MODIFIERS = { Control: "Ctrl", Shift: "Shift", Alt: "Alt", Meta: "Win" };

function keyName(code) {
  let m;
  if ((m = /^Key([A-Z])$/.exec(code))) return m[1];
  if ((m = /^Digit(\d)$/.exec(code))) return m[1];
  if (/^F\d{1,2}$/.test(code) || /^Numpad\d$/.test(code)) return code;
  return CODE_NAMES[code] || null;
}

function heldModifiers(e) {
  return [e.ctrlKey && "Ctrl", e.shiftKey && "Shift", e.altKey && "Alt", e.metaKey && "Win"].filter(Boolean);
}

let draftCounter = 0;
let pendingFocus = null; // row key (or keys text) whose button gets focus after re-render

/** Stable identity for a hotkey row: server id, or a local id for drafts. */
function rowKey(h) {
  return h.id || h._draft;
}

function hotkeyRows() {
  return [...state.hotkeys, ...hotkeyDrafts];
}

function updateRow(key, changes) {
  saveHotkeys(hotkeyRows().map((h) => (rowKey(h) === key ? { ...h, ...changes } : h)));
}

function saveHotkeys(rows) {
  const previous = { saved: state.hotkeys, drafts: hotkeyDrafts };
  // Update the local list right away so a second quick edit builds on this
  // one instead of on the list from before the server's broadcast arrives.
  state.hotkeys = rows.filter((h) => h.keys);
  hotkeyDrafts = rows.filter((h) => !h.keys);
  const body = state.hotkeys.map(({ _draft, ...h }) => h);
  api("PUT", "/api/hotkeys", body).catch((e) => {
    // Restore what the server still has, keeping unsaved drafts.
    state.hotkeys = previous.saved;
    hotkeyDrafts = previous.drafts;
    renderHotkeys(true);
    toast(e.message);
  });
}

function startRecording(key, button) {
  if (recording) cancelRecording();
  const original = button.textContent;
  button.classList.add("recording");
  button.textContent = "Press keys… (Esc to cancel)";
  let mods = [];
  const finish = (keys) => {
    stopRecording();
    pendingFocus = keys || key;
    if (keys) {
      updateRow(key, { keys });
    } else {
      button.classList.remove("recording");
      button.textContent = original;
      button.focus();
      pendingFocus = null;
    }
  };
  const onKeyDown = (e) => {
    if (e.repeat) return e.preventDefault();
    // Plain Tab leaves the recorder instead of becoming a hotkey.
    if (e.code === "Tab" && !heldModifiers(e).some((m) => m !== "Shift")) return finish(null);
    e.preventDefault();
    if (e.code === "Escape") return finish(null);
    const mod = MODIFIERS[e.key];
    if (mod) {
      const now = heldModifiers(e);
      if (now.length > mods.length) mods = now;
      button.textContent = `${mods.join("+")}+…`;
      return;
    }
    const name = keyName(e.code);
    if (name) finish([...heldModifiers(e), name].join("+"));
  };
  const onKeyUp = (e) => {
    // A modifier pressed and released on its own becomes the hotkey (e.g. "Alt").
    if (MODIFIERS[e.key] && heldModifiers(e).length === 0 && mods.length) finish(mods.join("+"));
  };
  const onMouse = (e) => {
    const name = { 1: "Mouse3", 3: "Mouse4", 4: "Mouse5" }[e.button];
    if (name) {
      e.preventDefault();
      finish([...heldModifiers(e), name].join("+"));
    } else if (e.target !== button) {
      // Clicking anywhere else cancels; the click itself goes through.
      finish(null);
    }
  };
  const onBlur = () => finish(null);
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("keyup", onKeyUp, true);
  window.addEventListener("mousedown", onMouse, true);
  window.addEventListener("blur", onBlur);
  recording = {
    key,
    cancel: () => finish(null),
    stop: () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("mousedown", onMouse, true);
      window.removeEventListener("blur", onBlur);
    },
  };
}

function stopRecording() {
  if (recording) recording.stop();
  recording = null;
}

function cancelRecording() {
  if (recording) recording.cancel();
}

let lastHotkeyRender = "";
function renderHotkeys(force = false) {
  if (!state || (recording && !force)) return;
  // Drafts pointing at a source or sound that has since been removed move to
  // another target.
  for (const d of hotkeyDrafts) {
    if (d.action === "play_sound" && !state.sounds.length) d.action = "toggle_mute";
    d.target = targetFor(d.action, d.target);
  }
  const names = [state.sources.map((s) => [s.id, s.name]), state.sounds.map((s) => [s.id, s.name])];
  const signature = JSON.stringify([hotkeyRows(), names]);
  if (!force && signature === lastHotkeyRender) return;
  lastHotkeyRender = signature;

  const container = $("#hotkeys");
  const rows = hotkeyRows();
  if (!rows.length) {
    container.replaceChildren(el("p", { className: "empty", textContent: "No hotkeys yet. Add one to mute or push-to-talk any channel, or to play a sound." }));
    return;
  }
  let focusTarget = null;
  container.replaceChildren(...rows.map((row, i) => {
    const key = rowKey(row);
    const targets = targetsFor(row.action);
    const where = (targets.find(([v]) => v === row.target) || targets[0] || ["", "nothing"])[1];
    const keys = el("button", {
      className: "keys",
      textContent: row.keys || "Click to record",
      title: "Click to record a new key combination",
      "aria-label": `Hotkey ${i + 1} keys: ${row.keys || "none"}. Click to record`,
    });
    keys.addEventListener("click", () => startRecording(key, keys));
    if (pendingFocus && (pendingFocus === key || pendingFocus === row.keys)) focusTarget = keys;

    const action = el("select", { "aria-label": `Hotkey ${i + 1} action (${where})` }, ...ACTIONS.map(([v, t]) =>
      // There's nothing to play until a sound is added.
      el("option", { value: v, textContent: t, disabled: v === "play_sound" && !state.sounds.length && row.action !== v })));
    action.value = row.action;
    action.addEventListener("change", () => updateRow(key, { action: action.value, target: targetFor(action.value, row.target) }));

    const target = el("select", {
      "aria-label": `Hotkey ${i + 1} ${row.action === "play_sound" ? "sound" : "channel"}`,
      disabled: row.action === "stop_sounds",
    }, ...targets.map(([v, t]) => el("option", { value: v, textContent: t })));
    target.value = row.target;
    target.addEventListener("change", () => updateRow(key, { target: target.value }));

    const del = el("button", {
      className: "ghost icon danger",
      textContent: "✕",
      title: "Delete hotkey",
      "aria-label": `Delete hotkey ${i + 1}${row.keys ? ` (${row.keys})` : ""}`,
    });
    del.addEventListener("click", () => saveHotkeys(hotkeyRows().filter((h) => rowKey(h) !== key)));

    return el("div", { className: "hotkey-row" }, keys, action, target, del);
  }));
  if (focusTarget) {
    focusTarget.focus();
    pendingFocus = null;
  }
}

$("#add-hotkey").addEventListener("click", () => {
  if (!state) return;
  const row = { id: "", _draft: `draft${++draftCounter}`, keys: "", action: "push_to_talk", target: state.sources[0] ? state.sources[0].id : "master" };
  hotkeyDrafts.push(row);
  renderHotkeys(true);
  const buttons = document.querySelectorAll("#hotkeys .keys");
  startRecording(row._draft, buttons[buttons.length - 1]);
});

// ---------------------------------------------------------------- settings

$("#autostart").addEventListener("change", (e) => {
  api("PUT", "/api/settings", { autostart: e.target.checked }).catch((err) => {
    e.target.checked = !e.target.checked;
    toast(err.message);
  });
});

$("#quit").addEventListener("click", () => {
  if (!confirm("Stop the mixer? Apps using the virtual mic will hear silence.")) return;
  attempt(api("POST", "/api/quit").then(() => {
    document.body.replaceChildren(el("main", {}, el("p", { className: "empty", textContent: "Virtual Audio Mixer has stopped. You can close this tab." })));
  }));
});

loadOutputs();
connect();
