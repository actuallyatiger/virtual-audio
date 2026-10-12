//! Owns the configuration and all running audio threads.
//!
//! * One render thread pulls from the [`Mixer`] at the output device's pace
//!   (or a 10 ms timer when no device is available).
//! * One capture thread per source pushes into that source's ring buffer.
//! * A supervisor thread (re)starts captures when apps launch/exit or devices
//!   come and go, and persists configuration changes.

use crate::backend::{Backend, CaptureTarget, NullRender, RenderStream};
use crate::config::{
    Channel, Config, DeviceRef, Hotkey, SourceConfig, SourceKind, db_to_gain, new_id,
};
use crate::mixer::{ChannelControl, Mixer, push_frames, source_ring};
use anyhow::{anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const MASTER: &str = "master";
const RETRY_DELAY: Duration = Duration::from_secs(3);
/// Limits on API input, so a misbehaving local client can't bloat the
/// config or the 10 ms hotkey poll loop.
const MAX_NAME_CHARS: usize = 100;
const MAX_SOURCES: usize = 64;
const MAX_HOTKEYS: usize = 64;

fn check_len(what: &str, value: &str, max: usize) -> anyhow::Result<()> {
    if value.chars().count() > max {
        bail!("{what} is too long (max {max} characters)");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    /// "running", "waiting", "error" or "idle".
    pub state: &'static str,
    pub message: String,
}

impl Status {
    fn new(state: &'static str, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
        }
    }
}

type SharedStatus = Arc<Mutex<Status>>;

fn set_status(s: &SharedStatus, state: &'static str, message: impl Into<String>) {
    *s.lock().unwrap() = Status::new(state, message);
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn spawn(name: String, f: impl FnOnce(Arc<AtomicBool>) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || f(flag))
            .expect("spawn thread");
        Self { stop, thread }
    }

    fn finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Signals the thread to stop without waiting for it (it exits within
    /// ~100-200 ms, at its next wait timeout).
    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct SourceRuntime {
    ctrl: Arc<ChannelControl>,
    status: SharedStatus,
    capture: Option<Worker>,
    pid: Option<u32>,
    /// For "default microphone" sources: the device the capture opened.
    default_id: Option<String>,
    retry_at: Instant,
}

struct Inner {
    config: Config,
    sources: HashMap<String, SourceRuntime>,
    output: Option<Worker>,
    output_status: SharedStatus,
    dirty: bool,
    save_error: Option<String>,
}

pub struct Engine {
    backend: Arc<dyn Backend>,
    inner: Mutex<Inner>,
    master: Arc<ChannelControl>,
    mixer: Arc<Mutex<Mixer>>,
    changed: tokio::sync::watch::Sender<u64>,
    shutdown: AtomicBool,
    /// Serializes supervise() runs (the supervisor thread and add_source),
    /// which no longer hold `inner` throughout.
    supervising: Mutex<()>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ChannelPatch {
    pub name: Option<String>,
    pub gain_db: Option<f32>,
    pub muted: Option<bool>,
}

#[derive(Serialize)]
pub struct SourceView {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: SourceKind,
    #[serde(flatten)]
    pub channel: Channel,
    pub status: Status,
}

#[derive(Serialize)]
pub struct StateView {
    pub version: u64,
    pub output: Option<DeviceRef>,
    pub output_status: Status,
    pub master: Channel,
    pub sources: Vec<SourceView>,
    pub hotkeys: Vec<Hotkey>,
}

#[derive(Serialize)]
pub struct ChannelMeter {
    pub peak: f32,
    /// False while a push-to-talk/mute hotkey is silencing the channel.
    pub open: bool,
}

impl Engine {
    pub fn new(backend: Box<dyn Backend>, config: Config) -> Arc<Self> {
        let master = Arc::new(ChannelControl::new(
            db_to_gain(config.master.gain_db),
            config.master.muted,
        ));
        let mixer = Arc::new(Mutex::new(Mixer::new(master.clone())));
        let mut sources = HashMap::new();
        for s in &config.sources {
            sources.insert(s.id.clone(), SourceRuntime::new(&s.channel));
        }
        let engine = Arc::new(Self {
            backend: Arc::from(backend),
            inner: Mutex::new(Inner {
                config,
                sources,
                output: None,
                output_status: Arc::new(Mutex::new(Status::default())),
                dirty: false,
                save_error: None,
            }),
            master,
            mixer,
            changed: tokio::sync::watch::channel(0).0,
            shutdown: AtomicBool::new(false),
            supervising: Mutex::new(()),
        });
        engine.restart_output(&mut engine.inner.lock().unwrap());
        let weak = Arc::downgrade(&engine);
        std::thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || {
                while let Some(engine) = weak.upgrade() {
                    if engine.shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    engine.supervise();
                    drop(engine);
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .expect("spawn supervisor");
        engine
    }

    pub fn backend(&self) -> &dyn Backend {
        &*self.backend
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn version(&self) -> u64 {
        *self.changed.borrow()
    }

    fn bump(&self, inner: &mut Inner) {
        inner.dirty = true;
        self.changed.send_modify(|v| *v += 1);
    }

    /// Stops all audio and writes the configuration.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Wait out a supervise() in progress (it may be saving an older
        // snapshot outside `inner`); later ones see the flag and do nothing.
        // Then this is the one, final save.
        let _serial = self.supervising.lock().unwrap();
        let mut inner = self.inner.lock().unwrap();
        if let Some(w) = inner.output.take() {
            w.stop();
        }
        for rt in inner.sources.values_mut() {
            if let Some(w) = rt.capture.take() {
                w.stop();
            }
        }
        if let Err(e) = inner.config.save() {
            tracing::error!("saving config: {e}");
        }
        inner.dirty = false;
    }

    // -- queries ------------------------------------------------------------

    pub fn state(&self) -> StateView {
        let inner = self.inner.lock().unwrap();
        StateView {
            version: self.version(),
            output: inner.config.output.clone(),
            output_status: inner.output_status.lock().unwrap().clone(),
            master: inner.config.master.clone(),
            sources: inner
                .config
                .sources
                .iter()
                .map(|s| SourceView {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    kind: s.kind.clone(),
                    channel: s.channel.clone(),
                    status: inner.sources[&s.id].status.lock().unwrap().clone(),
                })
                .collect(),
            hotkeys: inner.config.hotkeys.clone(),
        }
    }

    pub fn meters(&self) -> HashMap<String, ChannelMeter> {
        let inner = self.inner.lock().unwrap();
        let mut out: HashMap<String, ChannelMeter> = inner
            .sources
            .iter()
            .map(|(id, rt)| {
                (
                    id.clone(),
                    ChannelMeter {
                        peak: rt.ctrl.peak(),
                        open: rt.ctrl.gate_open(),
                    },
                )
            })
            .collect();
        out.insert(
            MASTER.into(),
            ChannelMeter {
                peak: self.master.peak(),
                open: self.master.gate_open(),
            },
        );
        out
    }

    pub fn hotkeys(&self) -> Vec<Hotkey> {
        self.inner.lock().unwrap().config.hotkeys.clone()
    }

    /// Controls for every channel, keyed by "master" or source id.
    pub fn controls(&self) -> HashMap<String, Arc<ChannelControl>> {
        let inner = self.inner.lock().unwrap();
        let mut out: HashMap<_, _> = inner
            .sources
            .iter()
            .map(|(id, rt)| (id.clone(), rt.ctrl.clone()))
            .collect();
        out.insert(MASTER.into(), self.master.clone());
        out
    }

    // -- mutations ----------------------------------------------------------

    pub fn add_source(&self, name: Option<String>, kind: SourceKind) -> anyhow::Result<String> {
        let name = match (name.filter(|n| !n.trim().is_empty()), &kind) {
            (Some(n), _) => n.trim().to_owned(),
            (None, SourceKind::Device { device: Some(d) }) => d.name.clone(),
            (None, SourceKind::Device { device: None }) => "Default microphone".into(),
            (None, SourceKind::App { exe }) => display_exe(exe),
        };
        if let SourceKind::App { exe } = &kind {
            if exe.trim().is_empty() {
                bail!("executable name is required");
            }
            check_len("executable name", exe, 260)?;
        }
        check_len("name", &name, MAX_NAME_CHARS)?;
        let mut inner = self.inner.lock().unwrap();
        if inner.config.sources.len() >= MAX_SOURCES {
            bail!("too many sources (max {MAX_SOURCES})");
        }
        let id = new_id("s");
        let cfg = SourceConfig {
            id: id.clone(),
            name,
            kind,
            channel: Channel::default(),
        };
        inner
            .sources
            .insert(id.clone(), SourceRuntime::new(&cfg.channel));
        inner.config.sources.push(cfg);
        self.bump(&mut inner);
        drop(inner);
        self.supervise();
        Ok(id)
    }

    pub fn remove_source(&self, id: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.config.sources.len();
        inner.config.sources.retain(|s| s.id != id);
        if inner.config.sources.len() == before {
            bail!("no such source");
        }
        if let Some(rt) = inner.sources.remove(id)
            && let Some(w) = rt.capture
        {
            w.stop();
        }
        inner.config.hotkeys.retain(|h| h.target != id);
        self.mixer.lock().unwrap().remove_input(id);
        self.bump(&mut inner);
        Ok(())
    }

    pub fn update_channel(&self, id: &str, patch: ChannelPatch) -> anyhow::Result<()> {
        if let Some(name) = &patch.name {
            check_len("name", name.trim(), MAX_NAME_CHARS)?;
        }
        let mut inner = self.inner.lock().unwrap();
        let (channel, ctrl) = if id == MASTER {
            (&mut inner.config.master, self.master.clone())
        } else {
            let ctrl = inner
                .sources
                .get(id)
                .ok_or_else(|| anyhow!("no such source"))?
                .ctrl
                .clone();
            let src = inner
                .config
                .sources
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow!("no such source"))?;
            if let Some(name) = patch.name.filter(|n| !n.trim().is_empty()) {
                src.name = name.trim().to_owned();
            }
            (&mut src.channel, ctrl)
        };
        if let Some(db) = patch.gain_db {
            channel.gain_db = db.clamp(-60.0, 12.0);
            ctrl.set_gain(db_to_gain(channel.gain_db));
        }
        if let Some(muted) = patch.muted {
            channel.muted = muted;
            ctrl.set_muted(muted);
        }
        self.bump(&mut inner);
        Ok(())
    }

    pub fn toggle_mute(&self, id: &str) {
        let muted = {
            let inner = self.inner.lock().unwrap();
            if id == MASTER {
                inner.config.master.muted
            } else {
                match inner.config.sources.iter().find(|s| s.id == id) {
                    Some(s) => s.channel.muted,
                    None => return,
                }
            }
        };
        let _ = self.update_channel(
            id,
            ChannelPatch {
                muted: Some(!muted),
                ..Default::default()
            },
        );
    }

    pub fn set_output(&self, output: Option<DeviceRef>) {
        let mut inner = self.inner.lock().unwrap();
        if inner.config.output == output {
            return;
        }
        inner.config.output = output;
        self.restart_output(&mut inner);
        // Re-run the feedback check for device sources against the new output.
        let devices: Vec<String> = inner
            .config
            .sources
            .iter()
            .filter(|s| matches!(s.kind, SourceKind::Device { .. }))
            .map(|s| s.id.clone())
            .collect();
        for id in devices {
            if let Some(rt) = inner.sources.get_mut(&id) {
                if let Some(w) = rt.capture.take() {
                    w.stop();
                }
                rt.retry_at = Instant::now();
            }
        }
        self.bump(&mut inner);
    }

    pub fn set_hotkeys(&self, hotkeys: Vec<Hotkey>) -> anyhow::Result<()> {
        if hotkeys.len() > MAX_HOTKEYS {
            bail!("too many hotkeys (max {MAX_HOTKEYS})");
        }
        let mut inner = self.inner.lock().unwrap();
        for h in &hotkeys {
            check_len("hotkey", &h.keys, MAX_NAME_CHARS)?;
            crate::hotkeys::parse(&h.keys)?;
            if h.target != MASTER && !inner.sources.contains_key(&h.target) {
                bail!("hotkey target {} does not exist", h.target);
            }
        }
        inner.config.hotkeys = hotkeys
            .into_iter()
            .map(|mut h| {
                if h.id.is_empty() {
                    h.id = new_id("h");
                }
                h
            })
            .collect();
        self.bump(&mut inner);
        Ok(())
    }

    // -- threads ------------------------------------------------------------

    fn restart_output(&self, inner: &mut Inner) {
        if let Some(w) = inner.output.take() {
            w.stop();
        }
        let backend = self.backend.clone();
        let mixer = self.mixer.clone();
        let status = Arc::new(Mutex::new(Status::default()));
        inner.output_status = status.clone();
        let device = inner.config.output.clone();
        let changed = self.changed.clone();
        inner.output = Some(Worker::spawn("render".into(), move |stop| {
            render_thread(&*backend, device, &mixer, &status, &stop, &changed)
        }));
    }

    /// Starts captures that should be running and reaps dead ones.
    ///
    /// Backend queries (COM device/process enumeration) and the config save
    /// can be slow, so they run without holding `inner`: snapshot what is
    /// needed, query, then re-lock briefly to apply.
    fn supervise(&self) {
        let _serial = self.supervising.lock().unwrap();
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();

        // 1. Snapshot under the lock.
        let (sources, pids, wants_default, to_resolve) = {
            let inner = self.inner.lock().unwrap();
            let mut pids = Vec::new();
            let mut wants_default = false;
            let mut to_resolve = Vec::new();
            for src in &inner.config.sources {
                let rt = &inner.sources[&src.id];
                pids.extend(rt.pid);
                let idle = rt.capture.is_none() && now >= rt.retry_at;
                match &src.kind {
                    SourceKind::Device { device: None } => {
                        wants_default |= rt.capture.is_some() || idle
                    }
                    SourceKind::App { exe } if idle => to_resolve.push(exe.clone()),
                    _ => {}
                }
            }
            (
                inner.config.sources.clone(),
                pids,
                wants_default,
                to_resolve,
            )
        };

        // 2. Query the backend without the lock.
        let default_input = wants_default
            .then(|| self.backend.default_input())
            .flatten();
        let alive: HashMap<u32, bool> = pids
            .into_iter()
            .map(|pid| (pid, self.backend.process_alive(pid)))
            .collect();
        let found: HashMap<String, Option<u32>> = to_resolve
            .into_iter()
            .map(|exe| {
                let pid = self.backend.find_process(&exe);
                (exe, pid)
            })
            .collect();

        // 3. Apply under the lock. Sources may have changed in the meantime.
        let mut inner = self.inner.lock().unwrap();
        let mut changed = false;
        if inner.output.as_ref().is_some_and(|w| w.finished()) {
            tracing::error!("output thread stopped unexpectedly, restarting it");
            self.restart_output(&mut inner);
            changed = true;
        }
        let output_name = inner.config.output.as_ref().map(|o| o.name.clone());
        for src in &sources {
            let Some(rt) = inner.sources.get_mut(&src.id) else {
                continue;
            };

            if rt.capture.is_some()
                && let Some(captured) = &rt.default_id
                && default_input.as_ref().map(|d| &d.id) != Some(captured)
            {
                let name = default_input.as_ref().map_or("none", |d| d.name.as_str());
                tracing::info!(
                    "source {:?}: Windows default mic changed to {name:?}",
                    src.name
                );
                if let Some(w) = rt.capture.take() {
                    w.stop();
                }
                rt.default_id = None;
                rt.retry_at = now;
            }

            let exited = rt.pid.is_some_and(|pid| alive.get(&pid) == Some(&false));
            let failed = rt.capture.as_ref().is_some_and(|w| w.finished());
            if exited || failed {
                if let Some(w) = rt.capture.take() {
                    w.stop();
                }
                rt.pid = None;
                match &src.kind {
                    SourceKind::App { exe } if exited => {
                        set_status(
                            &rt.status,
                            "waiting",
                            format!("{exe} exited, waiting for it to start"),
                        );
                        rt.retry_at = now;
                    }
                    _ => {
                        // A thread that ends without reporting an error panicked.
                        if rt.status.lock().unwrap().state != "error" {
                            set_status(
                                &rt.status,
                                "error",
                                "Capture stopped unexpectedly, see the log",
                            );
                        }
                        rt.retry_at = now + RETRY_DELAY;
                    }
                }
                changed = true;
            }
            if rt.capture.is_some() || now < rt.retry_at {
                continue;
            }

            let target = match &src.kind {
                SourceKind::Device { device } => {
                    // Resolve "default" to a concrete device so we open exactly
                    // what we checked, and can notice when the default moves.
                    let (id, name) = match device {
                        Some(d) => (Some(d.id.clone()), Some(d.name.clone())),
                        None => match &default_input {
                            Some(d) => (Some(d.id.clone()), Some(d.name.clone())),
                            None => (None, None),
                        },
                    };
                    rt.default_id = if device.is_none() { id.clone() } else { None };
                    if let (Some(name), Some(out)) = (&name, &output_name)
                        && feeds_back(name, out)
                    {
                        let msg = format!(
                            "“{name}” is the other end of the output “{out}”, so capturing \
                                 it would loop the mix into itself. Choose your real microphone."
                        );
                        if rt.status.lock().unwrap().message != msg {
                            tracing::warn!("source {:?}: refusing feedback loop: {msg}", src.name);
                            set_status(&rt.status, "error", msg);
                            self.mixer.lock().unwrap().remove_input(&src.id);
                            changed = true;
                        }
                        rt.retry_at = now + RETRY_DELAY;
                        continue;
                    }
                    CaptureTarget::Device(id)
                }
                SourceKind::App { exe } => match found.get(exe).copied().flatten() {
                    Some(pid) => {
                        tracing::info!("source {:?}: found {exe} as PID {pid}", src.name);
                        rt.pid = Some(pid);
                        CaptureTarget::Process(pid)
                    }
                    None => {
                        // Not looked up this tick (it became idle meanwhile)
                        // or not running: try again shortly.
                        if found.contains_key(exe) {
                            let msg = format!("Waiting for {exe} to start");
                            if rt.status.lock().unwrap().message != msg {
                                set_status(&rt.status, "waiting", msg);
                                changed = true;
                            }
                            rt.retry_at = now + Duration::from_secs(2);
                        }
                        continue;
                    }
                },
            };

            let (tx, rx) = source_ring();
            self.mixer
                .lock()
                .unwrap()
                .set_input(&src.id, rt.ctrl.clone(), rx);
            set_status(&rt.status, "waiting", "Starting…");
            let backend = self.backend.clone();
            let status = rt.status.clone();
            let changed_tx = self.changed.clone();
            rt.capture = Some(Worker::spawn(
                format!("capture {}", src.name),
                move |stop| capture_thread(&*backend, target, tx, &status, &stop, &changed_tx),
            ));
            changed = true;
        }
        if changed {
            self.changed.send_modify(|v| *v += 1);
        }

        // 4. Save without the lock. On failure mark dirty again so the next
        // tick retries, and log each distinct error once.
        let to_save = std::mem::take(&mut inner.dirty).then(|| inner.config.clone());
        drop(inner);
        if let Some(config) = to_save {
            let result = config.save();
            let mut inner = self.inner.lock().unwrap();
            match result {
                Ok(()) => inner.save_error = None,
                Err(e) => {
                    inner.dirty = true;
                    let msg = format!("{e:#}");
                    if inner.save_error.as_ref() != Some(&msg) {
                        tracing::error!("saving config: {msg}");
                        inner.save_error = Some(msg);
                    }
                }
            }
        }
    }
}

impl SourceRuntime {
    fn new(channel: &Channel) -> Self {
        Self {
            ctrl: Arc::new(ChannelControl::new(
                db_to_gain(channel.gain_db),
                channel.muted,
            )),
            status: Arc::new(Mutex::new(Status::new("waiting", "Starting…"))),
            capture: None,
            pid: None,
            default_id: None,
            retry_at: Instant::now(),
        }
    }
}

/// Whether capturing `capture` would record the mix we play into `output`,
/// i.e. it is the recording end of the same virtual cable. Virtual cables
/// name their ends "X Input (…)" / "X Output (…)" (VB-CABLE, Voicemeeter).
fn feeds_back(capture: &str, output: &str) -> bool {
    let capture = capture.to_lowercase();
    capture.contains("output") && capture.replacen("output", "input", 1) == output.to_lowercase()
}

fn display_exe(exe: &str) -> String {
    let stem = exe.trim().trim_end_matches(".exe").trim_end_matches(".EXE");
    let mut chars = stem.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => exe.to_owned(),
    }
}

fn capture_thread(
    backend: &dyn Backend,
    target: CaptureTarget,
    mut tx: rtrb::Producer<f32>,
    status: &SharedStatus,
    stop: &AtomicBool,
    changed: &tokio::sync::watch::Sender<u64>,
) {
    // Once stopped, a replacement thread shares `status`; stay quiet so a
    // late result from this one (e.g. a slow open) can't overwrite it.
    let report = |state, message: String| {
        if !stop.load(Ordering::Relaxed) {
            set_status(status, state, message);
            changed.send_modify(|v| *v += 1);
        }
    };
    backend.promote_audio_thread();
    tracing::info!("opening capture {target:?}");
    let mut stream = match backend.open_capture(&target) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("capture {target:?} failed: {e:#}");
            report("error", format!("{e:#}"));
            return;
        }
    };
    tracing::info!("capturing {target:?} ({})", stream.describe());
    report("running", stream.describe());
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = stream.read(&mut |data| push_frames(&mut tx, data)) {
            tracing::warn!("capture {target:?} stopped: {e:#}");
            report("error", format!("{e:#}"));
            return;
        }
    }
}

