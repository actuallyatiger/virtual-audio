//! Owns the configuration and all running audio threads.
//!
//! * One render thread pulls from the [`Mixer`] at the output device's pace
//!   (or a 10 ms timer when no device is available).
//! * One capture thread per source pushes into that source's ring buffer
//!   (the soundboard's "capture" mixes the sounds that are playing).
//! * A supervisor thread (re)starts captures when apps launch/exit or devices
//!   come and go, and persists configuration changes.

use crate::backend::{Backend, CaptureStream, CaptureTarget, DeviceInfo, NullRender, RenderStream};
use crate::config::{
    Channel, Config, DeviceRef, GateMode, Hotkey, HotkeyAction, MusicItem, Processing, Repeat,
    SoundConfig, SourceConfig, SourceKind, db_to_gain, new_id,
};
use crate::dsp::{ProcessingControl, Processor};
use crate::mixer::{ChannelControl, Mixer, MonitorTap, push_frames, source_ring};
use crate::music::{self, MusicPlayer, MusicView};
use crate::soundboard::{self, MAX_FILE_BYTES, MAX_SOUNDS, Player, Trim};
use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    processing: Arc<ProcessingControl>,
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
    /// UI connections that have Listen switched on.
    listeners: HashSet<u64>,
    /// The whole mix on the default output, while anyone has Listen on.
    listen: Monitor,
    /// Just the soundboard on the default output (unless Listen is on,
    /// which already includes it).
    hear: Monitor,
    dirty: bool,
    save_error: Option<String>,
    /// Sounds that failed to load at startup, with why.
    sound_errors: HashMap<String, String>,
}

/// Something played on the Windows default output (your headphones), fed
/// from a tap: Listen (the final mix) or Hear sounds (the soundboard).
struct Monitor {
    /// What it's for, in messages: "listen", "hear sounds".
    what: &'static str,
    /// What it plays, each with its own volume.
    inputs: Vec<(MonitorTap, Arc<ChannelControl>)>,
    worker: Option<Worker>,
    status: SharedStatus,
    /// (default output id, mix output id) it was last set up for; a change
    /// means it must be re-checked and restarted.
    key: Option<(Option<String>, Option<String>)>,
    retry_at: Instant,
}

impl Monitor {
    fn new(what: &'static str, inputs: Vec<(MonitorTap, Arc<ChannelControl>)>) -> Self {
        Self {
            what,
            inputs,
            worker: None,
            status: Arc::new(Mutex::new(Status::new("idle", ""))),
            key: None,
            retry_at: Instant::now(),
        }
    }

    fn stop(&mut self) {
        for (tap, _) in &self.inputs {
            *tap.lock().unwrap() = None;
        }
        if let Some(w) = self.worker.take() {
            w.stop();
        }
        self.key = None;
        self.status = Arc::new(Mutex::new(Status::new("idle", "")));
    }

    /// Shows why it can't play; the supervisor re-checks when devices change.
    fn refuse(&mut self, message: String) {
        tracing::warn!("{}: {message}", self.what);
        self.status = Arc::new(Mutex::new(Status::new("error", message)));
    }
}

pub struct Engine {
    backend: Arc<dyn Backend>,
    inner: Mutex<Inner>,
    master: Arc<ChannelControl>,
    mixer: Arc<Mutex<Mixer>>,
    player: Arc<Player>,
    music: Arc<MusicPlayer>,
    tap: MonitorTap,
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
    /// Microphone processing, for device sources only.
    pub noise_suppression: Option<bool>,
    pub noise_gate: Option<bool>,
    pub gate_mode: Option<GateMode>,
    pub gate_db: Option<f32>,
}

/// The music player for the UI: settings, list and what's playing.
#[derive(Serialize)]
pub struct MusicStateView {
    pub items: Vec<MusicItem>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub duck: bool,
    pub duck_db: f32,
    pub hear: bool,
    pub hear_db: f32,
    #[serde(flatten)]
    pub player: MusicView,
}

