"use strict";

const $ = (sel, root = document) => root.querySelector(sel);
// The dB buttons beside volume sliders, kept so they can be updated while
// editDb has swapped one out of the page for a text field.
const dbButtons = {
  hear: $("#hear-gain-value"),
  sound: $("#sound-gain-value"),
  music: $("#hear-music-value"),
};

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

// Icons are drawn here rather than using emoji such as 🎚, which some Windows
// fonts don't have (they show as a box).
const SVG = 'viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"';
const ICONS = {
  sliders: `<svg ${SVG}><path d="M6 4v16M12 4v16M18 4v16" stroke-width="1.5" opacity=".55"/><path d="M4 15h4M10 8h4M16 12h4" stroke-width="3"/></svg>`,
  window: `<svg ${SVG}><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/></svg>`,
  mic: `<svg ${SVG}><rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3M9 21h6"/></svg>`,
  speaker: `<svg ${SVG}><path d="M4 9v6h4l5 4V5L8 9z"/><path d="M16.5 9a4 4 0 0 1 0 6M19 6.5a8 8 0 0 1 0 11"/></svg>`,
  cog: `<svg ${SVG} stroke-width="1.75"><path d="M19.21 9.59 L19.51 10.81 L22.13 10.85 L22.13 13.15 L19.51 13.19 L19.21 14.41 L18.80 15.39 L18.15 16.47 L19.98 18.35 L18.35 19.98 L16.47 18.15 L15.39 18.80 L14.41 19.21 L13.19 19.51 L13.15 22.13 L10.85 22.13 L10.81 19.51 L9.59 19.21 L8.61 18.80 L7.53 18.15 L5.65 19.98 L4.02 18.35 L5.85 16.47 L5.20 15.39 L4.79 14.41 L4.49 13.19 L1.87 13.15 L1.87 10.85 L4.49 10.81 L4.79 9.59 L5.20 8.61 L5.85 7.53 L4.02 5.65 L5.65 4.02 L7.53 5.85 L8.61 5.20 L9.59 4.79 L10.81 4.49 L10.85 1.87 L13.15 1.87 L13.19 4.49 L14.41 4.79 L15.39 5.20 L16.47 5.85 L18.35 4.02 L19.98 5.65 L18.15 7.53 L18.80 8.61Z"/><circle cx="12" cy="12" r="3"/></svg>`,
  refresh: `<svg ${SVG}><path d="M20 12a8 8 0 1 1-2.34-5.66"/><path d="M20 4v5h-5"/></svg>`,
  plus: `<svg ${SVG}><path d="M12 5v14M5 12h14"/></svg>`,
  play: `<svg ${SVG}><path d="M8 5.5v13l10.5-6.5z" fill="currentColor" stroke-width="1.5"/></svg>`,
  pause: `<svg ${SVG}><rect x="6.5" y="5" width="4" height="14" rx="1" fill="currentColor" stroke="none"/><rect x="13.5" y="5" width="4" height="14" rx="1" fill="currentColor" stroke="none"/></svg>`,
  next: `<svg ${SVG}><path d="M5.5 5.5v13l9.5-6.5z" fill="currentColor" stroke-width="1.5"/><path d="M18 5.5v13" stroke-width="2.5"/></svg>`,
  previous: `<svg ${SVG}><path d="M18.5 5.5v13L9 12z" fill="currentColor" stroke-width="1.5"/><path d="M6 5.5v13" stroke-width="2.5"/></svg>`,
  shuffle: `<svg ${SVG}><path d="M3 7h3.5c4.5 0 6.5 10 11 10H21M3 17h3.5c1.7 0 2.9-1.4 4-3.2M14.5 9.6C15.5 8 16.6 7 18 7h3"/><path d="M18 4l3 3-3 3M18 14l3 3-3 3"/></svg>`,
  repeat: `<svg ${SVG}><path d="M4 11V9.5A3.5 3.5 0 0 1 7.5 6H20"/><path d="M17 3l3 3-3 3"/><path d="M20 13v1.5a3.5 3.5 0 0 1-3.5 3.5H4"/><path d="M7 21l-3-3 3-3"/></svg>`,
  repeatOne: `<svg ${SVG}><path d="M4 11V9.5A3.5 3.5 0 0 1 7.5 6H20"/><path d="M17 3l3 3-3 3"/><path d="M20 13v1.5a3.5 3.5 0 0 1-3.5 3.5H4"/><path d="M7 21l-3-3 3-3"/><path d="M11 10.5l1.5-1v5" stroke-width="1.8"/></svg>`,
  music: `<svg ${SVG}><path d="M9 18V5.5l11-2V16"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="17.5" cy="16" r="2.5"/></svg>`,
  stop: `<svg ${SVG}><rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" stroke="none"/></svg>`,
  headphones: `<svg ${SVG}><path d="M4 16v-4a8 8 0 0 1 16 0v4"/><rect x="3" y="14" width="4.5" height="7" rx="1.5"/><rect x="16.5" y="14" width="4.5" height="7" rx="1.5"/></svg>`,
  close: `<svg ${SVG}><path d="M6 6l12 12M18 6L6 18"/></svg>`,
  sun: `<svg ${SVG}><circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4"/></svg>`,
  moon: `<svg ${SVG}><path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/></svg>`,
  themeAuto: `<svg ${SVG}><circle cx="12" cy="12" r="8.5"/><path d="M12 3.5a8.5 8.5 0 0 1 0 17z" fill="currentColor" stroke="none"/></svg>`,
  info: `<svg ${SVG}><circle cx="12" cy="12" r="9"/><path d="M12 11v6"/><circle cx="12" cy="7.5" r="1.2" fill="currentColor" stroke="none"/></svg>`,
  grip: `<svg ${SVG}><g fill="currentColor" stroke="none"><circle cx="9" cy="6" r="1.6"/><circle cx="15" cy="6" r="1.6"/><circle cx="9" cy="12" r="1.6"/><circle cx="15" cy="12" r="1.6"/><circle cx="9" cy="18" r="1.6"/><circle cx="15" cy="18" r="1.6"/></g></svg>`,
};

/** Draws the icon named by `data-icon` into `root` and each such element
 * inside it. Every icon button is `<button class="icon" data-icon="…">`,
 * sized and shaped by the one `button.icon` style. */
function drawIcons(root) {
  const own = root.matches && root.matches("[data-icon]") ? [root] : [];
  for (const node of [...own, ...root.querySelectorAll("[data-icon]")]) {
    if (node.dataset.drawn !== node.dataset.icon) {
      node.innerHTML = ICONS[node.dataset.icon];
      node.dataset.drawn = node.dataset.icon;
    }
  }
}

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
  if (props["data-icon"]) drawIcons(node);
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
      updateMusicPosition(msg.music_position_ms);
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
  if (!themeSaves && state.theme !== appliedTheme) applyTheme(state.theme);
  $("#demo-banner").hidden = state.platform_supported;
  $("#autostart-wrap").hidden = !state.platform_supported;
  $("#autostart").checked = state.autostart;
  $("#autostart-admin-wrap").hidden = !state.platform_supported || !state.autostart;
  if (!adminPending) $("#autostart-admin").checked = state.autostart_admin;
  $("#elevation-note").textContent = state.elevated
    ? "The mixer is running as administrator, so hotkeys also work while apps running as administrator are focused."
    : "Hotkeys can't see keys while an app running as administrator (some games) is focused. Turn on Start with Windows → as Administrator, or run the mixer as administrator, to fix that.";
  renderOutput();
  renderStrips();
  renderMicDialog();
  renderListen();
  renderSounds();
  renderHear();
  renderMusic();
  renderHotkeys();
  renderSetup();
}

