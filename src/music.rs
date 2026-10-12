//! Music: a playlist of audio files and folders, streamed from disk into
//! the mix as one source.
//!
//! A decode thread reads the current track a packet at a time into a short
//! feed (half a second); the music source's capture thread ([`Stream`])
//! plays from that feed in real time. Jumping (play a track, next, seek)
//! gives the stream a brand-new feed rather than flushing the old one, so
//! stale audio is never heard. Tracks play back to back from one feed, so
//! there's no gap between them.

use crate::backend::CaptureStream;
pub use crate::config::Repeat;
use crate::decode::{AUDIO_EXTENSIONS, TrackDecoder};
use crate::dsp::ProcessingControl;
use crate::mixer::{CHANNELS, MonitorTap, SAMPLE_RATE, push_frames};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

/// Half a second of audio between the decoder and the player.
const FEED_SAMPLES: usize = SAMPLE_RATE as usize * CHANNELS / 2;
const BLOCK_FRAMES: usize = SAMPLE_RATE as usize / 100;
pub const MAX_TRACKS: usize = 10_000;
const MAX_DEPTH: usize = 8;
/// "Previous" within this far into a track restarts it instead.
const RESTART_MS: u64 = 3_000;

#[derive(Debug, Clone)]
pub struct Track {
    pub path: PathBuf,
    pub name: String,
}

/// Finds the audio files in `paths` (files, and folders searched
/// recursively), in path order, without duplicates.
pub fn scan(paths: &[String]) -> Vec<Track> {
    let mut found = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            let mut files = Vec::new();
            walk(path, 0, &mut files);
            files.sort_by_key(|f| f.to_string_lossy().to_lowercase());
            found.extend(files);
        } else if is_audio(path) {
            found.push(path.to_path_buf());
        }
        if found.len() >= MAX_TRACKS {
            break;
        }
    }
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .take(MAX_TRACKS)
        .map(|path| Track {
            name: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
        })
        .collect()
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || out.len() >= MAX_TRACKS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, depth + 1, out),
            Ok(t) if t.is_file() && is_audio(&path) => out.push(path),
            _ => {}
        }
    }
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A small, good-enough shuffle (no dependency for one feature).
fn shuffled(n: usize, first: Option<usize>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    for i in (1..n).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        order.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    if let Some(f) = first
        && let Some(p) = order.iter().position(|&t| t == f)
    {
        order.swap(0, p);
    }
    order
}

struct State {
    tracks: Vec<Track>,
    /// Play order: indexes into `tracks`, shuffled or not.
    order: Vec<usize>,
    /// Position in `order` of the current track.
    at: Option<usize>,
    playing: bool,
    shuffle: bool,
    repeat: Repeat,
    /// Bumped by every jump; the decoder starts a new feed for it.
    generation: u64,
    /// Where the current generation starts in its track.
    start_ms: u64,
    /// Now playing, as far as the decoder has got.
    title: Option<String>,
    artist: Option<String>,
    duration_ms: Option<u64>,
    error: Option<String>,
    shutdown: bool,
}

impl State {
    fn current(&self) -> Option<usize> {
        self.at.and_then(|a| self.order.get(a).copied())
    }

    /// Moves to the next track per `repeat`. With `user` (pressing Next),
    /// repeat-one moves on too. Returns false at the end of the list.
    fn advance(&mut self, user: bool) -> bool {
        let n = self.order.len();
        let Some(at) = self.at else {
            return false;
        };
        if n == 0 {
            return false;
        }
        if self.repeat == Repeat::One && !user {
            return true;
        }
        if at + 1 < n {
            self.at = Some(at + 1);
            true
        } else if self.repeat != Repeat::Off {
            if self.shuffle {
                self.order = shuffled(n, None);
            }
            self.at = Some(0);
            true
        } else {
            false
        }
    }

    fn jump(&mut self, start_ms: u64) {
        self.generation += 1;
        self.start_ms = start_ms;
        self.title = None;
        self.artist = None;
        self.duration_ms = None;
    }
}