#[derive(Debug, Default, Deserialize)]
pub struct MusicPatch {
    pub shuffle: Option<bool>,
    pub repeat: Option<Repeat>,
    pub duck: Option<bool>,
    pub duck_db: Option<f32>,
    pub hear: Option<bool>,
    pub hear_db: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MusicControl {
    Toggle,
    Pause,
    Next,
    Previous,
    /// Play a track (index into the list) from the start.
    Play {
        track: usize,
    },
    Seek {
        position_ms: u64,
    },
}

#[derive(Debug, Default, Deserialize)]
pub struct SoundboardPatch {
    pub hear_sounds: Option<bool>,
    pub hear_sounds_db: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SoundPatch {
    pub name: Option<String>,
    pub gain_db: Option<f32>,
    pub start_ms: Option<u32>,
    /// `null` plays to the end again.
    #[serde(default, deserialize_with = "present")]
    pub end_ms: Option<Option<u32>>,
    pub fade_in_ms: Option<u32>,
    pub fade_out_ms: Option<u32>,
}

/// Tells a field that was sent as `null` (`Some(None)`) from one that was
/// left out (`None`).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d).map(Some)
}

#[derive(Serialize)]
pub struct SourceView {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: SourceKind,
    #[serde(flatten)]
    pub channel: Channel,
    #[serde(flatten)]
    pub processing: Processing,
    pub status: Status,
}

#[derive(Serialize)]
pub struct SoundView {
    pub id: String,
    pub name: String,
    pub gain_db: f32,
    pub start_ms: u32,
    pub end_ms: Option<u32>,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    /// Length of the whole file, once loaded.
    pub duration_ms: Option<u32>,
    /// "loading", "ready" or "error".
    pub state: &'static str,
    pub message: String,
}

#[derive(Serialize)]
pub struct StateView {
    pub version: u64,
    pub output: Option<DeviceRef>,
    pub output_status: Status,
    pub master: Channel,
    pub sources: Vec<SourceView>,
    pub hotkeys: Vec<Hotkey>,
    /// Listen (monitor the mix on the default output) status.
    pub monitor: Status,
    pub hear_sounds: bool,
    pub hear_sounds_db: f32,
    /// Hear sounds status.
    pub hear_status: Status,
    pub music: MusicStateView,
    pub sounds: Vec<SoundView>,
}

#[derive(Serialize)]
pub struct ChannelMeter {
    pub peak: f32,
    /// False while a push-to-talk/mute hotkey is silencing the channel.
    pub open: bool,
    /// Device sources: input level after noise suppression, before the
    /// gain and the noise gate (linear peak).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic_level: Option<f32>,
    /// Device sources: whether the noise gate is letting audio through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic_gate_open: Option<bool>,
    /// Device sources: raw input peak, before any processing (for the
    /// setup check's "is the mic picking anything up").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic_input: Option<f32>,
}

impl Engine {
    pub fn new(backend: Box<dyn Backend>, config: Config) -> Arc<Self> {
        let master = Arc::new(ChannelControl::new(
            db_to_gain(config.master.gain_db),
            config.master.muted,
        ));
        let mixer = Arc::new(Mutex::new(Mixer::new(master.clone())));
        let tap: MonitorTap = Arc::default();
        let player = Arc::new(Player::default());
        let changed = tokio::sync::watch::channel(0).0;
        let notify = changed.clone();
        let music = MusicPlayer::new(config.music.shuffle, config.music.repeat, move || {
            notify.send_modify(|v| *v += 1)
        });
        music
            .duck
            .enabled
            .store(config.music.duck, Ordering::Relaxed);
        music.duck.set_db(config.music.duck_db);
        let listen = Monitor::new(
            "listen",
            vec![(tap.clone(), Arc::new(ChannelControl::new(1.0, false)))],
        );
        // Hear sounds and Hear music share one monitor, each muted when off.
        let hear = Monitor::new(
            "hear sounds",
            vec![
                (
                    player.tap.clone(),
                    Arc::new(ChannelControl::new(
                        db_to_gain(config.hear_sounds_db),
                        !config.hear_sounds,
                    )),
                ),
                (
                    music.tap.clone(),
                    Arc::new(ChannelControl::new(
                        db_to_gain(config.music.hear_db),
                        !config.music.hear,
                    )),
                ),
            ],
        );
        let mut sources = HashMap::new();
        for s in &config.sources {
            sources.insert(s.id.clone(), SourceRuntime::new(s));
        }
        let engine = Arc::new(Self {
            backend: Arc::from(backend),
            inner: Mutex::new(Inner {
                config,
                sources,
                output: None,
                output_status: Arc::new(Mutex::new(Status::default())),
                listeners: HashSet::new(),
                listen,
                hear,
                dirty: false,
                save_error: None,
                sound_errors: HashMap::new(),
            }),
            master,
            mixer,
            player,
            music,
            tap,
            changed,
            shutdown: AtomicBool::new(false),
            supervising: Mutex::new(()),
        });
        engine.restart_output(&mut engine.inner.lock().unwrap());
        engine.load_sounds();
        engine.rescan_music_in_background();
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
        inner.listen.stop();
        inner.hear.stop();
        self.music.shutdown();
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
                    processing: s.processing.clone(),
                    status: inner.sources[&s.id].status.lock().unwrap().clone(),
                })
                .collect(),
            hotkeys: inner.config.hotkeys.clone(),
            monitor: inner.listen.status.lock().unwrap().clone(),
            hear_sounds: inner.config.hear_sounds,
            hear_sounds_db: inner.config.hear_sounds_db,
            hear_status: inner.hear.status.lock().unwrap().clone(),
            music: MusicStateView {
                items: inner.config.music.items.clone(),
                shuffle: inner.config.music.shuffle,
                repeat: inner.config.music.repeat,
                duck: inner.config.music.duck,
                duck_db: inner.config.music.duck_db,
                hear: inner.config.music.hear,
                hear_db: inner.config.music.hear_db,
                player: self.music.view(),
            },
            sounds: inner
                .config
                .sounds
                .iter()
                .map(|s| {
                    let (state, message) = if self.player.contains(&s.id) {
                        ("ready", String::new())
                    } else if let Some(e) = inner.sound_errors.get(&s.id) {
                        ("error", e.clone())
                    } else {
                        ("loading", String::new())
                    };
                    SoundView {
                        id: s.id.clone(),
                        name: s.name.clone(),
                        gain_db: s.gain_db,
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        fade_in_ms: s.fade_in_ms,
                        fade_out_ms: s.fade_out_ms,
                        duration_ms: self.player.duration_ms(&s.id),
                        state,
                        message,
                    }
                })
                .collect(),
        }
    }

    pub fn meters(&self) -> HashMap<String, ChannelMeter> {
        let inner = self.inner.lock().unwrap();
        let mut out: HashMap<String, ChannelMeter> = inner
            .config
            .sources
            .iter()
            .map(|s| {
                let rt = &inner.sources[&s.id];
                let mic = matches!(s.kind, SourceKind::Device { .. });
                (
                    s.id.clone(),
                    ChannelMeter {
                        peak: rt.ctrl.peak(),
                        open: rt.ctrl.gate_open(),
                        mic_level: mic.then(|| rt.processing.level()),
                        mic_gate_open: mic.then(|| rt.processing.gate_open()),
                        mic_input: mic.then(|| rt.processing.input_level()),
                    },
                )
            })
            .collect();
        out.insert(
            MASTER.into(),
            ChannelMeter {
                peak: self.master.peak(),
                open: self.master.gate_open(),
                mic_level: None,
                mic_gate_open: None,
                mic_input: None,
            },
        );
        out
    }

    /// Ids of the soundboard sounds playing now.
    pub fn playing_sounds(&self) -> Vec<String> {
        self.player.playing()
    }

    /// Where the current music track is.
    pub fn music_position_ms(&self) -> u64 {
        self.music.position_ms()
    }

    pub fn autostart_admin(&self) -> bool {
        self.inner.lock().unwrap().config.autostart_admin
    }

    pub fn set_autostart_admin(&self, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.config.autostart_admin = on;
        self.bump(&mut inner);
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
        let mut inner = self.inner.lock().unwrap();
        let id = push_source(&mut inner, name, kind)?;
        self.bump(&mut inner);
        drop(inner);
        self.supervise();
        Ok(id)
    }

    pub fn remove_source(&self, id: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let index = inner
            .config
            .sources
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| anyhow!("no such source"))?;
        match inner.config.sources.remove(index).kind {
            // Nothing plays them any more; don't let them resume when the
            // soundboard comes back.
            SourceKind::Soundboard => self.player.clear(),
            SourceKind::Music => self.music.pause(),
            _ => {}
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
        if patch.gate_db.is_some_and(|db| !db.is_finite()) {
            bail!("invalid gate level");
        }
        let processing_patch = patch.noise_suppression.is_some()
            || patch.noise_gate.is_some()
            || patch.gate_mode.is_some()
            || patch.gate_db.is_some();
        let mut inner = self.inner.lock().unwrap();
        let (channel, ctrl) = if id == MASTER {
            if processing_patch {
                bail!("noise suppression and the noise gate are only for microphones");
            }
            (&mut inner.config.master, self.master.clone())
        } else {
            let rt = inner
                .sources
                .get(id)
                .ok_or_else(|| anyhow!("no such source"))?;
            let (ctrl, processing) = (rt.ctrl.clone(), rt.processing.clone());
            let src = inner
                .config
                .sources
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow!("no such source"))?;
            if processing_patch && !matches!(src.kind, SourceKind::Device { .. }) {
                bail!("noise suppression and the noise gate are only for microphones");
            }
            if let Some(name) = patch.name.filter(|n| !n.trim().is_empty()) {
                src.name = name.trim().to_owned();
            }
            if let Some(on) = patch.noise_suppression {
                src.processing.noise_suppression = on;
                processing.set_suppress(on);
            }
            if let Some(on) = patch.noise_gate {
                src.processing.noise_gate = on;
                processing.set_gate(on);
            }
            if let Some(mode) = patch.gate_mode {
                src.processing.gate_mode = mode;
                processing.set_gate_auto(mode == GateMode::Auto);
            }
            if let Some(db) = patch.gate_db {
                src.processing.gate_db = db.clamp(-60.0, 0.0);
                processing.set_threshold_db(src.processing.gate_db);
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
            let exists = match h.action {
                HotkeyAction::PlaySound => inner.config.sounds.iter().any(|s| s.id == h.target),
                HotkeyAction::StopSounds
                | HotkeyAction::MusicPlayPause
                | HotkeyAction::MusicNext
                | HotkeyAction::MusicPrevious => true,
                _ => h.target == MASTER || inner.sources.contains_key(&h.target),
            };
            if !exists {
                bail!("hotkey target {} does not exist", h.target);
            }
        }
        inner.config.hotkeys = hotkeys
            .into_iter()
            .map(|mut h| {
                if h.id.is_empty() {
                    h.id = new_id("h");
                }
                if matches!(
                    h.action,
                    HotkeyAction::StopSounds
                        | HotkeyAction::MusicPlayPause
                        | HotkeyAction::MusicNext
                        | HotkeyAction::MusicPrevious
                ) {
                    h.target.clear();
                }
                h
            })
            .collect();
        self.bump(&mut inner);
        Ok(())
    }

    // -- soundboard ---------------------------------------------------------

    /// Decodes the saved sounds in the background, so a large library
    /// doesn't delay startup.
    fn load_sounds(self: &Arc<Self>) {
        let sounds = self.inner.lock().unwrap().config.sounds.clone();
        if sounds.is_empty() {
            return;
        }
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("load sounds".into())
            .spawn(move || {
                for sound in sounds {
                    let ext = soundboard::extension(&sound.file);
                    let result = soundboard::sound_path(&sound.file).and_then(|path| {
                        let bytes = std::fs::read(&path)
                            .with_context(|| format!("reading {}", path.display()))?;
                        soundboard::decode(bytes, Some(&ext))
                    });
                    let Some(engine) = weak.upgrade() else { return };
                    let mut inner = engine.inner.lock().unwrap();
                    // It may have been removed or changed while loading.
                    let Some(current) = inner.config.sounds.iter().find(|s| s.id == sound.id)
                    else {
                        continue;
                    };
                    match result {
                        Ok(samples) => {
                            engine.player.insert(
                                &sound.id,
                                samples,
                                db_to_gain(current.gain_db),
                                trim_of(current),
                            );
                        }
                        Err(e) => {
                            tracing::warn!("sound {:?}: {e:#}", sound.name);
                            inner
                                .sound_errors
                                .insert(sound.id.clone(), format!("{e:#}"));
                        }
                    }
                    drop(inner);
                    engine.changed.send_modify(|v| *v += 1);
                }
            })
            .expect("spawn sound loader");
    }

    /// Adds an uploaded audio file to the soundboard, and the soundboard to
    /// the mix if it isn't there. Decoding can take a moment, so call it off
    /// the async executor.
    pub fn add_sound(
        &self,
        name: Option<String>,
        filename: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<String> {
        if bytes.len() > MAX_FILE_BYTES {
            bail!("the file is too big (max {} MB)", MAX_FILE_BYTES >> 20);
        }
        let name = match name.filter(|n| !n.trim().is_empty()) {
            Some(n) => n.trim().to_owned(),
            None => {
                let stem = std::path::Path::new(filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Sound");
                stem.chars().take(MAX_NAME_CHARS).collect()
            }
        };
        check_len("name", &name, MAX_NAME_CHARS)?;
        if self.inner.lock().unwrap().config.sounds.len() >= MAX_SOUNDS {
            bail!("too many sounds (max {MAX_SOUNDS})");
        }
        let ext = soundboard::extension(filename);
        let samples = soundboard::decode(bytes.clone(), Some(&ext))?;
        let id = new_id("snd");
        let file = format!("{id}.{ext}");
        let path = soundboard::sounds_dir().join(&file);
        std::fs::create_dir_all(soundboard::sounds_dir())
            .and_then(|()| std::fs::write(&path, &bytes))
            .with_context(|| format!("saving {}", path.display()))?;

        let mut inner = self.inner.lock().unwrap();
        if inner.config.sounds.len() >= MAX_SOUNDS {
            drop(inner);
            let _ = std::fs::remove_file(&path);
            bail!("too many sounds (max {MAX_SOUNDS})");
        }
        let add_board = !inner
            .config
            .sources
            .iter()
            .any(|s| s.kind == SourceKind::Soundboard);
        // Before committing the sound, so a failure leaves nothing behind.
        if add_board && let Err(e) = push_source(&mut inner, None, SourceKind::Soundboard) {
            drop(inner);
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        self.player.insert(&id, samples, 1.0, Trim::default());
        inner.config.sounds.push(SoundConfig {
            id: id.clone(),
            name,
            file,
            gain_db: 0.0,
            start_ms: 0,
            end_ms: None,
            fade_in_ms: 0,
            fade_out_ms: 0,
        });
        self.bump(&mut inner);
        drop(inner);
        if add_board {
            self.supervise();
        }
        Ok(id)
    }

    pub fn update_sound(&self, id: &str, patch: SoundPatch) -> anyhow::Result<()> {
        if let Some(name) = &patch.name {
            check_len("name", name.trim(), MAX_NAME_CHARS)?;
        }
        let mut inner = self.inner.lock().unwrap();
        let sound = inner
            .config
            .sounds
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow!("no such sound"))?;
        if let Some(name) = patch.name.filter(|n| !n.trim().is_empty()) {
            sound.name = name.trim().to_owned();
        }
        if let Some(db) = patch.gain_db {
            sound.gain_db = db.clamp(-60.0, 12.0);
            self.player.set_gain(id, db_to_gain(sound.gain_db));
        }
        let trimmed = patch.start_ms.is_some()
            || patch.end_ms.is_some()
            || patch.fade_in_ms.is_some()
            || patch.fade_out_ms.is_some();
        if trimmed {
            // Validate the whole trim before changing anything.
            let duration = self
                .player
                .duration_ms(id)
                .ok_or_else(|| anyhow!("the sound is still loading"))?;
            let start = patch.start_ms.unwrap_or(sound.start_ms).min(duration);
            let end = patch
                .end_ms
                .unwrap_or(sound.end_ms)
                .map(|e| e.min(duration));
            let length = end.unwrap_or(duration).saturating_sub(start);
            if length < MIN_SOUND_MS {
                bail!("the trimmed sound must be at least {MIN_SOUND_MS} ms long");
            }
            let fade_in = patch.fade_in_ms.unwrap_or(sound.fade_in_ms);
            let fade_out = patch.fade_out_ms.unwrap_or(sound.fade_out_ms);
            if fade_in > MAX_FADE_MS || fade_out > MAX_FADE_MS {
                bail!("fades can be at most {MAX_FADE_MS} ms");
            }
            sound.start_ms = start;
            // An end at the very end is the same as no end.
            sound.end_ms = end.filter(|&e| e < duration);
            sound.fade_in_ms = fade_in;
            sound.fade_out_ms = fade_out;
            self.player.set_trim(id, trim_of(sound));
        }
        self.bump(&mut inner);
        Ok(())
    }

    /// The sound's shape for the trim editor.
    pub fn sound_waveform(&self, id: &str) -> anyhow::Result<soundboard::Waveform> {
        self.player
            .waveform(id, WAVEFORM_BUCKETS)
            .ok_or_else(|| anyhow!("the sound isn't loaded"))
    }

    /// Plays a sound to you alone, through Hear sounds, to try out a trim.
    pub fn preview_sound(&self, id: &str) -> anyhow::Result<()> {
        let inner = self.inner.lock().unwrap();
        if inner.hear.status.lock().unwrap().state != "running" {
            bail!("previews play through Hear sounds, which isn't playing right now");
        }
        if !self.player.preview(id) {
            bail!("the sound isn't loaded");
        }
        Ok(())
    }

    pub fn remove_sound(&self, id: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let index = inner
            .config
            .sounds
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| anyhow!("no such sound"))?;
        let sound = inner.config.sounds.remove(index);
        inner.sound_errors.remove(id);
        inner.config.hotkeys.retain(|h| h.target != id);
        self.player.remove(id);
        self.bump(&mut inner);
        drop(inner);
        match soundboard::sound_path(&sound.file) {
            Ok(path) => {
                if let Err(e) = std::fs::remove_file(&path)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!("deleting {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("not deleting sound file: {e:#}"),
        }
        Ok(())
    }

    pub fn play_sound(&self, id: &str) -> anyhow::Result<()> {
        let inner = self.inner.lock().unwrap();
        if !inner.config.sounds.iter().any(|s| s.id == id) {
            bail!("no such sound");
        }
        if !inner
            .config
            .sources
            .iter()
            .any(|s| s.kind == SourceKind::Soundboard)
        {
            bail!("the soundboard isn't in the mix; add it back first");
        }
        if !self.player.play(id) {
            match inner.sound_errors.get(id) {
                Some(e) => bail!("the sound couldn't be loaded: {e}"),
                None => bail!("the sound is still loading"),
            }
        }
        Ok(())
    }

    pub fn stop_sounds(&self) {
        self.player.stop_all();
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
        let tap = self.tap.clone();
        inner.output = Some(Worker::spawn("render".into(), move |stop| {
            render_thread(&*backend, device, &mixer, &tap, &status, &stop, &changed)
        }));
    }

    /// Turns Listen on/off for one UI connection. The mix plays on the
    /// Windows default output while any connection has it on, so closing
    /// every tab that turned it on (which drops their connections) stops it.
    /// The supervisor keeps the monitor running: it restarts it after errors
    /// and when the default output or the mix output changes. May block
    /// briefly on device queries, so call it off the async executor.
    pub fn set_listening(&self, connection: u64, on: bool) {
        if on && self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let toggled = {
            let mut inner = self.inner.lock().unwrap();
            let was = !inner.listeners.is_empty();
            if on {
                inner.listeners.insert(connection);
            } else {
                inner.listeners.remove(&connection);
            }
            let now = !inner.listeners.is_empty();
            if was && !now {
                inner.listen.stop();
                self.changed.send_modify(|v| *v += 1);
            }
            was != now
        };
        if toggled {
            // Start Listen (or Hear sounds, which pauses while Listen is on)
            // now rather than at the next supervisor tick.
            self.supervise();
        }
    }

    pub fn update_soundboard(&self, patch: SoundboardPatch) -> anyhow::Result<()> {
        if patch.hear_sounds_db.is_some_and(|db| !db.is_finite()) {
            bail!("invalid volume");
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(db) = patch.hear_sounds_db {
            inner.config.hear_sounds_db = db.clamp(-60.0, 12.0);
            inner.hear.inputs[HEAR_SOUNDS]
                .1
                .set_gain(db_to_gain(inner.config.hear_sounds_db));
        }
        let toggled = patch
            .hear_sounds
            .is_some_and(|on| on != inner.config.hear_sounds);
        if let Some(on) = patch.hear_sounds {
            inner.config.hear_sounds = on;
            inner.hear.inputs[HEAR_SOUNDS].1.set_muted(!on);
        }
        self.bump(&mut inner);
        drop(inner);
        if toggled {
            self.supervise();
        }
        Ok(())
    }

    // -- music ----------------------------------------------------------------

    /// Scans the saved music folders without delaying startup.
    fn rescan_music_in_background(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("music scan".into())
            .spawn(move || {
                if let Some(engine) = weak.upgrade() {
                    engine.rescan_music();
                }
            })
            .expect("spawn music scan");
    }

    /// Re-reads the playlist's files and folders. Can take a while for big
    /// folders, so call it off the async executor.
    fn rescan_music(&self) {
        let paths: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner
                .config
                .music
                .items
                .iter()
                .map(|i| i.path.clone())
                .collect()
        };
        self.music.set_tracks(music::scan(&paths));
        self.changed.send_modify(|v| *v += 1);
    }

    /// Adds a music file or folder (by path) to the playlist, and the music
    /// player to the mix if it isn't there.
    pub fn add_music(&self, path: &str) -> anyhow::Result<String> {
        let path = path.trim().trim_matches('"');
        check_len("path", path, MAX_PATH_CHARS)?;
        let p = std::path::Path::new(path);
        if !p.is_absolute() {
            bail!("enter the full path, e.g. C:\\Users\\you\\Music");
        }
        if !p.exists() {
            bail!("can't find {path}");
        }
        if !p.is_dir() && !music::is_audio(p) {
            bail!("that isn't an audio file or a folder");
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.config.music.items.iter().any(|i| i.path == path) {
            bail!("that's already in the playlist");
        }
        if inner.config.music.items.len() >= MAX_MUSIC_ITEMS {
            bail!("too many files and folders (max {MAX_MUSIC_ITEMS})");
        }
        let add_source = !inner
            .config
            .sources
            .iter()
            .any(|s| s.kind == SourceKind::Music);
        if add_source {
            push_source(&mut inner, None, SourceKind::Music)?;
        }
        let id = new_id("m");
        inner.config.music.items.push(MusicItem {
            id: id.clone(),
            path: path.to_owned(),
        });
        self.bump(&mut inner);
        drop(inner);
        self.rescan_music();
        if add_source {
            self.supervise();
        }
        Ok(id)
    }

    pub fn remove_music(&self, id: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.config.music.items.len();
        inner.config.music.items.retain(|i| i.id != id);
        if inner.config.music.items.len() == before {
            bail!("no such file or folder");
        }
        self.bump(&mut inner);
        drop(inner);
        self.rescan_music();
        Ok(())
    }

    pub fn music_control(&self, control: MusicControl) -> anyhow::Result<()> {
        match control {
            MusicControl::Toggle => self.music.toggle(),
            MusicControl::Pause => self.music.pause(),
            MusicControl::Next => self.music.next(),
            MusicControl::Previous => self.music.previous(),
            MusicControl::Play { track } => {
                if !self.music.play_track(track) {
                    bail!("no such track");
                }
            }
            MusicControl::Seek { position_ms } => self.music.seek(position_ms),
        }
        self.changed.send_modify(|v| *v += 1);
        Ok(())
    }

    pub fn update_music(&self, patch: MusicPatch) -> anyhow::Result<()> {
        if [patch.duck_db, patch.hear_db]
            .iter()
            .flatten()
            .any(|db| !db.is_finite())
        {
            bail!("invalid volume");
        }
        let mut inner = self.inner.lock().unwrap();
        let m = &mut inner.config.music;
        if let Some(on) = patch.shuffle {
            m.shuffle = on;
            self.music.set_shuffle(on);
        }
        if let Some(repeat) = patch.repeat {
            m.repeat = repeat;
            self.music.set_repeat(repeat);
        }
        if let Some(on) = patch.duck {
            m.duck = on;
            self.music.duck.enabled.store(on, Ordering::Relaxed);
        }
        if let Some(db) = patch.duck_db {
            m.duck_db = db.clamp(-40.0, 0.0);
            self.music.duck.set_db(m.duck_db);
        }
        let toggled = patch.hear.is_some_and(|on| on != m.hear);
        if let Some(on) = patch.hear {
            m.hear = on;
        }
        if let Some(db) = patch.hear_db {
            m.hear_db = db.clamp(-60.0, 12.0);
        }
        let (hear, hear_db) = (m.hear, m.hear_db);
        let ctrl = &inner.hear.inputs[HEAR_MUSIC].1;
        ctrl.set_muted(!hear);
        ctrl.set_gain(db_to_gain(hear_db));
        self.bump(&mut inner);
        drop(inner);
        if toggled {
            self.supervise();
        }
        Ok(())
    }

    /// Starts, restarts, refuses or stops a monitor as needed. Called by
    /// supervise() with the current default output; returns whether the
    /// visible state changed.
    fn supervise_monitor(
        &self,
        m: &mut Monitor,
        wanted: bool,
        output: Option<&DeviceRef>,
        default_output: Option<&DeviceInfo>,
        now: Instant,
    ) -> bool {
        if !wanted {
            let active = m.worker.is_some() || m.key.is_some();
            if active {
                m.stop();
            }
            return active;
        }
        let key = (
            default_output.map(|d| d.id.clone()),
            output.map(|d| d.id.clone()),
        );
        if m.key.as_ref() == Some(&key) {
            match &m.worker {
                Some(w) if !w.finished() => return false, // healthy
                Some(_) if now < m.retry_at => return false,
                None => return false, // refused; waits for a device change
                Some(_) => {}         // failed: retry
            }
        }
        m.stop();
        m.key = Some(key);
        m.retry_at = now + RETRY_DELAY;
        match default_output {
            None => m.refuse(format!("No default output device to {} on", m.what)),
            Some(dev) if output.is_some_and(|o| o.id == dev.id) => m.refuse(format!(
                "Your Windows default output is “{}”, which the mix already goes to. \
                 Set your headphones as the default output to {}.",
                dev.name, m.what
            )),
            Some(dev) => self.start_monitor(m, dev.clone()),
        }
        true
    }

    fn start_monitor(&self, m: &mut Monitor, device: DeviceInfo) {
        // A mixer gives us priming, drift trimming and gain ramps.
        let mut mixer = Mixer::new(Arc::new(ChannelControl::new(1.0, false)));
        for (i, (tap, ctrl)) in m.inputs.iter().enumerate() {
            let (tx, rx) = source_ring();
            *tap.lock().unwrap() = Some(tx);
            mixer.set_input(&format!("monitor {i}"), ctrl.clone(), rx);
        }
        let status = Arc::new(Mutex::new(Status::new("waiting", "Starting…")));
        m.status = status.clone();
        let backend = self.backend.clone();
        let changed = self.changed.clone();
        let what = m.what;
        m.worker = Some(Worker::spawn(what.into(), move |stop| {
            monitor_thread(&*backend, mixer, device, what, &status, &stop, &changed)
        }));
    }

    /// Reorders sources to match `ids`, which must list every source once.
    pub fn reorder_sources(&self, ids: &[String]) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        reorder(&mut inner.config.sources, ids, |s| &s.id, "source")?;
        self.bump(&mut inner);
        Ok(())
    }

    /// Reorders soundboard sounds to match `ids`, which must list every
    /// sound once.
    pub fn reorder_sounds(&self, ids: &[String]) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        reorder(&mut inner.config.sounds, ids, |s| &s.id, "sound")?;
        self.bump(&mut inner);
        Ok(())
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
        let (sources, pids, wants_default, to_resolve, monitoring) = {
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
                !inner.listeners.is_empty() || hear_wanted(&inner),
            )
        };

        // 2. Query the backend without the lock.
        let default_input = wants_default
            .then(|| self.backend.default_input())
            .flatten();
        let default_output = monitoring.then(|| self.backend.default_output()).flatten();
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
        // Ducking listens to the microphones.
        *self.music.duck.mics.lock().unwrap() = inner
            .config
            .sources
            .iter()
            .filter(|s| matches!(s.kind, SourceKind::Device { .. }))
            .filter_map(|s| inner.sources.get(&s.id))
            .map(|rt| rt.processing.clone())
            .collect();
        let output = inner.config.output.clone();
        let (listening, hearing) = (!inner.listeners.is_empty(), hear_wanted(&inner));
        let fields = &mut *inner; // borrow both monitors at once
        let (listen, hear) = (&mut fields.listen, &mut fields.hear);
        changed |= self.supervise_monitor(
            listen,
            listening,
            output.as_ref(),
            default_output.as_ref(),
            now,
        );
        changed |=
            self.supervise_monitor(hear, hearing, output.as_ref(), default_output.as_ref(), now);
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
                    Capture::Backend {
                        target: CaptureTarget::Device(id),
                        processing: Some(rt.processing.clone()),
                    }
                }
                SourceKind::Soundboard => Capture::Soundboard(self.player.clone()),
                SourceKind::Music => Capture::Music(self.music.clone()),
                SourceKind::App { exe } => match found.get(exe).copied().flatten() {
                    Some(pid) => {
                        tracing::info!("source {:?}: found {exe} as PID {pid}", src.name);
                        rt.pid = Some(pid);
                        Capture::Backend {
                            target: CaptureTarget::Process(pid),
                            processing: None,
                        }
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

/// Validates and adds a source to the config (the caller bumps and
/// supervises).
fn push_source(
    inner: &mut Inner,
    name: Option<String>,
    kind: SourceKind,
) -> anyhow::Result<String> {
    let name = match (name.filter(|n| !n.trim().is_empty()), &kind) {
        (Some(n), _) => n.trim().to_owned(),
        (None, SourceKind::Device { device: Some(d) }) => d.name.clone(),
        (None, SourceKind::Device { device: None }) => "Default microphone".into(),
        (None, SourceKind::App { exe }) => display_exe(exe),
        (None, SourceKind::Soundboard) => "Soundboard".into(),
        (None, SourceKind::Music) => "Music".into(),
    };
    if let SourceKind::App { exe } = &kind {
        if exe.trim().is_empty() {
            bail!("executable name is required");
        }
        check_len("executable name", exe, 260)?;
    }
    check_len("name", &name, MAX_NAME_CHARS)?;
    if inner.config.sources.len() >= MAX_SOURCES {
        bail!("too many sources (max {MAX_SOURCES})");
    }
    let single = matches!(kind, SourceKind::Soundboard | SourceKind::Music);
    if single && inner.config.sources.iter().any(|s| s.kind == kind) {
        bail!("the {} is already in the mix", name.to_lowercase());
    }
    let id = new_id("s");
    let cfg = SourceConfig {
        id: id.clone(),
        name,
        kind,
        channel: Channel::default(),
        processing: Processing::default(),
    };
    inner.sources.insert(id.clone(), SourceRuntime::new(&cfg));
    inner.config.sources.push(cfg);
    Ok(id)
}

impl SourceRuntime {
    fn new(src: &SourceConfig) -> Self {
        let (channel, p) = (&src.channel, &src.processing);
        Self {
            ctrl: Arc::new(ChannelControl::new(
                db_to_gain(channel.gain_db),
                channel.muted,
            )),
            processing: Arc::new(ProcessingControl::new(
                p.noise_suppression,
                p.noise_gate,
                p.gate_mode == GateMode::Auto,
                p.gate_db,
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

/// Trims shorter than this are refused, so a sound can't vanish.
const MIN_SOUND_MS: u32 = 20;
const MAX_FADE_MS: u32 = 5_000;
const WAVEFORM_BUCKETS: usize = 400;

fn trim_of(s: &SoundConfig) -> Trim {
    Trim::from_ms(s.start_ms, s.end_ms, s.fade_in_ms, s.fade_out_ms)
}

/// Sorts `items` into the order of `ids`, which must name each item once.
fn reorder<T>(
    items: &mut [T],
    ids: &[String],
    id: impl Fn(&T) -> &String,
    what: &str,
) -> anyhow::Result<()> {
    let current: HashSet<&String> = items.iter().map(&id).collect();
    let wanted: HashSet<&String> = ids.iter().collect();
    if ids.len() != current.len() || current != wanted {
        bail!("order must list every {what} exactly once");
    }
    items.sort_by_key(|item| ids.iter().position(|i| i == id(item)));
    Ok(())
}

const MAX_PATH_CHARS: usize = 1_000;
const MAX_MUSIC_ITEMS: usize = 200;

/// Which of the Hear monitor's inputs is which.
const HEAR_SOUNDS: usize = 0;
const HEAR_MUSIC: usize = 1;

/// The Hear monitor plays while Hear sounds is on with the soundboard in
/// the mix, or Hear music with the music in it, and Listen is off (Listen
/// already includes both).
fn hear_wanted(inner: &Inner) -> bool {
    let has = |kind: SourceKind| inner.config.sources.iter().any(|s| s.kind == kind);
    inner.listeners.is_empty()
        && ((inner.config.hear_sounds && has(SourceKind::Soundboard))
            || (inner.config.music.hear && has(SourceKind::Music)))
}

fn display_exe(exe: &str) -> String {
    let stem = exe.trim().trim_end_matches(".exe").trim_end_matches(".EXE");
    let mut chars = stem.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => exe.to_owned(),
    }
}

/// What a source's capture thread records.
enum Capture {
    /// A device or app, through the backend. Devices get microphone
    /// processing.
    Backend {
        target: CaptureTarget,
        processing: Option<Arc<ProcessingControl>>,
    },
    Soundboard(Arc<Player>),
    Music(Arc<MusicPlayer>),
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capture::Backend { target, .. } => target.fmt(f),
            Capture::Soundboard(_) => f.write_str("Soundboard"),
            Capture::Music(_) => f.write_str("Music"),
        }
    }
}

fn capture_thread(
    backend: &dyn Backend,
    capture: Capture,
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
    let target = format!("{capture:?}");
    tracing::info!("opening capture {target}");
    let (opened, mut processor) = match capture {
        Capture::Backend { target, processing } => (
            backend.open_capture(&target),
            processing.map(Processor::new),
        ),
        Capture::Soundboard(player) => (
            Ok(Box::new(soundboard::Stream::new(player)) as Box<dyn CaptureStream>),
            None,
        ),
        Capture::Music(player) => (
            Ok(Box::new(music::Stream::new(player)) as Box<dyn CaptureStream>),
            None,
        ),
    };
    let mut stream = match opened {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("capture {target} failed: {e:#}");
            report("error", format!("{e:#}"));
            return;
        }
    };
    tracing::info!("capturing {target} ({})", stream.describe());
    report("running", stream.describe());
    while !stop.load(Ordering::Relaxed) {
        let result = stream.read(&mut |data| match &mut processor {
            Some(p) => p.process(data, &mut |out| push_frames(&mut tx, out)),
            None => push_frames(&mut tx, data),
        });
        if let Err(e) = result {
            tracing::warn!("capture {target} stopped: {e:#}");
            report("error", format!("{e:#}"));
            return;
        }
    }
}

fn render_thread(
    backend: &dyn Backend,
    device: Option<DeviceRef>,
    mixer: &Mutex<Mixer>,
    tap: &Mutex<Option<rtrb::Producer<f32>>>,
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
        let result = stream.render(&mut |buf| {
            mixer.lock().unwrap().process(buf);
            // Never block the audio thread on the tap; skip a block instead.
            if let Ok(mut tap) = tap.try_lock()
                && let Some(tx) = tap.as_mut()
            {
                push_frames(tx, buf);
            }
        });
        if let Err(e) = result {
            tracing::warn!("output failed: {e:#}");
            set_status(status, "error", format!("{e:#}"));
            changed.send_modify(|v| *v += 1);
            stream = Box::new(NullRender::default());
            retry_at = Some(Instant::now() + RETRY_DELAY);
        }
    }
}

/// Plays a monitor's tap (via `mixer`) on the Windows default output.
fn monitor_thread(
    backend: &dyn Backend,
    mut mixer: Mixer,
    dev: DeviceInfo,
    what: &str,
    status: &SharedStatus,
    stop: &AtomicBool,
    changed: &tokio::sync::watch::Sender<u64>,
) {
    // Once stopped (replaced or turned off), stay quiet. Errors end the
    // thread; the supervisor retries.
    let fail = |msg: String| {
        tracing::warn!("{what}: {msg}");
        if !stop.load(Ordering::Relaxed) {
            set_status(status, "error", msg);
            changed.send_modify(|v| *v += 1);
        }
    };
    backend.promote_audio_thread();
    let mut stream = match backend.open_render(&dev.id) {
        Ok(s) => s,
        Err(e) => return fail(format!("{}: {e:#}", dev.name)),
    };
    tracing::info!("{what}: playing on {:?}", dev.name);
    if stop.load(Ordering::Relaxed) {
        return;
    }
    set_status(status, "running", dev.name);
    changed.send_modify(|v| *v += 1);
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = stream.render(&mut |buf| mixer.process(buf)) {
            return fail(format!("{e:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AppInfo, CaptureStream};

    /// Two outputs ("cable", "phones"; phones is the Windows default), no
    /// inputs or apps, and renders that just tick.
    struct TestBackend;

    impl Backend for TestBackend {
        fn input_devices(&self) -> anyhow::Result<Vec<DeviceInfo>> {
            Ok(Vec::new())
        }
        fn output_devices(&self) -> anyhow::Result<Vec<DeviceInfo>> {
            Ok(vec![device("cable"), device("phones")])
        }
        fn default_input(&self) -> Option<DeviceInfo> {
            None
        }
        fn default_output(&self) -> Option<DeviceInfo> {
            Some(device("phones"))
        }
        fn audio_apps(&self) -> anyhow::Result<Vec<AppInfo>> {
            Ok(Vec::new())
        }
        fn find_process(&self, _exe: &str) -> Option<u32> {
            None
        }
        fn process_alive(&self, _pid: u32) -> bool {
            false
        }
        fn open_capture(&self, _target: &CaptureTarget) -> anyhow::Result<Box<dyn CaptureStream>> {
            bail!("no captures in tests")
        }
        fn open_render(&self, _device_id: &str) -> anyhow::Result<Box<dyn RenderStream>> {
            Ok(Box::new(NullRender::default()))
        }
    }

    fn device(id: &str) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: id.into(),
            is_default: id == "phones",
        }
    }

    /// An engine with app sources "a", "b", "c" mixing to `output`. Its config
    /// is saved under a temp dir, never the user's.
    fn engine(output: &str) -> Arc<Engine> {
        static ISOLATE: std::sync::Once = std::sync::Once::new();
        ISOLATE.call_once(|| {
            let dir = std::env::temp_dir().join(format!("vam-tests-{}", std::process::id()));
            // SAFETY: set once, before any test reads the environment.
            unsafe {
                std::env::set_var("APPDATA", &dir);
                std::env::set_var("XDG_CONFIG_HOME", &dir);
            }
        });
        let sources = ["a", "b", "c"]
            .map(|id| SourceConfig {
                id: id.into(),
                name: id.into(),
                kind: SourceKind::App {
                    exe: format!("{id}.exe"),
                },
                channel: Channel::default(),
                processing: Processing::default(),
            })
            .to_vec();
        let config = Config {
            output: Some(DeviceRef {
                id: output.into(),
                name: output.into(),
            }),
            sources,
            ..Config::default()
        };
        Engine::new(Box::new(TestBackend), config)
    }

    fn order(engine: &Engine) -> Vec<String> {
        engine.state().sources.into_iter().map(|s| s.id).collect()
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Waits up to 3 s (the supervisor ticks every 500 ms) for a state.
    fn wait_monitor(engine: &Engine, state: &str) -> Status {
        for _ in 0..300 {
            let status = engine.state().monitor;
            if status.state == state {
                return status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        engine.state().monitor
    }

    #[test]
    fn reorder_requires_every_source_once() {
        let engine = engine("cable");
        engine.reorder_sources(&ids(&["c", "a", "b"])).unwrap();
        assert_eq!(order(&engine), ids(&["c", "a", "b"]));
        for bad in [&["c", "c", "a"][..], &["c", "a"], &["c", "a", "zz"], &[]] {
            assert!(engine.reorder_sources(&ids(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(
            order(&engine),
            ids(&["c", "a", "b"]),
            "rejected orders change nothing"
        );
        engine.shutdown();
    }

    #[test]
    fn listen_runs_while_any_connection_has_it_on() {
        let engine = engine("cable");
        assert_eq!(engine.state().monitor.state, "idle");
        engine.set_listening(1, true);
        assert_eq!(wait_monitor(&engine, "running").state, "running");
        engine.set_listening(2, true);
        engine.set_listening(1, false);
        assert_eq!(
            engine.state().monitor.state,
            "running",
            "connection 2 still listening"
        );
        engine.set_listening(7, false); // never turned it on: no effect
        assert_eq!(engine.state().monitor.state, "running");
        engine.set_listening(2, false);
        assert_eq!(engine.state().monitor.state, "idle");

        engine.shutdown();
        engine.set_listening(3, true);
        assert_eq!(
            engine.state().monitor.state,
            "idle",
            "ignored after shutdown"
        );
    }

    #[test]
    fn listen_refuses_when_the_mix_already_goes_to_the_default_output() {
        let engine = engine("phones");
        engine.set_listening(1, true);
        let status = wait_monitor(&engine, "error");
        assert_eq!(status.state, "error");
        assert!(status.message.contains("phones"), "{}", status.message);

        // Moving the mix to the cable lets it start without toggling Listen.
        engine.set_output(Some(DeviceRef {
            id: "cable".into(),
            name: "cable".into(),
        }));
        assert_eq!(wait_monitor(&engine, "running").state, "running");
        engine.shutdown();
    }

    /// A short 16-bit mono WAV file.
    fn wav() -> Vec<u8> {
        wav_of(4_800)
    }

    /// A 16-bit mono 48 kHz WAV file `frames` long.
    fn wav_of(frames: usize) -> Vec<u8> {
        let samples = vec![1000i16; frames];
        let data_len = (samples.len() * 2) as u32;
        let mut f = Vec::new();
        f.extend(b"RIFF");
        f.extend((36 + data_len).to_le_bytes());
        f.extend(b"WAVEfmt ");
        for v in [16u32, 1 | (1 << 16), 48_000, 96_000, 2 | (16 << 16)] {
            f.extend(v.to_le_bytes());
        }
        f.extend(b"data");
        f.extend(data_len.to_le_bytes());
        f.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        f
    }

    fn hotkey(action: HotkeyAction, target: &str) -> Hotkey {
        Hotkey {
            id: String::new(),
            keys: "F1".into(),
            action,
            target: target.into(),
        }
    }

    fn soundboards(engine: &Engine) -> Vec<SourceView> {
        engine
            .state()
            .sources
            .into_iter()
            .filter(|s| s.kind == SourceKind::Soundboard)
            .collect()
    }

    #[test]
    fn sounds_bring_the_soundboard_play_and_clean_up() {
        let engine = engine("cable");
        let id = engine.add_sound(None, "Air Horn.wav", wav()).unwrap();
        let sound = &engine.state().sounds[0];
        assert_eq!((sound.name.as_str(), sound.state), ("Air Horn", "ready"));
        assert_eq!(
            soundboards(&engine).len(),
            1,
            "first sound adds the soundboard"
        );
        let two = engine
            .add_sound(Some("Two".into()), "two.wav", wav())
            .unwrap();
        assert!(engine.add_source(None, SourceKind::Soundboard).is_err());
        assert_eq!(soundboards(&engine).len(), 1, "only ever one");

        // The soundboard's capture runs without a backend.
        let mut status = String::new();
        for _ in 0..300 {
            status = soundboards(&engine)[0].status.state.to_owned();
            if status == "running" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(status, "running");

        engine.play_sound(&id).unwrap();
        assert_eq!(engine.playing_sounds(), vec![id.clone()]);
        engine.stop_sounds();
        assert!(engine.playing_sounds().is_empty());

        let err = engine
            .add_sound(None, "x.mp3", b"junk".to_vec())
            .unwrap_err();
        assert!(err.to_string().contains("supported"), "{err}");
        assert_eq!(
            engine.state().sounds.len(),
            2,
            "a rejected file isn't added"
        );

        engine
            .set_hotkeys(vec![
                hotkey(HotkeyAction::PlaySound, &id),
                hotkey(HotkeyAction::StopSounds, "anything"),
                hotkey(HotkeyAction::PlaySound, &two),
            ])
            .unwrap();
        assert_eq!(engine.hotkeys()[1].target, "", "stop needs no target");
        assert!(
            engine
                .set_hotkeys(vec![hotkey(HotkeyAction::PlaySound, MASTER)])
                .is_err()
        );
        assert!(
            engine
                .set_hotkeys(vec![hotkey(HotkeyAction::ToggleMute, &id)])
                .is_err(),
            "a sound isn't a channel"
        );

        let file = {
            let inner = engine.inner.lock().unwrap();
            soundboard::sounds_dir().join(&inner.config.sounds[0].file)
        };
        assert!(file.exists());
        engine.remove_sound(&id).unwrap();
        assert!(!file.exists(), "file deleted");
        assert_eq!(engine.hotkeys().len(), 2, "its hotkey went with it");
        assert!(engine.play_sound(&id).is_err());
        engine.shutdown();
    }

    /// Waits up to 3 s for Hear sounds to reach a state.
    fn wait_hear(engine: &Engine, state: &str) -> Status {
        for _ in 0..300 {
            let status = engine.state().hear_status;
            if status.state == state {
                return status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        engine.state().hear_status
    }

    #[test]
    fn hear_sounds_plays_unless_listen_is_on() {
        let engine = engine("cable");
        assert_eq!(
            engine.state().hear_status.state,
            "idle",
            "no soundboard yet"
        );
        engine.add_sound(None, "a.wav", wav()).unwrap();
        assert_eq!(wait_hear(&engine, "running").state, "running");

        // Listen includes the soundboard, so Hear sounds pauses meanwhile.
        engine.set_listening(1, true);
        assert_eq!(wait_hear(&engine, "idle").state, "idle");
        assert_eq!(wait_monitor(&engine, "running").state, "running");
        engine.set_listening(1, false);
        assert_eq!(wait_hear(&engine, "running").state, "running");

        let off = SoundboardPatch {
            hear_sounds: Some(false),
            hear_sounds_db: Some(-6.0),
        };
        engine.update_soundboard(off).unwrap();
        assert_eq!(engine.state().hear_status.state, "idle");
        assert_eq!(engine.state().hear_sounds_db, -6.0);
        engine.shutdown();
    }

    #[test]
    fn hear_sounds_refuses_the_mix_output() {
        let engine = engine("phones");
        engine.add_sound(None, "a.wav", wav()).unwrap();
        let status = wait_hear(&engine, "error");
        assert!(status.message.contains("hear sounds"), "{}", status.message);
        engine.shutdown();
    }

    #[test]
    fn trims_are_validated_and_saved() {
        let engine = engine("cable");
        let id = engine.add_sound(None, "a.wav", wav()).unwrap(); // 100 ms
        let patch = |start: Option<u32>, end: Option<Option<u32>>| SoundPatch {
            start_ms: start,
            end_ms: end,
            ..Default::default()
        };
        let sound = |e: &Engine| e.state().sounds.into_iter().find(|s| s.id == id).unwrap();
        assert_eq!(sound(&engine).duration_ms, Some(100));

        engine
            .update_sound(&id, patch(Some(10), Some(Some(60))))
            .unwrap();
        let s = sound(&engine);
        assert_eq!((s.start_ms, s.end_ms), (10, Some(60)));

        assert!(
            engine.update_sound(&id, patch(Some(55), None)).is_err(),
            "under 20 ms"
        );
        assert_eq!(sound(&engine).start_ms, 10, "rejected trims change nothing");

        engine
            .update_sound(&id, patch(None, Some(Some(5_000))))
            .unwrap();
        assert_eq!(
            sound(&engine).end_ms,
            None,
            "an end past the sound means the end"
        );
        engine
            .update_sound(&id, patch(None, Some(Some(60))))
            .unwrap();
        engine.update_sound(&id, patch(None, Some(None))).unwrap();
        assert_eq!(sound(&engine).end_ms, None, "null resets the end");

        let fade = SoundPatch {
            fade_in_ms: Some(6_000),
            ..Default::default()
        };
        assert!(engine.update_sound(&id, fade).is_err());
        let w = engine.sound_waveform(&id).unwrap();
        assert_eq!(w.duration_ms, 100);

        // JSON: a null end resets it; a missing one leaves it alone.
        let p: SoundPatch = serde_json::from_str(r#"{"end_ms":null}"#).unwrap();
        assert_eq!(p.end_ms, Some(None));
        let p: SoundPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(p.end_ms, None);
        engine.shutdown();
    }

    #[test]
    fn music_plays_a_folder_and_moves_on_by_itself() {
        let engine = engine("cable");
        let dir = std::env::temp_dir().join(format!("vam-music-{}", new_id("d")));
        std::fs::create_dir_all(&dir).unwrap();
        // A second each: longer than the decoder's half-second lead, so
        // the current track follows playback rather than decoding.
        for name in ["1 first.wav", "2 second.wav", "notes.txt"] {
            std::fs::write(dir.join(name), wav_of(48_000)).unwrap();
        }
        let folder = dir.to_string_lossy().into_owned();

        assert!(engine.add_music("relative/path").is_err());
        assert!(engine.add_music(&format!("{folder}/missing")).is_err());
        let err = engine
            .add_music(&dir.join("notes.txt").to_string_lossy())
            .unwrap_err();
        assert!(err.to_string().contains("audio"), "{err}");

        let id = engine.add_music(&folder).unwrap();
        assert!(engine.add_music(&folder).is_err(), "no duplicates");
        let state = engine.state();
        assert_eq!(state.music.player.tracks, ["1 first", "2 second"]);
        assert!(
            state.sources.iter().any(|s| s.kind == SourceKind::Music),
            "the first music adds the player to the mix"
        );

        engine
            .music_control(MusicControl::Play { track: 0 })
            .unwrap();
        // The music source's thread plays the 100 ms tracks back to back.
        let mut current = Vec::new();
        for _ in 0..600 {
            let c = engine.state().music.player.current;
            if current.last() != Some(&c) {
                current.push(c);
            }
            if current.len() >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            current,
            [Some(0), Some(1), Some(0)],
            "plays on, and repeats the list (the default)"
        );
        engine.music_control(MusicControl::Pause).unwrap();
        assert!(!engine.state().music.player.playing);

        engine.remove_music(&id).unwrap();
        assert!(engine.state().music.player.tracks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        engine.shutdown();
    }

    #[test]
    fn reorder_sounds_requires_every_sound_once() {
        let engine = engine("cable");
        let a = engine.add_sound(Some("a".into()), "a.wav", wav()).unwrap();
        let b = engine.add_sound(Some("b".into()), "b.wav", wav()).unwrap();
        engine.reorder_sounds(&[b.clone(), a.clone()]).unwrap();
        let names = |e: &Engine| {
            e.state()
                .sounds
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&engine), ["b", "a"]);
        assert!(engine.reorder_sounds(std::slice::from_ref(&b)).is_err());
        assert!(engine.reorder_sounds(&[b.clone(), b.clone()]).is_err());
        assert_eq!(names(&engine), ["b", "a"], "rejected orders change nothing");
        engine.shutdown();
    }

    #[test]
    fn removing_the_soundboard_stops_its_sounds() {
        let engine = engine("cable");
        let id = engine.add_sound(None, "a.wav", wav()).unwrap();
        engine.play_sound(&id).unwrap();
        let board = soundboards(&engine)[0].id.clone();
        engine.remove_source(&board).unwrap();
        assert!(engine.playing_sounds().is_empty(), "nothing left queued");
        let err = engine.play_sound(&id).unwrap_err();
        assert!(err.to_string().contains("isn't in the mix"), "{err}");
        engine.add_source(None, SourceKind::Soundboard).unwrap();
        engine.play_sound(&id).unwrap();
        engine.shutdown();
    }

    #[test]
    fn a_failed_upload_leaves_nothing_behind() {
        let engine = engine("cable");
        // Fill the mix so the soundboard can't be added.
        for i in 3..MAX_SOURCES {
            let kind = SourceKind::App {
                exe: format!("{i}.exe"),
            };
            engine.add_source(None, kind).unwrap();
        }
        // Other tests share the sounds folder; only this one stores .flac.
        let files = || {
            std::fs::read_dir(soundboard::sounds_dir()).map_or(0, |d| {
                d.filter(|e| {
                    e.as_ref()
                        .is_ok_and(|e| e.path().extension() == Some("flac".as_ref()))
                })
                .count()
            })
        };
        let before = files();
        let err = engine.add_sound(None, "a.flac", wav()).unwrap_err();
        assert!(err.to_string().contains("too many sources"), "{err}");
        assert!(engine.state().sounds.is_empty());
        assert_eq!(files(), before, "file removed again");
        engine.shutdown();
    }

    #[test]
    fn saved_sounds_load_at_startup() {
        engine("cable").shutdown(); // isolates the data dir
        std::fs::create_dir_all(soundboard::sounds_dir()).unwrap();
        let file = format!("{}.wav", new_id("t"));
        std::fs::write(soundboard::sounds_dir().join(&file), wav()).unwrap();
        let sound = |id: &str, file: &str| SoundConfig {
            id: id.into(),
            name: id.into(),
            file: file.into(),
            ..Default::default()
        };
        let config = Config {
            sounds: vec![
                sound("good", &file),
                sound("missing", "nope.wav"),
                sound("escape", "../config.json"),
            ],
            ..Config::default()
        };
        let engine = Engine::new(Box::new(TestBackend), config);
        let mut states = Vec::new();
        for _ in 0..300 {
            states = engine.state().sounds.iter().map(|s| s.state).collect();
            if !states.contains(&"loading") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(states, ["ready", "error", "error"]);
        engine.add_source(None, SourceKind::Soundboard).unwrap();
        engine.play_sound("good").unwrap();
        let err = engine.play_sound("missing").unwrap_err();
        assert!(err.to_string().contains("nope.wav"), "{err}");
        engine.shutdown();
    }

    #[test]
    fn processing_is_for_microphones_only() {
        let engine = engine("cable");
        let suppress = || ChannelPatch {
            noise_suppression: Some(true),
            ..Default::default()
        };
        assert!(
            engine.update_channel("a", suppress()).is_err(),
            "app source"
        );
        assert!(engine.update_channel(MASTER, suppress()).is_err());

        let mic = engine
            .add_source(None, SourceKind::Device { device: None })
            .unwrap();
        engine
            .update_channel(
                &mic,
                ChannelPatch {
                    noise_suppression: Some(true),
                    noise_gate: Some(true),
                    gate_db: Some(-200.0),
                    ..Default::default()
                },
            )
            .unwrap();
        // A bad value rejects the whole patch, applying nothing.
        let bad = ChannelPatch {
            noise_gate: Some(false),
            gate_db: Some(f32::INFINITY),
            ..Default::default()
        };
        assert!(engine.update_channel(&mic, bad).is_err());
        // A bad value rejects the whole patch, applying nothing.
        let bad = ChannelPatch {
            noise_gate: Some(false),
            gate_db: Some(f32::INFINITY),
            ..Default::default()
        };
        assert!(engine.update_channel(&mic, bad).is_err());
        let src = engine
            .state()
            .sources
            .into_iter()
            .find(|s| s.id == mic)
            .unwrap();
        assert!(src.processing.noise_suppression && src.processing.noise_gate);
        assert_eq!(src.processing.gate_db, -60.0, "clamped");
        let meters = engine.meters();
        assert!(meters[&mic].mic_level.is_some());
        assert!(meters["a"].mic_level.is_none());
        engine.shutdown();
    }

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