// ---------------------------------------------------------------- output

let devices = { inputs: [], outputs: [] };
let devicesLoaded = false;

/** Loads the device lists (for Output and the setup check). `quiet` skips
 * the error toast for background refreshes. */
async function loadDevices(quiet = false) {
  try {
    devices = await api("GET", "/api/devices");
    outputs = devices.outputs;
    devicesLoaded = true;
  } catch (e) {
    if (!quiet) toast(e.message);
  }
  renderOutput();
  renderSetup();
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
}

$("#output").addEventListener("change", (e) => {
  const id = e.target.value;
  const dev = outputs.find((d) => d.id === id) || (state.output && state.output.id === id ? state.output : null);
  attempt(api("PUT", "/api/output", { device: dev ? { id: dev.id, name: dev.name } : null }));
  e.target.blur();
});
$("#refresh-outputs").addEventListener("click", () => loadDevices());

// ---------------------------------------------------------------- strips

function channelList() {
  const list = [{
    id: "master",
    name: "Master",
    detail: "Final mix sent to the output",
    icon: "sliders",
    gain_db: state.master.gain_db,
    muted: state.master.muted,
    status: null,
  }];
  for (const s of state.sources) {
    const [detail, icon] = {
      app: [s.exe, "window"],
      device: [s.device ? s.device.name : "Default microphone (follows Windows)", "mic"],
      soundboard: ["Sounds from the soundboard below", "speaker"],
      music: ["Music from the player below", "music"],
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
    fill: $(".meter-fill", node),
    gain: $(".gain", node),
    gainValue: $(".gain-value", node),
    mute: $(".mute", node),
    remove: $(".remove", node),
    handle: $(".handle", node),
    listen: $(".listen", node),
    monitorStatus: $(".monitor-status", node),
    settings: $(".settings", node),
    dragging: false,
  };
  const id = ch.id;
  node.dataset.id = id;
  parts.icon.dataset.icon = ch.icon;
  drawIcons(node);
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
  if (ch.kind === "device") {
    parts.settings.hidden = false;
    parts.settings.addEventListener("click", () => openMicDialog(id));
  }

  parts.gainValue.addEventListener("click", () =>
    editDb(parts.gain, parts.gainValue, `${parts.name.value} volume`, (db) => attempt(api("PATCH", `/api/channels/${id}`, { gain_db: db }))));

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

/** Parses a typed gain: accepts "−6", "-6,5", "-6 dB". Returns null if invalid. */
function parseDb(text) {
  const normalised = text.trim().replace(/\s*db$/i, "").replace("\u2212", "-").replace(",", ".");
  if (normalised === "") return null;
  const db = Number(normalised);
  return Number.isFinite(db) ? Math.max(-60, Math.min(12, Math.round(db * 2) / 2)) : null;
}

/** Swaps a volume slider's dB button for a text field; Enter or blur
 *  applies (calling `apply` with the new dB), Esc cancels. */
function editDb(range, button, label, apply) {
  const opened = range.value;
  const input = el("input", {
    type: "text",
    inputMode: "decimal",
    className: "gain-input",
    value: opened,
    "aria-label": `${label} in dB`,
  });
  button.replaceWith(input);
  input.focus();
  input.select();
  let done = false;
  const finish = (save, refocus) => {
    if (done) return;
    done = true;
    const db = parseDb(input.value);
    // Only send a change the user made: blurring an untouched field must not
    // overwrite a newer gain that arrived meanwhile.
    if (save && db !== null && input.value !== opened) {
      range.value = db;
      button.textContent = formatDb(db);
      range.setAttribute("aria-valuetext", formatDb(db));
      apply(db);
    }
    input.replaceWith(button);
    // Refocus only for keyboard exits; a click or Tab elsewhere keeps its target.
    if (refocus) button.focus();
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
  $(".label", listen).textContent = listening ? "Listening" : "Listen";
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
    // Master has no status (its element is removed); new cards aren't in
    // the page yet on their first render, so don't test for that.
    if (ch.status) {
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
    if (ch.kind === "device") {
      // The cog lights up while any processing is on.
      const on = [ch.noise_suppression && "noise suppression", ch.noise_gate && `${ch.gate_mode === "auto" ? "automatic" : "manual"} noise gate`].filter(Boolean);
      const summary = `Mic processing: ${on.length ? on.join(", ") : "off"}`;
      p.settings.classList.toggle("on", on.length > 0);
      p.settings.title = summary;
      p.settings.setAttribute("aria-label", `${ch.name} ${summary.toLowerCase()}`);
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
/** Shows a level by clipping the meter's colour bar (clipping, not covering
 *  it, so no colour peeks out at the rounded ends). */
function setMeter(fill, peak) {
  const db = peak > 0 ? 20 * Math.log10(peak) : -Infinity;
  const pct = Math.max(0, Math.min(1, (db + 60) / 60));
  fill.style.clipPath = `inset(0 ${(1 - pct) * 100}% 0 0)`;
}

function updateMeters(meters) {
  for (const [id, m] of Object.entries(meters)) {
    const strip = strips.get(id);
    if (!strip) continue;
    setMeter(strip.parts.fill, m.peak);
    strip.el.classList.toggle("gated", !m.open);
  }
  const now = Date.now();
  for (const [id, m] of Object.entries(meters)) {
    if (m.mic_input > SILENT) heardAt.set(id, now);
  }
  const mic = micDialogId && meters[micDialogId];
  if (mic && mic.mic_level !== undefined) {
    setMeter($("#mic-dialog .mic-meter .meter-fill"), mic.mic_level);
    const state = $("#mic-gate-state");
    state.textContent = mic.mic_gate_open ? "Open" : "Closed";
    state.className = `gate-state ${mic.mic_gate_open ? "open" : "closed"}`;
  }
}

drawIcons(document);

// ---------------------------------------------------------------- dialogs

// Clicking the dimmed backdrop outside a dialog closes it. The dialog's
// content fills it edge to edge, so a click on the <dialog> element itself
// is a click on the backdrop. Only when the press also started there, so
// selecting text and releasing outside doesn't close it.
for (const dialog of document.querySelectorAll("dialog")) {
  let pressedOutside = false;
  dialog.addEventListener("pointerdown", (e) => (pressedOutside = e.target === dialog));
  dialog.addEventListener("click", (e) => {
    if (e.target === dialog && pressedOutside) dialog.close();
    pressedOutside = false;
  });
}

// ---------------------------------------------------------------- info and help

/** The (i) buttons next to panel headings toggle a short explanation. */
for (const btn of document.querySelectorAll(".info")) {
  const pop = document.getElementById(btn.getAttribute("aria-controls"));
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    const open = pop.hidden;
    closeInfo();
    pop.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
  });
}

function closeInfo(except) {
  for (const btn of document.querySelectorAll(".info[aria-expanded=true]")) {
    const pop = document.getElementById(btn.getAttribute("aria-controls"));
    if (pop === except) continue;
    pop.hidden = true;
    btn.setAttribute("aria-expanded", "false");
  }
}

// Clicking elsewhere or pressing Esc closes an open explanation.
document.addEventListener("click", (e) => {
  if (!e.target.closest(".info-pop")) closeInfo();
});
document.addEventListener("keydown", (e) => {
  const open = document.querySelector(".info[aria-expanded=true]");
  if (e.key === "Escape" && open) {
    closeInfo();
    open.focus();
  }
});

function openHelp(section) {
  closeInfo();
  for (const d of document.querySelectorAll("dialog[open]")) if (d.id !== "help-dialog") d.close();
  const dialog = $("#help-dialog");
  if (!dialog.open) dialog.showModal();
  const target = section && document.getElementById(section);
  if (target) target.scrollIntoView({ block: "start" });
}

$("#help-btn").addEventListener("click", () => openHelp());
$("#help-close").addEventListener("click", () => $("#help-dialog").close());
// "More in Help" links, and links between help sections.
document.addEventListener("click", (e) => {
  const link = e.target.closest("[data-help], #help-dialog a[href^='#help-']");
  if (!link) return;
  e.preventDefault();
  openHelp(link.dataset.help || link.getAttribute("href").slice(1));
});

// ---------------------------------------------------------------- setup check

/** Below this (dBFS) the raw input counts as silence: a muted or dead mic
 * gives digital zeros, while a live one always picks up some room noise. */
const SILENT = 10 ** (-90 / 20);
const QUIET_MS = 10000;
const heardAt = new Map(); // mic source id -> when it last picked up sound
const runningSince = new Map(); // mic source id -> when we first saw it running

const isCable = (name) => /cable|voicemeeter|virtual/i.test(name);
/** The recording end of the cable `output` plays into ("X Output" for "X Input"). */
const feedsBack = (input, output) => /output/i.test(input) && input.toLowerCase().replace("output", "input") === output.toLowerCase();

/** Everything worth checking, each { level: ok|info|pending|warn|error,
 * title, detail, fix?: { label, run }, help? }. */
function setupChecks() {
  if (!state || !devicesLoaded) return [];
  const checks = [];
  const add = (level, title, detail, extra = {}) => checks.push({ level, title, detail, ...extra });
  const now = Date.now();

  const cables = devices.outputs.filter((d) => isCable(d.name));
  const cable = cables.find((d) => /cable input/i.test(d.name)) || cables[0];
  const useCable = cable && {
    label: `Send the mix to ${cable.name}`,
    run: () => api("PUT", "/api/output", { device: { id: cable.id, name: cable.name } }),
  };
  if (cable) add("ok", "Virtual cable installed", cable.name);
  else add("error", "No virtual cable found", "Install one, such as the free VB-Audio Virtual Cable, then restart your PC.", { help: "help-start" });

  const out = state.output;
  if (!out) add("error", "The mix isn't going anywhere", "Choose where to send it in Output.", { fix: useCable, help: "help-start" });
  else if (state.output_status.state === "error") add("error", `Can't play the mix on ${out.name}`, state.output_status.message, { help: "help-trouble" });
  else if (!isCable(out.name)) add("warn", `The mix goes to ${out.name}`, "That isn't a virtual cable, so other apps can't use the mix as a microphone.", { fix: useCable, help: "help-start" });
  else add("ok", "The mix goes to the virtual cable", out.name);

  const defOut = devices.outputs.find((d) => d.is_default);
  if (defOut && out && defOut.id === out.id) {
    add("warn", "Your Windows default output is the cable", "Everything your PC plays goes into the mic, and you won't hear it. Set your speakers or headphones as the default output in Windows' sound settings.", { help: "help-trouble" });
  }
  const defIn = devices.inputs.find((d) => d.is_default);
  if (defIn && out && feedsBack(defIn.name, out.name)) {
    add("warn", "Your Windows default microphone is the cable", `“${defIn.name}” records the mix itself, so apps using the default microphone hear the mix, and “Default microphone” sources can't hear you. Set your real microphone as the default in Windows' sound settings.`, { help: "help-trouble" });
  }

  const mics = state.sources.filter((s) => s.kind === "device");
  if (!mics.length) {
    add("warn", "No microphone in the mix", "Add your microphone so others hear you.", {
      fix: { label: "Add your default microphone", run: () => api("POST", "/api/sources", { kind: "device", device: null }) },
    });
  }
  for (const m of mics) {
    if (m.status.state !== "running") runningSince.delete(m.id);
    else if (!runningSince.has(m.id)) runningSince.set(m.id, now);
    const since = runningSince.get(m.id);
    if (m.status.state === "error") add("error", `${m.name} isn't working`, m.status.message, { help: "help-trouble" });
    else if (!since) add("pending", `${m.name} is starting`, m.status.message);
    else if (now - (heardAt.get(m.id) || 0) < QUIET_MS) add("ok", `${m.name} is picking up sound`, m.status.message + (m.muted ? " (muted on its card)" : ""));
    else if (now - since < QUIET_MS) add("pending", `Listening to ${m.name}…`, "Say something.");
    else add("warn", `No sound from ${m.name}`, "It hasn't picked anything up for 10 seconds. Check it isn't muted in Windows or on the mic itself, and that it's the right device.", { help: "help-trouble" });
  }
  for (const s of state.sources) {
    if (s.kind !== "device" && s.status.state === "error") add("warn", `${s.name} isn't working`, s.status.message);
  }
  if (state.hear_status.state === "error") add("warn", "Hear sounds can't play", state.hear_status.message, { help: "help-trouble" });
  if (state.monitor.state === "error") add("warn", "Listen can't play", state.monitor.message, { help: "help-trouble" });
  if (state.music.error) add("warn", "A music track couldn't be played", state.music.error, { help: "help-music" });
  return checks;
}

const LEVELS = ["ok", "info", "pending", "warn", "error"];
let lastSetup = "";
let setupFixes = [];

function renderSetup() {
  const checks = setupChecks();
  const worst = checks.reduce((w, c) => (LEVELS.indexOf(c.level) > LEVELS.indexOf(w) ? c.level : w), "ok");
  const btn = $("#setup-btn");
  btn.dataset.level = checks.length ? worst : "";
  const problems = checks.filter((c) => c.level === "warn" || c.level === "error").length;
  btn.setAttribute("aria-label", `Setup check: ${problems ? `${problems} to look at` : "all good"}`);

  const errors = checks.filter((c) => c.level === "error");
  $("#setup-banner").hidden = !errors.length;
  if (errors.length) $("#setup-banner-text").textContent = `Setup: ${errors[0].title}.${errors.length > 1 ? ` (+${errors.length - 1} more)` : ""}`;

  // Only rebuild the list when it changed, so focus on its buttons stays.
  const signature = JSON.stringify(checks.map(({ fix, ...c }) => ({ ...c, fix: fix && fix.label })));
  if (signature === lastSetup) return;
  lastSetup = signature;
  setupFixes = checks.map((c) => c.fix);
  const order = (c) => -LEVELS.indexOf(c.level);
  const sorted = checks.map((c, i) => ({ c, i })).sort((a, b) => order(a.c) - order(b.c));
  $("#setup-list").replaceChildren(...sorted.map(({ c, i }) => {
    const actions = el("div", { className: "check-actions" });
    if (c.fix) {
      const fix = el("button", { type: "button", className: "primary", textContent: c.fix.label });
      fix.addEventListener("click", () => attempt(setupFixes[i].run().then(() => loadDevices(true))));
      actions.append(fix);
    }
    if (c.help) actions.append(el("button", { type: "button", className: "link", textContent: "How to fix", "data-help": c.help }));
    return el("li", { className: `check ${c.level}` },
      el("span", { className: "check-icon", "aria-hidden": "true" }),
      el("div", {}, el("b", { textContent: c.title }), el("p", { textContent: c.detail }), ...(actions.children.length ? [actions] : [])));
  }));
}

$("#setup-btn").addEventListener("click", () => {
  loadDevices(true);
  $("#setup-dialog").showModal();
});
$("#setup-banner-open").addEventListener("click", () => $("#setup-btn").click());
$("#setup-close").addEventListener("click", () => $("#setup-dialog").close());
// Pending checks (e.g. listening for the mic) move on with time.
setInterval(renderSetup, 1000);

// ---------------------------------------------------------------- mic processing

let micDialogId = null; // source whose mic processing dialog is open

function micSource() {
  return state && state.sources.find((s) => s.id === micDialogId);
}

function openMicDialog(id) {
  micDialogId = id;
  renderMicDialog();
  $("#mic-dialog").showModal();
}

function renderMicDialog() {
  if (!micDialogId) return;
  const src = micSource();
  if (!src) {
    $("#mic-dialog").close();
    return;
  }
  $("#mic-title").textContent = `Mic processing: ${src.name}`;
  $("#mic-suppress").checked = src.noise_suppression;
  $("#mic-gate").checked = src.noise_gate;
  $("#mic-gate-controls").hidden = !src.noise_gate;
  for (const r of document.querySelectorAll("#mic-dialog [name=gate-mode]")) r.checked = r.value === src.gate_mode;
  const manual = src.gate_mode === "manual";
  $("#mic-extra").hidden = manual;
  $("#mic-extra-strength").checked = src.extra_strength;
  $("#mic-keep").hidden = !src.extra_strength;
  $("#mic-keep-in-time").checked = src.keep_in_time;
  $("#mic-threshold").hidden = $("#mic-threshold-value").hidden = $("#mic-threshold-mark").hidden = !manual;
  $("#mic-gate-hint").textContent = manual
    ? "The meter shows your mic. Set the line just above where it sits while you're quiet."
    : "Opens when it hears speech that's louder than the background noise. If it misses quiet speech, use a level instead.";
  if (document.activeElement !== $("#mic-threshold")) showThreshold(src.gate_db);
}

function showThreshold(db) {
  const text = `${db < 0 ? "−" : ""}${Math.abs(db)} dB`;
  $("#mic-threshold").value = db;
  $("#mic-threshold-value").textContent = text;
  $("#mic-threshold").setAttribute("aria-valuetext", text);
  $("#mic-threshold-mark").style.left = `${((db + 60) / 60) * 100}%`;
}

const patchMic = (body) => attempt(api("PATCH", `/api/channels/${micDialogId}`, body));
$("#mic-suppress").addEventListener("change", (e) => patchMic({ noise_suppression: e.target.checked }));
$("#mic-gate").addEventListener("change", (e) => {
  $("#mic-gate-controls").hidden = !e.target.checked;
  patchMic({ noise_gate: e.target.checked });
});
for (const r of document.querySelectorAll("#mic-dialog [name=gate-mode]")) {
  r.addEventListener("change", () => patchMic({ gate_mode: r.value }));
}
$("#mic-extra-strength").addEventListener("change", (e) => {
  $("#mic-keep").hidden = !e.target.checked;
  patchMic({ extra_strength: e.target.checked });
});
$("#mic-keep-in-time").addEventListener("change", (e) => patchMic({ keep_in_time: e.target.checked }));
const sendThreshold = throttle((id, db) => attempt(api("PATCH", `/api/channels/${id}`, { gate_db: db })), 60);
$("#mic-threshold").addEventListener("input", (e) => {
  const db = Number(e.target.value);
  $("#mic-threshold").value = db;
  showThreshold(db);
  sendThreshold(micDialogId, db);
});
$("#mic-dialog").addEventListener("close", () => {
  const strip = strips.get(micDialogId);
  micDialogId = null;
  if (strip) strip.parts.settings.focus();
});

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
let uploading = null; // { queue, done, total } while files are being added
let editingSound = null; // id of the sound open in the edit dialog
let lastSoundsRender = "";
let lastHint = "";

const addBack = el("button", { type: "button", className: "link", textContent: "Add it back" });
addBack.addEventListener("click", () => attempt(api("POST", "/api/sources", { kind: "soundboard" })));

function renderSounds() {
  if (!state) return;
  const inMix = state.sources.some((s) => s.kind === "soundboard");
  let hint;
  if (uploading) hint = `Adding sounds… ${uploading.done} of ${uploading.total} done`;
  else if (!state.sounds.length) hint = "No sounds yet. Add some with + Add sounds, or drop audio files here.";
  else if (!inMix) hint = "The soundboard isn't in the mix, so nobody hears it. ";
  else hint = "";
  // Only touch the hint when it changes, so a focused "Add it back" survives
  // unrelated state updates.
  if (hint !== lastHint) {
    lastHint = hint;
    $("#sb-hint").hidden = !hint;
    $("#sb-hint").replaceChildren(hint, ...(!uploading && state.sounds.length && !inMix ? [addBack] : []));
  }

  const signature = JSON.stringify(state.sounds);
  // Mid-drag the tiles are being moved by hand; catch up when it ends.
  if (signature !== lastSoundsRender && !soundDragId) {
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
      if (!trimDrag) {
        trim = trimOf(sound);
        showTrim();
      }
    }
  }
}

/** Hear sounds: the soundboard on your own headphones, with its own volume. */
function renderHear() {
  const on = state.hear_sounds;
  $("#hear-sounds").setAttribute("aria-pressed", String(on));
  $("#hear-row").hidden = !on || !state.sounds.length;
  if (document.activeElement !== $("#hear-gain")) showHearGain(state.hear_sounds_db);
  const status = $("#hear-status");
  const h = state.hear_status;
  if (state.monitor.state !== "idle") {
    status.className = "status";
    status.textContent = "Paused while Listen is on (it includes the soundboard)";
  } else if (!state.sources.some((s) => s.kind === "soundboard")) {
    status.className = "status";
    status.textContent = "";
  } else {
    status.className = `status ${h.state}`;
    status.textContent = h.state === "running" ? `Playing on ${h.message}` : h.message;
  }
}

function showHearGain(db) {
  $("#hear-gain").value = db;
  dbButtons.hear.textContent = formatDb(db);
  $("#hear-gain").setAttribute("aria-valuetext", formatDb(db));
}

$("#hear-sounds").addEventListener("click", () => attempt(api("PATCH", "/api/soundboard", { hear_sounds: !state.hear_sounds })));
const sendHearGain = throttle((db) => attempt(api("PATCH", "/api/soundboard", { hear_sounds_db: db })), 60);
$("#hear-gain").addEventListener("input", () => {
  const db = Number($("#hear-gain").value);
  showHearGain(db);
  sendHearGain(db);
});
dbButtons.hear.addEventListener("click", (e) =>
  editDb($("#hear-gain"), e.currentTarget, "Hear sounds volume", (db) => attempt(api("PATCH", "/api/soundboard", { hear_sounds_db: db }))));

function soundTile(sound) {
  const ready = sound.state === "ready";
  const play = el("button", {
    type: "button",
    className: "play",
    disabled: !ready,
    title: ready ? `Play ${sound.name}` : sound.state === "error" ? sound.message : "Loading…",
  }, el("span", { className: "play-icon", "aria-hidden": "true", "data-icon": "play" }), el("span", { className: "sound-name", textContent: sound.name }));
  play.addEventListener("click", () => attempt(api("POST", `/api/sounds/${sound.id}/play`)));
  const edit = el("button", { type: "button", className: "edit icon", "data-icon": "cog", title: "Settings", "aria-label": `Settings for ${sound.name}` });
  edit.addEventListener("click", () => openSoundDialog(sound.id));
  const tile = el("div", { className: `sound ${sound.state}`, "data-id": sound.id, draggable: true }, play, edit);
  setUpSoundReorder(tile, sound.id);
  if (sound.state === "error") tile.append(el("span", { className: "sound-error", textContent: "Couldn't load this file", title: sound.message }));
  else if (sound.state === "loading") tile.append(el("span", { className: "sound-error", textContent: "Loading…" }));
  return tile;
}

// ---- reordering sounds: drag a tile, or Alt+←/→ on a focused one

let soundDragId = null;
let soundDropped = false;

function soundOrder() {
  return state.sounds.map((s) => s.id);
}

/** Applies a new order at once and saves it, restoring on failure. */
function saveSoundOrder(ids) {
  if (JSON.stringify(ids) === JSON.stringify(soundOrder())) return;
  const previous = state.sounds;
  state.sounds = ids.map((id) => previous.find((s) => s.id === id));
  renderSounds();
  api("PUT", "/api/sounds/order", { ids }).catch((e) => {
    state.sounds = previous;
    renderSounds();
    toast(e.message);
  });
}

function setUpSoundReorder(tile, id) {
  tile.addEventListener("dragstart", (e) => {
    soundDragId = id;
    soundDropped = false;
    tile.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("application/x-sound", id);
  });
  tile.addEventListener("dragend", () => {
    tile.classList.remove("dragging");
    soundDragId = null;
    if (soundDropped) saveSoundOrder([...document.querySelectorAll("#sounds .sound")].map((n) => n.dataset.id));
    else {
      lastSoundsRender = ""; // cancelled: put the tiles back
      renderSounds();
    }
  });
  tile.addEventListener("keydown", (e) => {
    const step = e.altKey && { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const ids = soundOrder();
    const from = ids.indexOf(id);
    const to = from + step;
    if (to < 0 || to >= ids.length) return;
    [ids[from], ids[to]] = [ids[to], ids[from]];
    saveSoundOrder(ids);
  });
}

$("#sounds").addEventListener("dragover", (e) => {
  if (!soundDragId) return;
  e.preventDefault();
  const dragged = document.querySelector(`#sounds .sound[data-id="${CSS.escape(soundDragId)}"]`);
  const target = e.target.closest(".sound");
  if (!dragged || !target || target === dragged) return;
  // Towards the end lands after the target, towards the start before it.
  const tiles = [...target.parentNode.children];
  const after = tiles.indexOf(dragged) < tiles.indexOf(target);
  target.parentNode.insertBefore(dragged, after ? target.nextSibling : target);
});
$("#sounds").addEventListener("drop", (e) => {
  if (!soundDragId) return;
  e.preventDefault();
  soundDropped = true;
});

function updatePlaying(ids) {
  playing = new Set(ids);
  for (const tile of document.querySelectorAll("#sounds .sound")) {
    const on = playing.has(tile.dataset.id);
    if (tile.classList.contains("playing") === on) continue;
    tile.classList.toggle("playing", on);
    const play = $(".play", tile);
    if (on) play.setAttribute("aria-description", "Playing");
    else play.removeAttribute("aria-description");
  }
  $("#stop-sounds").disabled = playing.size === 0;
}

/** Uploads files one at a time. Files added meanwhile join the queue. */
async function addSounds(files) {
  const list = [...files];
  if (!list.length) return;
  if (uploading) {
    uploading.queue.push(...list);
    uploading.total += list.length;
    renderSounds();
    return;
  }
  uploading = { queue: list, done: 0, total: list.length };
  renderSounds();
  const failures = [];
  while (uploading.queue.length) {
    const file = uploading.queue.shift();
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
  if (failures.length) {
    const shown = failures.slice(0, 3);
    if (failures.length > 3) shown.push(`…and ${failures.length - 3} more`);
    toast(shown.join("\n"));
  }
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
  dbButtons.sound.textContent = formatDb(db);
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
  loadWaveform(id);
}

function saveSoundName() {
  const sound = state && state.sounds.find((s) => s.id === editingSound);
  const name = $("#sound-name").value.trim();
  if (sound && name && name !== sound.name) attempt(api("PATCH", `/api/sounds/${sound.id}`, { name }));
}

const sendSoundGain = throttle((id, db) => attempt(api("PATCH", `/api/sounds/${id}`, { gain_db: db })), 60);
$("#sound-gain").addEventListener("input", () => {
  const db = Number($("#sound-gain").value);
  dbButtons.sound.textContent = formatDb(db);
  $("#sound-gain").setAttribute("aria-valuetext", formatDb(db));
  sendSoundGain(editingSound, db);
});
dbButtons.sound.addEventListener("click", (e) =>
  editDb($("#sound-gain"), e.currentTarget, "Sound volume", (db) => attempt(api("PATCH", `/api/sounds/${editingSound}`, { gain_db: db }))));
$("#sound-name").addEventListener("change", saveSoundName);
$("#sound-preview").addEventListener("click", () => attempt(api("POST", `/api/sounds/${editingSound}/preview`)));
$("#sound-delete").addEventListener("click", () => {
  const sound = state.sounds.find((s) => s.id === editingSound);
  if (!sound || !confirm(`Delete “${sound.name}” from the soundboard?`)) return;
  editingSound = null;
  $("#sound-dialog").close();
  attempt(api("DELETE", `/api/sounds/${sound.id}`));
});
$("#sound-dialog").addEventListener("close", () => {
  saveSoundName();
  // The tiles may have been rebuilt while the dialog was open, so the edit
  // button it came from can be gone: return focus to its replacement.
  const edit = editingSound && document.querySelector(`#sounds .sound[data-id="${CSS.escape(editingSound)}"] .edit`);
  (edit || $("#add-sound")).focus();
  editingSound = null;
  // Closed mid-drag (Esc): the release must not save to a closed dialog.
  trimDrag = null;
});

// ---- trim editor: the waveform with draggable edges, plus typed times

let wave = null; // { duration_ms, peaks, silence_start_ms, silence_end_ms }
let trim = null; // { start, end, fadeIn, fadeOut } in ms, as shown
let trimDrag = null; // "start" or "end" while dragging an edge

function trimOf(sound) {
  const duration = sound.duration_ms || (wave && wave.duration_ms) || 0;
  return { start: sound.start_ms, end: sound.end_ms ?? duration, fadeIn: sound.fade_in_ms, fadeOut: sound.fade_out_ms };
}

async function loadWaveform(id) {
  wave = null;
  const sound = state.sounds.find((s) => s.id === id);
  trim = sound ? trimOf(sound) : null;
  $("#trim").hidden = !sound || sound.state !== "ready";
  showTrim();
  if ($("#trim").hidden) return;
  try {
    const w = await api("GET", `/api/sounds/${id}/waveform`);
    if (editingSound !== id) return;
    wave = w;
    showTrim();
  } catch (e) {
    toast(e.message);
  }
}

const secs = (ms) => (ms / 1000).toFixed(2);

function showTrim() {
  if (!trim) return;
  for (const [sel, ms] of [["#trim-start", trim.start], ["#trim-end", trim.end], ["#fade-in", trim.fadeIn], ["#fade-out", trim.fadeOut]]) {
    if (document.activeElement !== $(sel)) $(sel).value = secs(ms);
  }
  if (wave) {
    $("#trim-start").max = $("#trim-end").max = secs(wave.duration_ms);
    $("#trim-silence").disabled = wave.silence_start_ms <= trim.start && wave.silence_end_ms >= trim.end;
    $("#trim-reset").disabled = !trim.start && trim.end >= wave.duration_ms && !trim.fadeIn && !trim.fadeOut;
  }
  drawWave();
}

function drawWave() {
  const canvas = $("#trim-wave");
  if (!wave || !trim || !canvas.isConnected) return;
  const scale = window.devicePixelRatio || 1;
  const w = canvas.clientWidth * scale, h = canvas.clientHeight * scale;
  if (!w) return;
  canvas.width = w;
  canvas.height = h;
  const g = canvas.getContext("2d");
  const css = getComputedStyle(document.documentElement);
  const accent = css.getPropertyValue("--accent").trim();
  const muted = css.getPropertyValue("--muted").trim();
  const x = (ms) => (ms / wave.duration_ms) * w;
  const [x0, x1] = [x(trim.start), x(trim.end)];
  // Selected part highlighted, the rest dimmed.
  g.fillStyle = accent;
  g.globalAlpha = 0.14;
  g.fillRect(x0, 0, x1 - x0, h);
  g.globalAlpha = 1;
  const bar = w / wave.peaks.length;
  // Scaled to the sound's own loudest point, so quiet sounds still show
  // their shape.
  const loudest = Math.max(...wave.peaks) || 1;
  wave.peaks.forEach((peak, i) => {
    const p = peak / loudest;
    const bx = i * bar;
    const inside = bx + bar / 2 >= x0 && bx + bar / 2 <= x1;
    g.fillStyle = inside ? accent : muted;
    g.globalAlpha = inside ? 1 : 0.45;
    const bh = Math.max(scale, p * (h - 8 * scale));
    g.fillRect(bx, (h - bh) / 2, Math.max(scale, bar - scale), bh);
  });
  g.globalAlpha = 1;
  // Fades: what they quieten is shaded, under a line from silent to full.
  const panel = css.getPropertyValue("--panel").trim();
  const text = css.getPropertyValue("--text").trim();
  for (const [edge, full] of [[x0, x(trim.start + trim.fadeIn)], [x1, x(trim.end - trim.fadeOut)]]) {
    if (Math.abs(full - edge) < 1) continue;
    g.beginPath();
    g.moveTo(edge, 0);
    g.lineTo(full, 0);
    g.lineTo(edge, h);
    g.closePath();
    g.fillStyle = panel;
    g.globalAlpha = 0.55;
    g.fill();
    g.beginPath();
    g.moveTo(edge, h);
    g.lineTo(full, 0);
    g.strokeStyle = text;
    g.lineWidth = 1.5 * scale;
    g.globalAlpha = 0.6;
    g.stroke();
  }
  g.globalAlpha = 1;
  // The draggable edges.
  g.fillStyle = accent;
  for (const ex of [x0, x1]) {
    g.fillRect(ex - scale, 0, 2 * scale, h);
    g.fillRect(ex - 4 * scale, 0, 8 * scale, 10 * scale);
    g.fillRect(ex - 4 * scale, h - 10 * scale, 8 * scale, 10 * scale);
  }
}

/** Sends trim changes (ms), with an end at the very end sent as null. */
function saveTrim(changes) {
  const id = editingSound;
  const body = {};
  if ("start" in changes) body.start_ms = Math.round(changes.start);
  if ("end" in changes) body.end_ms = wave && changes.end >= wave.duration_ms ? null : Math.round(changes.end);
  if ("fadeIn" in changes) body.fade_in_ms = Math.round(changes.fadeIn);
  if ("fadeOut" in changes) body.fade_out_ms = Math.round(changes.fadeOut);
  api("PATCH", `/api/sounds/${id}`, body).catch((e) => {
    toast(e.message);
    const sound = state.sounds.find((s) => s.id === id);
    if (sound && editingSound === id) {
      trim = trimOf(sound);
      showTrim();
    }
  });
}

const MIN_TRIM_MS = 20;

$("#trim-wave").addEventListener("pointerdown", (e) => {
  if (!wave || !trim || e.button !== 0) return;
  const rect = e.currentTarget.getBoundingClientRect();
  const ms = ((e.clientX - rect.left) / rect.width) * wave.duration_ms;
  // Grab whichever edge is nearer.
  trimDrag = Math.abs(ms - trim.start) <= Math.abs(ms - trim.end) ? "start" : "end";
  e.currentTarget.setPointerCapture(e.pointerId);
  moveEdge(e);
});
function moveEdge(e) {
  if (!trimDrag) return;
  const rect = $("#trim-wave").getBoundingClientRect();
  const ms = Math.max(0, Math.min(wave.duration_ms, ((e.clientX - rect.left) / rect.width) * wave.duration_ms));
  if (trimDrag === "start") trim.start = Math.min(ms, trim.end - MIN_TRIM_MS);
  else trim.end = Math.max(ms, trim.start + MIN_TRIM_MS);
  showTrim();
}
$("#trim-wave").addEventListener("pointermove", moveEdge);
for (const type of ["pointerup", "pointercancel"]) {
  $("#trim-wave").addEventListener(type, () => {
    if (!trimDrag) return;
    const edge = trimDrag;
    trimDrag = null;
    saveTrim({ [edge]: trim[edge] });
  });
}
for (const [sel, key] of [["#trim-start", "start"], ["#trim-end", "end"], ["#fade-in", "fadeIn"], ["#fade-out", "fadeOut"]]) {
  // Enter applies the time instead of submitting (closing) the dialog.
  $(sel).addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    e.target.dispatchEvent(new Event("change"));
  });
  $(sel).addEventListener("change", (e) => {
    const value = Number(e.target.value);
    if (!Number.isFinite(value) || value < 0) return showTrim();
    trim[key] = value * 1000;
    saveTrim({ [key]: trim[key] });
    showTrim();
  });
}
$("#trim-silence").addEventListener("click", () => {
  if (!wave) return;
  trim.start = wave.silence_start_ms;
  trim.end = wave.silence_end_ms;
  saveTrim({ start: trim.start, end: trim.end });
  showTrim();
  // The button disables itself once trimmed: keep focus in the dialog.
  if ($("#trim-silence").disabled) $("#trim-start").focus();
});
$("#trim-reset").addEventListener("click", () => {
  if (!wave) return;
  trim = { start: 0, end: wave.duration_ms, fadeIn: 0, fadeOut: 0 };
  saveTrim(trim);
  showTrim();
  $("#trim-start").focus(); // Reset disables itself
});
window.addEventListener("resize", drawWave);

// ---------------------------------------------------------------- music

let lastTracks = "";
let lastCurrent = null;
let lastMusicHint = null;
const musicBack = el("button", { type: "button", className: "link", textContent: "Add it back" });
musicBack.addEventListener("click", () => attempt(api("POST", "/api/sources", { kind: "music" })));
let seeking = false; // the user is dragging the seek bar

const clock = (ms) => {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

function renderMusic() {
  const m = state.music;
  const inMix = state.sources.some((s) => s.kind === "music");
  const hasTracks = m.tracks.length > 0;
  $("#player").hidden = !hasTracks;
  $("#hear-music").setAttribute("aria-pressed", String(m.hear));
  $("#hear-music-row").hidden = !m.hear || !hasTracks;
  if (document.activeElement !== $("#hear-music-gain")) {
    $("#hear-music-gain").value = m.hear_db;
    dbButtons.music.textContent = formatDb(m.hear_db);
  }

  let hint = "";
  if (!m.items.length) hint = "No music yet. Add a folder or files with Add music.";
  else if (!hasTracks) hint = "No audio files found in the folders and files you added.";
  else if (!inMix) hint = "The music isn't in the mix, so nobody hears it. ";
  else if (m.error) hint = `Couldn't play ${m.error}`;
  if (hint !== lastMusicHint) {
    lastMusicHint = hint;
    $("#music-hint").replaceChildren(hint, ...(hasTracks && !inMix ? [musicBack] : []));
  }
  $("#music-hint").hidden = !hint;
  $("#music-hint").classList.toggle("error", !!m.error && hasTracks && inMix);

  const current = m.current;
  const name = current !== null && current !== undefined ? m.tracks[current] : null;
  $("#now-title").textContent = m.title || name || "Nothing playing";
  $("#now-artist").textContent = m.artist || "";
  const toggle = $("#music-toggle");
  toggle.dataset.icon = m.playing ? "pause" : "play";
  toggle.setAttribute("aria-label", m.playing ? "Pause" : "Play");
  drawIcons(toggle);
  $("#music-shuffle").setAttribute("aria-pressed", String(m.shuffle));
  const repeat = $("#music-repeat");
  repeat.setAttribute("aria-pressed", String(m.repeat !== "off"));
  repeat.dataset.icon = m.repeat === "one" ? "repeatOne" : "repeat";
  repeat.setAttribute("aria-label", `Repeat: ${{ off: "off", all: "all tracks", one: "this track" }[m.repeat]}`);
  repeat.title = repeat.getAttribute("aria-label");
  drawIcons(repeat);
  $("#music-duck").checked = m.duck;
  const seek = $("#music-seek");
  seek.disabled = !m.duration_ms;
  if (m.duration_ms) seek.max = m.duration_ms;
  $("#music-dur").textContent = m.duration_ms ? clock(m.duration_ms) : "–:––";

  // The list: rebuilt only when the tracks change; the current one marked.
  const signature = JSON.stringify(m.tracks);
  const list = $("#tracks");
  if (signature !== lastTracks) {
    lastTracks = signature;
    // A removed track's × had focus: keep focus at that place in the list.
    const focused = document.activeElement?.closest?.("#tracks li");
    const index = focused ? [...list.children].indexOf(focused) : -1;
    list.replaceChildren(...m.tracks.map((t, i) => {
      const btn = el("button", { type: "button", className: "track" }, el("span", { className: "num", textContent: i + 1 }), el("span", { textContent: t }));
      btn.addEventListener("click", () => attempt(api("POST", "/api/music/control", { action: "play", track: i })));
      const remove = el("button", { type: "button", className: "ghost icon remove", "data-icon": "close", title: "Remove from the playlist (the file stays)", "aria-label": `Remove ${t} from the playlist` });
      remove.addEventListener("click", () => attempt(api("POST", "/api/music/tracks/remove", { track: i, name: t })));
      return el("li", {}, btn, remove);
    }));
    if (index >= 0) {
      const removes = list.querySelectorAll(".remove");
      (removes[Math.min(index, removes.length - 1)] || $("#music-add")).focus();
    }
  }
  [...list.children].forEach((li, i) => {
    const on = i === current;
    li.classList.toggle("current", on);
    if (on) li.firstChild.setAttribute("aria-current", "true");
    else li.firstChild.removeAttribute("aria-current");
  });
  // Keep a new current track in view, scrolling the list (not the page).
  if (current !== lastCurrent) {
    lastCurrent = current;
    const li = list.children[current];
    if (li) {
      const r = li.getBoundingClientRect(), box = list.getBoundingClientRect();
      if (r.top < box.top || r.bottom > box.bottom) list.scrollTop += r.top - box.top - (box.height - r.height) / 2;
    }
  }

  $("#music-clear").hidden = !m.items.length;
  const restore = $("#music-restore");
  restore.hidden = !m.removed;
  restore.textContent = `Restore ${m.removed} removed track${m.removed === 1 ? "" : "s"}`;
  if ($("#music-dialog").open) renderMusicItems();
}

function updateMusicPosition(ms) {
  if (ms === undefined || seeking || !state) return;
  $("#music-seek").value = ms;
  showSeek(ms);
}
function showSeek(ms) {
  $("#music-pos").textContent = clock(ms);
  const dur = state.music.duration_ms;
  $("#music-seek").setAttribute("aria-valuetext", dur ? `${clock(ms)} of ${clock(dur)}` : clock(ms));
}

const musicControl = (body) => attempt(api("POST", "/api/music/control", body));
$("#music-toggle").addEventListener("click", () => musicControl({ action: "toggle" }));
$("#music-next").addEventListener("click", () => musicControl({ action: "next" }));
$("#music-prev").addEventListener("click", () => musicControl({ action: "previous" }));
$("#music-shuffle").addEventListener("click", () => attempt(api("PATCH", "/api/music", { shuffle: !state.music.shuffle })));
$("#music-repeat").addEventListener("click", () => {
  const next = { off: "all", all: "one", one: "off" }[state.music.repeat];
  attempt(api("PATCH", "/api/music", { repeat: next }));
});
$("#music-duck").addEventListener("change", (e) => attempt(api("PATCH", "/api/music", { duck: e.target.checked })));
$("#music-seek").addEventListener("input", (e) => {
  seeking = true;
  showSeek(Number(e.target.value));
});
// The range is in milliseconds: arrow keys move 5 s and Page keys 30 s
// rather than a millisecond at a time.
$("#music-seek").addEventListener("keydown", (e) => {
  const step = { ArrowLeft: -5000, ArrowDown: -5000, ArrowRight: 5000, ArrowUp: 5000, PageDown: -30000, PageUp: 30000 }[e.key];
  const max = Number(e.target.max);
  let to;
  if (step) to = Number(e.target.value) + step;
  else if (e.key === "Home") to = 0;
  else if (e.key === "End") to = max - 1000;
  else return;
  e.preventDefault();
  to = Math.max(0, Math.min(max, to));
  e.target.value = to;
  showSeek(to);
  musicControl({ action: "seek", position_ms: to });
});
$("#music-seek").addEventListener("change", (e) => {
  seeking = false;
  musicControl({ action: "seek", position_ms: Number(e.target.value) });
});
$("#hear-music").addEventListener("click", () => attempt(api("PATCH", "/api/music", { hear: !state.music.hear })));
const sendHearMusic = throttle((db) => attempt(api("PATCH", "/api/music", { hear_db: db })), 60);
$("#hear-music-gain").addEventListener("input", (e) => {
  const db = Number(e.target.value);
  dbButtons.music.textContent = formatDb(db);
  e.target.setAttribute("aria-valuetext", formatDb(db));
  sendHearMusic(db);
});
dbButtons.music.addEventListener("click", (e) =>
  editDb($("#hear-music-gain"), e.currentTarget, "Hear music volume", (db) => attempt(api("PATCH", "/api/music", { hear_db: db }))));

// ---- the files and folders dialog

$("#music-clear").addEventListener("click", () => {
  if (!confirm("Clear the playlist? The files stay where they are.")) return;
  attempt(api("DELETE", "/api/music/items"));
  $("#music-add").focus(); // the button hides itself
});
$("#music-restore").addEventListener("click", () => {
  attempt(api("POST", "/api/music/tracks/restore"));
  $("#music-path").focus(); // the button hides itself
});

let lastItems = null; // rebuilt only when the items change, keeping focus
function renderMusicItems() {
  const items = state.music.items;
  const signature = JSON.stringify(items);
  if (signature === lastItems) return;
  lastItems = signature;
  // A removed item's button had focus: move it to the next one.
  const focused = document.activeElement?.closest?.("#music-items li");
  const index = focused ? [...$("#music-items").children].indexOf(focused) : -1;
  $("#music-items").replaceChildren(...(items.length ? items.map((item) => {
    const remove = el("button", { type: "button", className: "ghost icon", "data-icon": "close", title: "Remove", "aria-label": `Remove ${item.path}` });
    remove.addEventListener("click", () => attempt(api("DELETE", `/api/music/items/${item.id}`)));
    return el("li", {}, el("span", { className: "path", textContent: item.path }), remove);
  }) : [el("li", { className: "empty", textContent: "Nothing added yet." })]));
  if (index >= 0) {
    const buttons = $("#music-items").querySelectorAll("button");
    (buttons[Math.min(index, buttons.length - 1)] || $("#music-path")).focus();
  }
}

function musicError(message) {
  $("#music-error").textContent = message || "";
  $("#music-error").hidden = !message;
}

async function addMusicPaths(paths) {
  const failures = [];
  for (const path of paths) {
    try {
      await api("POST", "/api/music/items", { path });
    } catch (e) {
      failures.push(paths.length > 1 ? `${path}: ${e.message}` : e.message);
    }
  }
  musicError(failures.join("\n"));
  return failures.length === 0;
}

$("#music-add").addEventListener("click", () => {
  musicError("");
  $("#music-path").value = "";
  $("#music-browse").hidden = !state.platform_supported;
  lastItems = null;
  renderMusicItems();
  $("#music-dialog").showModal();
  $("#music-path").focus();
});
$("#music-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const path = $("#music-path").value.trim();
  if (!path) return;
  if (await addMusicPaths([path])) $("#music-path").value = "";
});
for (const [sel, folders] of [["#browse-folders", true], ["#browse-files", false]]) {
  $(sel).addEventListener("click", async (e) => {
    const btn = e.currentTarget;
    btn.disabled = true;
    musicError("");
    try {
      const { paths } = await api("POST", "/api/music/browse", { folders });
      if (paths.length) await addMusicPaths(paths);
    } catch (err) {
      musicError(err.message);
    } finally {
      btn.disabled = false;
    }
  });
}
$("#music-done").addEventListener("click", () => $("#music-dialog").close());

// ---------------------------------------------------------------- hotkeys

const ACTIONS = [
  ["toggle_mute", "Toggle mute"],
  ["push_to_talk", "Push to talk"],
  ["push_to_mute", "Push to mute"],
  ["play_sound", "Play sound"],
  ["stop_sounds", "Stop all sounds"],
  ["music_play_pause", "Music: play/pause"],
  ["music_next", "Music: next track"],
  ["music_previous", "Music: previous track"],
];

/** Actions that don't apply to a channel or sound. */
const NO_TARGET = { stop_sounds: "All sounds", music_play_pause: "Music", music_next: "Music", music_previous: "Music" };

/** [value, label] choices for a hotkey's target, which depend on its action. */
function targetsFor(action) {
  if (action === "play_sound") return state.sounds.map((s) => [s.id, s.name]);
  if (action in NO_TARGET) return [["", NO_TARGET[action]]];
  return [["master", "Master"], ...state.sources.map((s) => [s.id, s.name])];
}

/** Keeps `current` if it suits `action`, otherwise picks a sensible target. */
function targetFor(action, current) {
  const targets = targetsFor(action);
  if (targets.some(([v]) => v === current)) return current;
  if (action === "play_sound" || action in NO_TARGET) return targets.length ? targets[0][0] : "";
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
      disabled: row.action in NO_TARGET,
    }, ...targets.map(([v, t]) => el("option", { value: v, textContent: t })));
    target.value = row.target;
    target.addEventListener("change", () => updateRow(key, { target: target.value }));

    const del = el("button", {
      className: "ghost icon danger",
      "data-icon": "close",
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

// The theme: the server writes a chosen one into the page, so it's right
// from the first paint; this keeps it in step with the button and other tabs.
const THEMES = {
  system: { icon: "themeAuto", label: "Theme: automatic, like your system", next: "light" },
  light: { icon: "sun", label: "Theme: light", next: "dark" },
  dark: { icon: "moon", label: "Theme: dark", next: "system" },
};
let appliedTheme = document.documentElement.dataset.theme || "system";
// Clicks whose save hasn't answered: until then a broadcast still carries
// the old theme, and applying it would flick the page back.
let themeSaves = 0;

function applyTheme(theme) {
  const t = THEMES[theme] || THEMES.system;
  appliedTheme = theme;
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
  const btn = $("#theme-btn");
  btn.dataset.icon = t.icon;
  btn.setAttribute("aria-label", `${t.label}. Click for ${THEMES[t.next].label.slice(7)}`);
  btn.title = btn.getAttribute("aria-label");
  drawIcons(btn);
  drawWave(); // the waveform is drawn in the theme's colours
}

$("#theme-btn").addEventListener("click", () => {
  const next = THEMES[appliedTheme].next;
  applyTheme(next);
  themeSaves++;
  const save = api("PUT", "/api/settings", { theme: next });
  save.finally(() => themeSaves--).catch(() => {
    if (!themeSaves && state && state.theme !== appliedTheme) applyTheme(state.theme);
  });
  attempt(save);
});
// Following the system: redraw the waveform when it switches.
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", drawWave);
applyTheme(appliedTheme);

let adminPending = false; // waiting for the UAC prompt to be answered
let autostartSaving = false;

/** Saves Start with Windows settings. Changing "as administrator" (or
 * turning autostart off while it's on) makes Windows ask for permission,
 * and the request waits until that's answered. */
async function saveAutostart(body, input) {
  // One change at a time: an administrator change waits for the Windows
  // prompt. The checkboxes stay enabled (disabling them would drop focus).
  if (autostartSaving) {
    input.checked = !input.checked;
    return;
  }
  autostartSaving = true;
  const admin = body.autostart_admin !== undefined || (body.autostart === false && state.autostart_admin);
  if (admin) {
    adminPending = true;
    $("#autostart-admin-label").textContent = "as Administrator (answer the Windows prompt…)";
  }
  $(".topbar-actions").setAttribute("aria-busy", "true");
  try {
    await api("PUT", "/api/settings", body);
  } catch (err) {
    input.checked = !input.checked;
    toast(err.message);
  } finally {
    autostartSaving = false;
    $(".topbar-actions").removeAttribute("aria-busy");
    if (admin) {
      adminPending = false;
      $("#autostart-admin-label").textContent = "as Administrator";
    }
  }
}

$("#autostart").addEventListener("change", (e) => saveAutostart({ autostart: e.target.checked }, e.target));
$("#autostart-admin").addEventListener("change", (e) => saveAutostart({ autostart: true, autostart_admin: e.target.checked }, e.target));

$("#quit").addEventListener("click", () => {
  if (!confirm("Stop the mixer? Apps using the virtual mic will hear silence.")) return;
  attempt(api("POST", "/api/quit").then(() => {
    document.body.replaceChildren(el("main", {}, el("p", { className: "empty", textContent: "Virtual Audio Mixer has stopped. You can close this tab." })));
  }));
});

loadDevices();
// Pick up devices plugged in or defaults changed in Windows meanwhile.
setInterval(() => document.visibilityState === "visible" && loadDevices(true), 15000);
connect();