/// What the UI shows.
#[derive(Debug, Clone, Serialize)]
pub struct MusicView {
    pub tracks: Vec<String>,
    /// Index into `tracks` of the current track.
    pub current: Option<usize>,
    pub playing: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// Ducking: turns the music down while you talk.
#[derive(Default)]
pub struct Duck {
    pub enabled: AtomicBool,
    /// Gain while ducked (linear, f32 bits).
    gain: AtomicU32,
    /// The microphones to listen to.
    pub mics: Mutex<Vec<Arc<ProcessingControl>>>,
}

impl Duck {
    pub fn set_db(&self, db: f32) {
        self.gain
            .store(crate::config::db_to_gain(db).to_bits(), Ordering::Relaxed);
    }

    /// Whether anyone's mic hears speech: the noise gate's verdict where it's
    /// on, otherwise a plain level check.
    fn talking(&self) -> bool {
        const LEVEL: f32 = 0.0178; // -35 dBFS
        let Ok(mics) = self.mics.try_lock() else {
            return false;
        };
        mics.iter().any(|m| {
            if m.gate_enabled() {
                m.gate_open() && m.level() > 0.0
            } else {
                m.level() > LEVEL
            }
        })
    }
}

pub struct MusicPlayer {
    state: Mutex<State>,
    wake: Condvar,
    /// A new feed for the stream to switch to.
    feed: Mutex<Option<rtrb::Consumer<f32>>>,
    /// Frames the decoder has produced in the current generation, and how
    /// many of those are still waiting in the feed: the difference is where
    /// playback is.
    decoded: AtomicU64,
    buffered: AtomicU64,
    playing: AtomicBool,
    pub duck: Duck,
    /// Hear music: a copy of the music for your headphones.
    pub tap: MonitorTap,
    /// Tells the UI what's playing changed (a new track, the end, an error).
    notify: Box<dyn Fn() + Send + Sync>,
}

impl MusicPlayer {
    pub fn new(
        shuffle: bool,
        repeat: Repeat,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        let player = Arc::new(Self {
            state: Mutex::new(State {
                tracks: Vec::new(),
                order: Vec::new(),
                at: None,
                playing: false,
                shuffle,
                repeat,
                generation: 0,
                start_ms: 0,
                title: None,
                artist: None,
                duration_ms: None,
                error: None,
                shutdown: false,
            }),
            wake: Condvar::new(),
            feed: Mutex::new(None),
            decoded: AtomicU64::new(0),
            buffered: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            duck: Duck::default(),
            tap: MonitorTap::default(),
            notify: Box::new(notify),
        });
        let weak = Arc::downgrade(&player);
        std::thread::Builder::new()
            .name("music decoder".into())
            .spawn(move || decode_loop(weak))
            .expect("spawn music decoder");
        player
    }

    pub fn shutdown(&self) {
        self.state.lock().unwrap().shutdown = true;
        self.wake.notify_all();
    }

    /// Replaces the track list, keeping the current track if it's still
    /// there.
    pub fn set_tracks(&self, tracks: Vec<Track>) {
        let mut s = self.state.lock().unwrap();
        let current = s.current().map(|i| s.tracks[i].path.clone());
        let keep = current.and_then(|p| tracks.iter().position(|t| t.path == p));
        s.tracks = tracks;
        let n = s.tracks.len();
        s.order = if s.shuffle {
            shuffled(n, keep)
        } else {
            (0..n).collect()
        };
        s.at = keep.and_then(|k| s.order.iter().position(|&t| t == k));
        if s.at.is_none() && s.playing {
            // The playing track was removed.
            s.playing = false;
            s.jump(0);
            self.playing.store(false, Ordering::Relaxed);
        }
        self.wake.notify_all();
    }

    pub fn view(&self) -> MusicView {
        let s = self.state.lock().unwrap();
        MusicView {
            tracks: s.tracks.iter().map(|t| t.name.clone()).collect(),
            current: s.current(),
            playing: s.playing,
            title: s.title.clone(),
            artist: s.artist.clone(),
            duration_ms: s.duration_ms,
            error: s.error.clone(),
        }
    }

    /// Where playback is in the current track.
    pub fn position_ms(&self) -> u64 {
        let start = self.state.lock().unwrap().start_ms;
        let frames = self
            .decoded
            .load(Ordering::Relaxed)
            .saturating_sub(self.buffered.load(Ordering::Relaxed));
        start + frames * 1000 / SAMPLE_RATE as u64
    }

