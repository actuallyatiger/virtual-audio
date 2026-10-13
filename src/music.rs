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
use crate::config::Repeat;
use crate::decode::{AUDIO_EXTENSIONS, TrackDecoder};
use crate::dsp::ProcessingControl;
use crate::mixer::ChannelControl;
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
/// Folder entries a scan looks at before giving up (say, a whole drive).
const MAX_ENTRIES: usize = 200_000;
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
    let mut visited = 0;
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            let mut files = Vec::new();
            walk(path, 0, &mut files, &mut visited);
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

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, visited: &mut usize) {
    if depth > MAX_DEPTH || out.len() >= MAX_TRACKS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        *visited += 1;
        if *visited > MAX_ENTRIES {
            return;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, depth + 1, out, visited),
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

    fn current_path(&self) -> Option<PathBuf> {
        self.current()
            .and_then(|t| self.tracks.get(t))
            .map(|t| t.path.clone())
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

    /// Starts a new feed `start_ms` into the current track.
    fn jump(&mut self, start_ms: u64) {
        self.generation += 1;
        self.start_ms = start_ms;
    }

    /// Forgets what the previous track was called.
    fn new_track(&mut self) {
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
    /// The microphones to listen to, with their channels: a muted mic (or
    /// a push-to-talk one with the key up) isn't heard, so doesn't duck.
    pub mics: Mutex<Vec<(Arc<ProcessingControl>, Arc<ChannelControl>)>>,
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
        mics.iter().any(|(m, channel)| {
            if channel.muted() || !channel.gate_open() {
                false
            } else if m.gate_enabled() {
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
            s.new_track();
            self.jump(&mut s, 0);
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

    /// Starts a new feed `start_ms` into the current track; until it plays,
    /// that's the position.
    fn jump(&self, s: &mut State, start_ms: u64) {
        s.jump(start_ms);
        self.decoded.store(0, Ordering::Relaxed);
        self.buffered.store(0, Ordering::Relaxed);
        self.wake.notify_all();
    }

    /// Picks up where playback is with a new feed, for a new music stream
    /// (the old one took the feed with it).
    pub fn restart(&self) {
        let position = self.position_ms();
        let mut s = self.state.lock().unwrap();
        if s.at.is_some() {
            self.jump(&mut s, position);
        }
    }

    /// A track's path and name, by its index in the list.
    pub fn track(&self, track: usize) -> Option<(PathBuf, String)> {
        let s = self.state.lock().unwrap();
        s.tracks
            .get(track)
            .map(|t| (t.path.clone(), t.name.clone()))
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
        s.new_track();
        self.jump(&mut s, 0);
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
            s.new_track();
            self.jump(&mut s, 0);
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
            s.new_track();
            self.jump(&mut s, 0);
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
            if s.at != Some(at) {
                s.new_track();
            }
        }
        self.jump(&mut s, 0);
        self.set_playing(&mut s, true);
    }

    pub fn seek(&self, ms: u64) {
        let mut s = self.state.lock().unwrap();
        if s.at.is_some() {
            // Keeps the title and length: it's the same track.
            self.jump(&mut s, ms);
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
        let (generation, path, start_ms) = {
            let Some(p) = player.upgrade() else { return };
            let mut s = p.state.lock().unwrap();
            loop {
                if s.shutdown {
                    return;
                }
                if s.playing
                    && let Some(path) = s.current_path()
                {
                    break (s.generation, path, s.start_ms);
                }
                s = p
                    .wake
                    .wait_timeout(s, Duration::from_millis(500))
                    .unwrap()
                    .0;
            }
        };
        let Some(p) = player.upgrade() else { return };
        let (mut tx, rx) = rtrb::RingBuffer::new(FEED_SAMPLES);
        *p.feed.lock().unwrap() = Some(rx);
        p.decoded.store(0, Ordering::Relaxed);
        p.buffered.store(0, Ordering::Relaxed);
        stream_tracks(&p, generation, path, start_ms, &mut tx);
    }
}

/// A track counts as played once it has given this much audio (0.1 s).
const PLAYED_FRAMES: u64 = SAMPLE_RATE as u64 / 10;

/// Decodes tracks back to back into `tx` until a jump, a stop, or the end
/// of the list.
fn stream_tracks(
    p: &MusicPlayer,
    generation: u64,
    mut path: PathBuf,
    mut start_ms: u64,
    tx: &mut rtrb::Producer<f32>,
) {
    // Tracks in a row that played nothing (unreadable, empty, or tiny):
    // skipped, and enough of them stop playback rather than spin through
    // a list where nothing plays.
    let mut failures = 0;
    let mut buf = Vec::new();
    loop {
        let mut frames = 0;
        let opened = TrackDecoder::open_path(&path).and_then(|mut d| {
            if start_ms > 0 {
                d.seek(start_ms)?;
            }
            Ok(d)
        });
        match opened {
            Err(e) => {
                tracing::warn!("music: {}: {e:#}", path.display());
                let mut s = p.state.lock().unwrap();
                if s.generation != generation {
                    return;
                }
                s.error = Some(format!("{}: {e:#}", path.display()));
            }
            Ok(mut decoder) => {
                {
                    let mut s = p.state.lock().unwrap();
                    if s.generation != generation {
                        return;
                    }
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
                        // Paused, or the feed is full: wait without losing
                        // our place.
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
                            p.state.lock().unwrap().error =
                                Some(format!("{}: {e:#}", path.display()));
                            false
                        }
                    };
                    // A packet can be bigger than the free space: hand it
                    // over in pieces as the player drains the feed.
                    let mut rest = &buf[..];
                    while !rest.is_empty() {
                        let n = rest.len().min(tx.slots()) / CHANNELS * CHANNELS;
                        push_frames(tx, &rest[..n]);
                        p.decoded
                            .fetch_add((n / CHANNELS) as u64, Ordering::Relaxed);
                        frames += (n / CHANNELS) as u64;
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
            }
        }
        let played = frames >= PLAYED_FRAMES;
        failures = if played { 0 } else { failures + 1 };
        (p.notify)();

        // The track ended: carry on into the next one in the same feed. One
        // that didn't play is skipped even on repeat-one.
        let mut s = p.state.lock().unwrap();
        if s.generation != generation {
            return;
        }
        let give_up = failures >= s.order.len().clamp(1, 10);
        if give_up || !s.advance(!played) {
            drop(s);
            finish(p, generation, tx);
            return;
        }
        // Positions count from the new track's start.
        s.start_ms = 0;
        s.new_track();
        p.decoded.store(0, Ordering::Relaxed);
        p.buffered.store(0, Ordering::Relaxed);
        let Some(next) = s.current_path() else { return };
        path = next;
        start_ms = 0;
    }
}

/// The end of the list (or nothing in it would play): lets what's in the
/// feed play out (all but a last partial block, which the stream only
/// plays if it's still going), then stops.
fn finish(p: &MusicPlayer, generation: u64, tx: &rtrb::Producer<f32>) {
    let mut s = p.state.lock().unwrap();
    while tx.slots() <= FEED_SAMPLES - BLOCK_FRAMES * CHANNELS {
        if s.generation != generation || s.shutdown {
            return;
        }
        s = p.wake.wait_timeout(s, Duration::from_millis(20)).unwrap().0;
    }
    if s.generation != generation {
        return;
    }
    p.set_playing(&mut s, false);
    s.new_track();
    p.jump(&mut s, 0);
    drop(s);
    (p.notify)();
}

/// The music source's audio, played from the decoder's feed in real time.
pub struct Stream {
    player: Arc<MusicPlayer>,
    rx: Option<rtrb::Consumer<f32>>,
    next: Instant,
    buf: Vec<f32>,
    /// Gain at the end of the last block: 0 when silent. Every start, stop
    /// and switch to a new feed ramps through it rather than clicking.
    level: f32,
    duck_gain: f32,
    /// Blocks left to stay ducked after the talking stops.
    duck_hold: u32,
}

impl Stream {
    pub fn new(player: Arc<MusicPlayer>) -> Self {
        // A previous stream took the feed with it.
        player.restart();
        Self {
            player,
            rx: None,
            next: Instant::now(),
            buf: vec![0.0; BLOCK_FRAMES * CHANNELS],
            level: 0.0,
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

        // A jump offers a new feed: fade the old one out first (its audio
        // is then dropped), and start the new one from silence.
        let mut switching = false;
        if let Ok(mut feed) = self.player.feed.try_lock()
            && feed.is_some()
        {
            if self.level == 0.0 {
                self.rx = feed.take();
            } else {
                switching = true;
            }
        }
        let playing = self.player.playing.load(Ordering::Relaxed);
        let mut target = if playing && !switching { 1.0 } else { 0.0 };
        self.buf.fill(0.0);
        let mut frames = 0;
        if (target > 0.0 || self.level > 0.0)
            && let Some(rx) = &mut self.rx
        {
            let mut n = rx.slots().min(self.buf.len()) / CHANNELS * CHANNELS;
            if self.level == 0.0 && n < self.buf.len() {
                n = 0; // starting: wait for a whole block to fade in over
            }
            if let Ok(chunk) = rx.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                self.buf[..a.len()].copy_from_slice(a);
                self.buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
            frames = n / CHANNELS;
            if frames < BLOCK_FRAMES {
                target = 0.0; // running dry: fade out what there is
            }
            self.player
                .buffered
                .store((rx.slots() / CHANNELS) as u64, Ordering::Relaxed);
        }
        if frames > 0 && (self.level != 1.0 || target != 1.0) {
            let start = self.level;
            for (i, frame) in self.buf.as_chunks_mut::<CHANNELS>().0[..frames]
                .iter_mut()
                .enumerate()
            {
                let g = start + (target - start) * (i as f32 + 1.0) / frames as f32;
                frame[0] *= g;
                frame[1] *= g;
            }
        }
        self.level = if frames > 0 { target } else { 0.0 };

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

    /// A player with `tracks` WAVs of the given lengths (frames), and a
    /// count of its notifications.
    fn player(
        name: &str,
        lengths: &[usize],
        repeat: Repeat,
    ) -> (Arc<MusicPlayer>, Arc<AtomicU64>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("vam-music-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, &frames) in lengths.iter().enumerate() {
            std::fs::write(
                dir.join(format!("{i}.wav")),
                crate::decode::test_wav(frames),
            )
            .unwrap();
        }
        let notified = Arc::new(AtomicU64::new(0));
        let n = notified.clone();
        let player = MusicPlayer::new(false, repeat, move || {
            n.fetch_add(1, Ordering::Relaxed);
        });
        player.set_tracks(scan(&[dir.to_string_lossy().into_owned()]));
        (player, notified, dir)
    }

    /// Reads `blocks` 10 ms blocks; returns the frames that weren't silent.
    fn heard(stream: &mut Stream, blocks: usize) -> usize {
        let mut frames = 0;
        for _ in 0..blocks {
            stream
                .read(&mut |b| {
                    frames += b
                        .as_chunks::<CHANNELS>()
                        .0
                        .iter()
                        .filter(|f| f[0] != 0.0)
                        .count()
                })
                .unwrap();
        }
        frames
    }

    #[test]
    fn the_last_track_plays_to_the_end() {
        let (player, _, dir) = player("tail", &[14_400], Repeat::Off); // 300 ms
        let mut stream = Stream::new(player.clone());
        player.play_track(0);
        let frames = heard(&mut stream, 80);
        assert!(frames >= 14_400 - BLOCK_FRAMES, "heard {frames} of 14400");
        assert!(!player.view().playing, "then stops");
        player.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_list_where_nothing_plays_stops_instead_of_spinning() {
        let (player, notified, dir) = player("empty", &[0, 0], Repeat::All);
        player.play_track(0);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!player.view().playing);
        let n = notified.load(Ordering::Relaxed);
        assert!(n < 10, "{n} notifications");
        player.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_stream_picks_up_where_the_old_one_was() {
        let (player, _, dir) = player("restart", &[96_000], Repeat::Off); // 2 s
        let mut stream = Stream::new(player.clone());
        player.play_track(0);
        assert!(heard(&mut stream, 30) > 0);
        // The music source is removed (pausing it), then added back.
        player.pause();
        drop(stream);
        let position = player.position_ms();
        let mut stream = Stream::new(player.clone());
        assert_eq!(player.position_ms(), position);
        player.toggle();
        assert!(heard(&mut stream, 30) > 0, "audible again");
        assert!(player.position_ms() > position);
        player.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seeking_while_paused_moves_the_position_and_keeps_the_track_info() {
        let (player, _, dir) = player("seek", &[96_000], Repeat::Off);
        let mut stream = Stream::new(player.clone());
        player.play_track(0);
        heard(&mut stream, 30);
        player.pause();
        player.seek(1_000);
        assert_eq!(player.position_ms(), 1_000);
        assert_eq!(player.view().duration_ms, Some(2_000));
        player.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pausing_fades_out_instead_of_clicking() {
        let (player, _, dir) = player("fade", &[96_000], Repeat::Off);
        let mut stream = Stream::new(player.clone());
        player.play_track(0);
        heard(&mut stream, 20);
        player.pause();
        let mut block = Vec::new();
        stream.read(&mut |b| block = b.to_vec()).unwrap();
        let first = block[0];
        let last = block[block.len() - 2];
        assert!(first > 0.02, "starts at full level ({first})");
        assert!(last.abs() < first / 50.0, "ends near silence ({last})");
        player.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }
}