fn render_thread(
    backend: &dyn Backend,
    device: Option<DeviceRef>,
    mixer: &Mutex<Mixer>,
    status: &SharedStatus,
    stop: &AtomicBool,
    changed: &tokio::sync::watch::Sender<u64>,
) {
    backend.promote_audio_thread();
    // Returns the stream plus when to retry opening the real device.
    let open = || -> (Box<dyn RenderStream>, Option<Instant>) {
        let result = match &device {
            None => {
                set_status(status, "idle", "No output selected");
                (
                    Box::new(NullRender::default()) as Box<dyn RenderStream>,
                    None,
                )
            }
            Some(dev) => match backend.open_render(&dev.id) {
                Ok(s) => {
                    set_status(status, "running", dev.name.clone());
                    (s, None)
                }
                Err(e) => {
                    tracing::warn!("opening output {}: {e:#}", dev.name);
                    set_status(status, "error", format!("{}: {e:#}", dev.name));
                    (
                        Box::new(NullRender::default()) as Box<dyn RenderStream>,
                        Some(Instant::now() + RETRY_DELAY),
                    )
                }
            },
        };
        changed.send_modify(|v| *v += 1);
        result
    };
    let (mut stream, mut retry_at) = open();
    while !stop.load(Ordering::Relaxed) {
        if retry_at.is_some_and(|t| Instant::now() >= t) {
            (stream, retry_at) = open();
        }
        if let Err(e) = stream.render(&mut |buf| mixer.lock().unwrap().process(buf)) {
            tracing::warn!("output failed: {e:#}");
            set_status(status, "error", format!("{e:#}"));
            changed.send_modify(|v| *v += 1);
            stream = Box::new(NullRender::default());
            retry_at = Some(Instant::now() + RETRY_DELAY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_virtual_cable_loopback() {
        assert!(feeds_back(
            "CABLE Output (VB-Audio Virtual Cable)",
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
        assert!(feeds_back(
            "VoiceMeeter Output (VB-Audio VoiceMeeter VAIO)",
            "VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"
        ));
        assert!(!feeds_back(
            "Microphone (Blue Snowball)",
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
        assert!(!feeds_back(
            "CABLE-A Output (VB-Audio Cable A)",
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
    }

    #[test]
    fn exe_display_names() {
        assert_eq!(display_exe("chrome.exe"), "Chrome");
        assert_eq!(display_exe("Soundpad.EXE"), "Soundpad");
    }
}