    /// Plays a track (index into the list) from the start.
    pub fn play_track(&self, track: usize) -> bool {
        let mut s = self.state.lock().unwrap();
        let Some(at) = s.order.iter().position(|&t| t == track) else {
            return false;
        };
        s.at = Some(at);
        s.jump(0);
        self.set_playing(&mut s, true);
        true
    }

    /// Play/pause. Playing with nothing chosen starts the first track.
    pub fn toggle(&self) {
        let mut s = self.state.lock().unwrap();
        if s.at.is_none() {
            if s.order.is_empty() {
                return;
            }
            s.at = Some(0);
            s.jump(0);
        }
        let on = !s.playing;
        self.set_playing(&mut s, on);
    }

    pub fn pause(&self) {
        let mut s = self.state.lock().unwrap();
        self.set_playing(&mut s, false);
    }

    pub fn next(&self) {
        let mut s = self.state.lock().unwrap();
        if s.advance(true) {
            s.jump(0);
            self.set_playing(&mut s, true);
        }
    }

    /// Back to the start of the track, or the previous one if it only just
    /// started.
    pub fn previous(&self) {
        let position = self.position_ms();
        let mut s = self.state.lock().unwrap();
        let Some(at) = s.at else {
            return;
        };
        if position < RESTART_MS {
            if at > 0 {
                s.at = Some(at - 1);
            } else if s.repeat == Repeat::All && !s.order.is_empty() {
                s.at = Some(s.order.len() - 1);
            }
        }
        s.jump(0);
        self.set_playing(&mut s, true);
    }

    pub fn seek(&self, ms: u64) {
        let mut s = self.state.lock().unwrap();
        if s.at.is_some() {
            s.jump(ms);
            self.wake.notify_all();
        }
    }

    pub fn set_shuffle(&self, on: bool) {
        let mut s = self.state.lock().unwrap();
        if s.shuffle == on {
            return;
        }
        s.shuffle = on;
        let current = s.current();
        let n = s.tracks.len();
        s.order = if on {
            shuffled(n, current)
        } else {
            (0..n).collect()
        };
        s.at = current.and_then(|c| s.order.iter().position(|&t| t == c));
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        self.state.lock().unwrap().repeat = repeat;
    }

