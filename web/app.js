"use strict";

const $ = (sel, root = document) => root.querySelector(sel);

let state = null;
let outputs = [];
const strips = new Map(); // channel id -> { el, parts }
let hotkeyDrafts = []; // rows added in the UI that have no keys yet
let recording = null; // { key, cancel, stop }

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
  ws.onopen = () => {
    conn.textContent = "connected";
    conn.className = "conn up";
  };
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.type === "state") render(msg.state);
    else if (msg.type === "meters") updateMeters(msg.meters);
  };
  ws.onclose = () => {
    conn.textContent = "disconnected";
    conn.className = "conn down";
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
    const detail = s.kind === "app" ? s.exe : s.device ? s.device.name : "Default microphone (follows Windows)";
    list.push({ ...s, detail, icon: s.kind === "app" ? "🗔" : "🎙" });
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
    dragging: false,
  };
  const id = ch.id;
  if (id === "master") {
    node.classList.add("master");
    parts.name.readOnly = true;
    parts.remove.remove();
    parts.status.remove();
  }

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
    if (p.remove.isConnected) p.remove.setAttribute("aria-label", `Remove ${ch.name}`);
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
  // Only touch the DOM order when it changed, so focus is not lost.
  const same = ordered.length === container.children.length && ordered.every((n, i) => container.children[i] === n);
  if (!same) container.replaceChildren(...ordered);
}

function updateMeters(meters) {
  for (const [id, m] of Object.entries(meters)) {
    const strip = strips.get(id);
    if (!strip) continue;
    const db = m.peak > 0 ? 20 * Math.log10(m.peak) : -Infinity;
    const pct = Math.max(0, Math.min(1, (db + 60) / 60));
    strip.parts.mask.style.width = `${(1 - pct) * 100}%`;
    strip.el.classList.toggle("gated", !m.open);
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

// ---------------------------------------------------------------- hotkeys

const ACTIONS = [
  ["toggle_mute", "Toggle mute"],
  ["push_to_talk", "Push to talk"],
  ["push_to_mute", "Push to mute"],
];

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
  const targets = [["master", "Master"], ...state.sources.map((s) => [s.id, s.name])];
  const targetName = new Map(targets);
  // Drafts pointing at a source that has since been removed fall back to Master.
  for (const d of hotkeyDrafts) if (!targetName.has(d.target)) d.target = "master";
  const signature = JSON.stringify([hotkeyRows(), targets]);
  if (!force && signature === lastHotkeyRender) return;
  lastHotkeyRender = signature;

  const container = $("#hotkeys");
  const rows = hotkeyRows();
  if (!rows.length) {
    container.replaceChildren(el("p", { className: "empty", textContent: "No hotkeys yet. Add one to mute or push-to-talk any channel." }));
    return;
  }
  let focusTarget = null;
  container.replaceChildren(...rows.map((row, i) => {
    const key = rowKey(row);
    const where = targetName.get(row.target) || "Master";
    const keys = el("button", {
      className: "keys",
      textContent: row.keys || "Click to record",
      title: "Click to record a new key combination",
      "aria-label": `Hotkey ${i + 1} keys: ${row.keys || "none"}. Click to record`,
    });
    keys.addEventListener("click", () => startRecording(key, keys));
    if (pendingFocus && (pendingFocus === key || pendingFocus === row.keys)) focusTarget = keys;

    const action = el("select", { "aria-label": `Hotkey ${i + 1} action (${where})` }, ...ACTIONS.map(([v, t]) => el("option", { value: v, textContent: t })));
    action.value = row.action;
    action.addEventListener("change", () => updateRow(key, { action: action.value }));

    const target = el("select", { "aria-label": `Hotkey ${i + 1} channel` }, ...targets.map(([v, t]) => el("option", { value: v, textContent: t })));
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