    fn set_playing(&self, s: &mut State, on: bool) {
        s.playing = on && s.at.is_some();
        if s.playing {
            s.error = None;
        }
        self.playing.store(s.playing, Ordering::Relaxed);
        self.wake.notify_all();
    }
}

/// The decode thread: keeps the feed topped up with the current track.
fn decode_loop(player: Weak<MusicPlayer>) {
    loop {
        // Wait for something to play.
        let (generation, track, start_ms) = {
            let Some(p) = player.upgrade() else { return };
            let mut s = p.state.lock().unwrap();
            loop {
                if s.shutdown {
                    return;
                }
                if s.playing && s.current().is_some() {
                    break;
                }
                s = p
                    .wake
                    .wait_timeout(s, Duration::from_millis(500))
                    .unwrap()
                    .0;
            }
            (s.generation, s.current().unwrap(), s.start_ms)
        };
        let Some(p) = player.upgrade() else { return };
        let (mut tx, rx) = rtrb::RingBuffer::new(FEED_SAMPLES);
        *p.feed.lock().unwrap() = Some(rx);
        p.decoded.store(0, Ordering::Relaxed);
        p.buffered.store(0, Ordering::Relaxed);
        stream_tracks(&p, generation, track, start_ms, &mut tx);
    }
}

/// Decodes tracks back to back into `tx` until a jump, a stop, or the end
/// of the list.
fn stream_tracks(
    p: &MusicPlayer,
    generation: u64,
    mut track: usize,
    mut start_ms: u64,
    tx: &mut rtrb::Producer<f32>,
) {
    let mut failures = 0;
    let mut buf = Vec::new();
    loop {
        let path = p.state.lock().unwrap().tracks[track].path.clone();
        let opened = TrackDecoder::open_path(&path).and_then(|mut d| {
            if start_ms > 0 {
                d.seek(start_ms)?;
            }
            Ok(d)
        });
        let mut decoder = match opened {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("music: {}: {e:#}", path.display());
                let mut s = p.state.lock().unwrap();
                if s.generation != generation {
                    return;
                }
                failures += 1;
                s.error = Some(format!("{}: {e:#}", path.display()));
                // Skip it; give up if nothing in the list will play.
                if failures >= s.order.len().min(10) || !s.advance(true) {
                    p.set_playing(&mut s, false);
                    drop(s);
                    (p.notify)();
                    return;
                }
                track = s.current().unwrap();
                start_ms = 0;
                drop(s);
                (p.notify)();
                continue;
            }
        };
        {
            let mut s = p.state.lock().unwrap();
            if s.generation != generation {
                return;
            }
            failures = 0;
            s.title = decoder.title.clone();
            s.artist = decoder.artist.clone();
            s.duration_ms = decoder.duration_ms;
        }
        (p.notify)();
        loop {
            {
                let s = p.state.lock().unwrap();
                if s.generation != generation || s.shutdown {
                    return;
                }
                // Paused, or the feed is full: wait without losing our place.
                if !s.playing || tx.slots() < FEED_SAMPLES / 3 {
                    drop(p.wake.wait_timeout(s, Duration::from_millis(20)).unwrap());
                    continue;
                }
            }
            buf.clear();
            let more = match decoder.next(&mut buf) {
                Ok(more) => more,
                Err(e) => {
                    tracing::warn!("music: {}: {e:#}", path.display());
                    p.state.lock().unwrap().error = Some(format!("{}: {e:#}", path.display()));
                    false
                }
            };
            // A packet can be bigger than the free space: hand it over in
            // pieces as the player drains the feed.
            let mut rest = &buf[..];
            while !rest.is_empty() {
                let n = rest.len().min(tx.slots()) / CHANNELS * CHANNELS;
                push_frames(tx, &rest[..n]);
                p.decoded
                    .fetch_add((n / CHANNELS) as u64, Ordering::Relaxed);
                rest = &rest[n..];
                if !rest.is_empty() {
                    let s = p.state.lock().unwrap();
                    if s.generation != generation || s.shutdown {
                        return;
                    }
                    drop(p.wake.wait_timeout(s, Duration::from_millis(10)).unwrap());
                }
            }
            if !more {
                break;
            }
        }
        // The track ended: carry on into the next one in the same feed.
        let mut s = p.state.lock().unwrap();
        if s.generation != generation {
            return;
        }
        if !s.advance(false) {
            p.set_playing(&mut s, false);
            s.jump(0);
            drop(s);
            (p.notify)();
            return;
        }
        // Positions count from the new track's start.
        s.start_ms = 0;
        s.title = None;
        s.artist = None;
        s.duration_ms = None;
        p.decoded.store(0, Ordering::Relaxed);
        p.buffered.store(0, Ordering::Relaxed);
        track = s.current().unwrap();
        start_ms = 0;
    }
}

/// The music source's audio, played from the decoder's feed in real time.
pub struct Stream {
    player: Arc<MusicPlayer>,
    rx: Option<rtrb::Consumer<f32>>,
    next: Instant,
    buf: Vec<f32>,
    duck_gain: f32,
    /// Blocks left to stay ducked after the talking stops.
    duck_hold: u32,
}

impl Stream {
    pub fn new(player: Arc<MusicPlayer>) -> Self {
        Self {
            player,
            rx: None,
            next: Instant::now(),
            buf: vec![0.0; BLOCK_FRAMES * CHANNELS],
            duck_gain: 1.0,
            duck_hold: 0,
        }
    }

    /// The ducking gain to reach by the end of this block.
    fn duck_target(&mut self) -> f32 {
        let duck = &self.player.duck;
        if !duck.enabled.load(Ordering::Relaxed) {
            self.duck_hold = 0;
            return 1.0;
        }
        if duck.talking() {
            self.duck_hold = 30; // 300 ms
        } else {
            self.duck_hold = self.duck_hold.saturating_sub(1);
        }
        if self.duck_hold > 0 {
            f32::from_bits(duck.gain.load(Ordering::Relaxed))
        } else {
            1.0
        }
    }
}

impl CaptureStream for Stream {
    fn read(&mut self, sink: &mut dyn FnMut(&[f32])) -> anyhow::Result<()> {
        let period = Duration::from_secs_f64(BLOCK_FRAMES as f64 / SAMPLE_RATE as f64);
        let now = Instant::now();
        if now > self.next + 20 * period {
            self.next = now; // fell far behind: don't burst
        }
        if self.next > now {
            std::thread::sleep(self.next - now);
        }
        self.next += period;

        if let Ok(mut feed) = self.player.feed.try_lock()
            && let Some(rx) = feed.take()
        {
            self.rx = Some(rx); // a jump: the old feed's audio is dropped
        }
        self.buf.fill(0.0);
        if self.player.playing.load(Ordering::Relaxed)
            && let Some(rx) = &mut self.rx
        {
            let n = rx.slots().min(self.buf.len()) / CHANNELS * CHANNELS;
            if let Ok(chunk) = rx.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                self.buf[..a.len()].copy_from_slice(a);
                self.buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
            self.player
                .buffered
                .store((rx.slots() / CHANNELS) as u64, Ordering::Relaxed);
        }

        // Ducking: ramp across the block, quicker down than up.
        let target = self.duck_target();
        let step = if target < self.duck_gain { 0.25 } else { 0.03 };
        let end = self.duck_gain + (target - self.duck_gain).clamp(-step, step);
        if end != self.duck_gain || end != 1.0 {
            let frames = (self.buf.len() / CHANNELS) as f32;
            for (i, frame) in self
                .buf
                .as_chunks_mut::<CHANNELS>()
                .0
                .iter_mut()
                .enumerate()
            {
                let g = self.duck_gain + (end - self.duck_gain) * (i as f32 + 1.0) / frames;
                frame[0] *= g;
                frame[1] *= g;
            }
            self.duck_gain = end;
        }

        if let Ok(mut tap) = self.player.tap.try_lock()
            && let Some(tx) = tap.as_mut()
        {
            push_frames(tx, &self.buf);
        }
        sink(&self.buf);
        Ok(())
    }

    fn describe(&self) -> String {
        "Music".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(n: usize, repeat: Repeat) -> State {
        State {
            tracks: (0..n)
                .map(|i| Track {
                    path: format!("/t{i}.mp3").into(),
                    name: format!("t{i}"),
                })
                .collect(),
            order: (0..n).collect(),
            at: Some(0),
            playing: true,
            shuffle: false,
            repeat,
            generation: 0,
            start_ms: 0,
            title: None,
            artist: None,
            duration_ms: None,
            error: None,
            shutdown: false,
        }
    }

    #[test]
    fn advancing_follows_repeat() {
        let mut s = state(2, Repeat::Off);
        assert!(s.advance(false));
        assert_eq!(s.at, Some(1));
        assert!(!s.advance(false), "stops after the last track");

        let mut s = state(2, Repeat::All);
        s.advance(false);
        assert!(s.advance(false));
        assert_eq!(s.at, Some(0), "wraps around");

        let mut s = state(2, Repeat::One);
        assert!(s.advance(false));
        assert_eq!(s.at, Some(0), "the same track again");
        assert!(s.advance(true));
        assert_eq!(s.at, Some(1), "but Next still moves on");
    }

    #[test]
    fn shuffle_keeps_every_track_once_with_the_current_first() {
        let order = shuffled(50, Some(17));
        assert_eq!(order[0], 17);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn scanning_finds_audio_in_folders_in_order() {
        let dir = std::env::temp_dir().join(format!("vam-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("b/deeper")).unwrap();
        for f in [
            "b/deeper/3.flac",
            "b/2.MP3",
            "a.wav",
            "notes.txt",
            "cover.jpg",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let dir_s = dir.to_string_lossy().into_owned();
        let single = dir.join("a.wav").to_string_lossy().into_owned();
        let tracks = scan(&[dir_s, single, "/no/such/place".into()]);
        let names: Vec<_> = tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "2", "3"], "audio only, sorted, no duplicates");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
