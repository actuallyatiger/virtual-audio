//! Built-in soundboard: audio files decoded into memory and played into the
//! mix as one source.
//!
//! The soundboard source's capture thread runs a [`Stream`], which mixes the
//! sounds that are playing into 10 ms blocks at real-time pace. From there
//! it is an ordinary source: volume, mute, meter, hotkeys and drift
//! correction all work as they do for a microphone.

use crate::backend::CaptureStream;
use crate::decode::{AUDIO_EXTENSIONS, TrackDecoder};
use crate::mixer::{CHANNELS, MonitorTap, SAMPLE_RATE, push_frames};
use anyhow::bail;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_SOUNDS: usize = 64;
/// Largest file accepted for upload.
pub const MAX_FILE_BYTES: usize = 20 << 20;
const MAX_SECONDS: u64 = 60;
/// Stopping or restarting a sound fades it out over 5 ms instead of clicking.
const FADE_FRAMES: usize = 240;
/// Fade at a trimmed edge with no fade of its own (2 ms).
const EDGE_FRAMES: usize = 96;
/// The soundboard is mixed in 10 ms blocks.
const BLOCK_FRAMES: usize = SAMPLE_RATE as usize / 100;

pub fn sounds_dir() -> PathBuf {
    crate::config::data_dir().join("sounds")
}

/// Where a sound's file lives. `file` comes from config.json, which can be
/// edited by hand, so anything but a plain file name (a path that would
/// reach outside the sounds folder) is refused.
pub fn sound_path(file: &str) -> anyhow::Result<PathBuf> {
    let name = std::path::Path::new(file).file_name();
    if file.is_empty() || name != Some(file.as_ref()) {
        bail!("invalid sound file name {file:?}");
    }
    Ok(sounds_dir().join(file))
}

/// The extension to store an upload under: its own if it's a known audio
/// type (it helps the decoder guess the format), otherwise "bin".
pub fn extension(filename: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        ext
    } else {
        "bin".into()
    }
}

/// Decodes a whole audio file (MP3, WAV, OGG Vorbis, FLAC, M4A/AAC, …)
/// to interleaved stereo f32 at [`SAMPLE_RATE`].
pub fn decode(bytes: Vec<u8>, extension: Option<&str>) -> anyhow::Result<Vec<f32>> {
    let mut track = TrackDecoder::open(Box::new(std::io::Cursor::new(bytes)), extension)?;
    let mut stereo = Vec::new();
    let max = MAX_SECONDS as usize * SAMPLE_RATE as usize * CHANNELS;
    while track.next(&mut stereo)? {
        if stereo.len() > max {
            bail!("sounds can be at most {MAX_SECONDS} seconds long");
        }
    }
    if !track.has_audio() || stereo.is_empty() {
        bail!("the file has no audio");
    }
    if stereo.len() > max {
        bail!("sounds can be at most {MAX_SECONDS} seconds long");
    }
    Ok(stereo)
}

/// Which part of a sound plays, and its fades, in frames. Non-destructive:
/// the decoded file is kept whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Trim {
    pub start: usize,
    /// `None` plays to the end.
    pub end: Option<usize>,
    pub fade_in: usize,
    pub fade_out: usize,
}

const FRAMES_PER_MS: usize = SAMPLE_RATE as usize / 1000;

impl Trim {
    pub fn from_ms(start: u32, end: Option<u32>, fade_in: u32, fade_out: u32) -> Self {
        let f = |ms: u32| ms as usize * FRAMES_PER_MS;
        Self {
            start: f(start),
            end: end.map(f),
            fade_in: f(fade_in),
            fade_out: f(fade_out),
        }
    }

    /// The frames to play from a clip `len` frames long.
    fn range(&self, len: usize) -> (usize, usize) {
        let start = self.start.min(len);
        let end = self.end.unwrap_or(len).clamp(start, len);
        (start, end)
    }
}

struct Clip {
    samples: Vec<f32>,
    gain: AtomicU32,
    trim: Mutex<Trim>,
}

struct Voice {
    id: String,
    clip: Arc<Clip>,
    /// Gain applied at the end of the last block; volume changes ramp from
    /// it across the next block rather than jumping.
    gain: f32,
    /// Next frame to play, and where to stop.
    pos: usize,
    start: usize,
    end: usize,
    fade_in: usize,
    fade_out: usize,
    /// Frames left in a fade-out, once stopped.
    fade: Option<usize>,
    /// A preview: heard through Hear sounds only, never sent into the mix.
    private: bool,
}

impl Voice {
    /// The trim's fade in/out at the current position.
    fn envelope(&self) -> f32 {
        let mut env = 1.0f32;
        let since = self.pos - self.start;
        if since < self.fade_in {
            env = since as f32 / self.fade_in as f32;
        }
        let left = self.end - self.pos;
        if left < self.fade_out {
            env = env.min(left as f32 / self.fade_out as f32);
        }
        env
    }
}

/// A sound's shape for the trim editor.
#[derive(Debug, serde::Serialize)]
pub struct Waveform {
    pub duration_ms: u32,
    /// Peak level (0-1) of each equal slice of the sound.
    pub peaks: Vec<f32>,
    /// Where sound starts and stops, ignoring near-silence at either end.
    pub silence_start_ms: u32,
    pub silence_end_ms: u32,
}

#[derive(Default)]
struct PlayerState {
    clips: HashMap<String, Arc<Clip>>,
    voices: Vec<Voice>,
}

/// The loaded sounds and what is playing. Shared by the engine (which loads
/// and triggers sounds) and the soundboard source's [`Stream`].
#[derive(Default)]
pub struct Player {
    state: Mutex<PlayerState>,
    /// Hear sounds: a copy of the soundboard's output for your headphones.
    pub tap: MonitorTap,
}

impl Player {
    pub fn insert(&self, id: &str, samples: Vec<f32>, gain: f32, trim: Trim) {
        let clip = Arc::new(Clip {
            samples,
            gain: AtomicU32::new(gain.to_bits()),
            trim: Mutex::new(trim),
        });
        self.state.lock().unwrap().clips.insert(id.to_owned(), clip);
    }

    /// Unloads a sound, fading it out if it is playing.
    pub fn remove(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.clips.remove(id);
        for v in state.voices.iter_mut().filter(|v| v.id == id) {
            v.fade.get_or_insert(FADE_FRAMES);
        }
    }

    /// Drops everything playing at once; for when nothing is mixing the
    /// soundboard any more, so fades would never finish.
    pub fn clear(&self) {
        self.state.lock().unwrap().voices.clear();
    }

    pub fn contains(&self, id: &str) -> bool {
        self.state.lock().unwrap().clips.contains_key(id)
    }

    /// A loaded sound's length in milliseconds.
    pub fn duration_ms(&self, id: &str) -> Option<u32> {
        let state = self.state.lock().unwrap();
        let clip = state.clips.get(id)?;
        Some((clip.samples.len() / CHANNELS / FRAMES_PER_MS) as u32)
    }

    /// Changes a sound's volume, including while it plays.
    pub fn set_gain(&self, id: &str, gain: f32) {
        if let Some(clip) = self.state.lock().unwrap().clips.get(id) {
            clip.gain.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    /// Changes a sound's trim for its next plays.
    pub fn set_trim(&self, id: &str, trim: Trim) {
        if let Some(clip) = self.state.lock().unwrap().clips.get(id) {
            *clip.trim.lock().unwrap() = trim;
        }
    }

    /// Plays a sound (its trimmed part) from the start. If it is already
    /// playing, that copy fades out so repeated presses restart it rather
    /// than pile up.
    pub fn play(&self, id: &str) -> bool {
        self.start(id, false)
    }

    /// Plays a sound to you alone, through Hear sounds, not into the mix.
    pub fn preview(&self, id: &str) -> bool {
        self.start(id, true)
    }

    fn start(&self, id: &str, private: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(clip) = state.clips.get(id).cloned() else {
            return false;
        };
        for v in state
            .voices
            .iter_mut()
            .filter(|v| v.id == id && v.private == private)
        {
            v.fade.get_or_insert(FADE_FRAMES);
        }
        let gain = f32::from_bits(clip.gain.load(Ordering::Relaxed));
        let trim = *clip.trim.lock().unwrap();
        let len = clip.samples.len() / CHANNELS;
        let (start, end) = trim.range(len);
        // A cut mid-waveform would click, so it always gets a short fade.
        let edge = |cut: bool, fade: usize| if cut { fade.max(EDGE_FRAMES) } else { fade };
        state.voices.push(Voice {
            id: id.to_owned(),
            clip,
            gain,
            pos: start,
            start,
            end,
            fade_in: edge(start > 0, trim.fade_in),
            fade_out: edge(end < len, trim.fade_out),
            fade: None,
            private,
        });
        true
    }

    pub fn stop_all(&self) {
        for v in &mut self.state.lock().unwrap().voices {
            v.fade.get_or_insert(FADE_FRAMES);
        }
    }

    /// Ids of the sounds playing into the mix now.
    pub fn playing(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let mut ids: Vec<String> = state
            .voices
            .iter()
            .filter(|v| v.fade.is_none() && !v.private)
            .map(|v| v.id.clone())
            .collect();
        ids.dedup();
        ids
    }

    /// `buckets` peaks across the whole sound, for drawing it.
    pub fn waveform(&self, id: &str, buckets: usize) -> Option<Waveform> {
        let clip = self.state.lock().unwrap().clips.get(id).cloned()?;
        let frames = clip.samples.as_chunks::<CHANNELS>().0;
        let peak = |f: &[f32; CHANNELS]| f[0].abs().max(f[1].abs());
        let per = frames.len().div_ceil(buckets.max(1)).max(1);
        let peaks = frames
            .chunks(per)
            .map(|c| c.iter().map(peak).fold(0f32, f32::max).min(1.0))
            .collect();
        // Near-silence (below -50 dBFS) at the ends, with 10 ms kept.
        const QUIET: f32 = 0.003;
        let ms = |frame: usize| (frame / FRAMES_PER_MS) as u32;
        let first = frames.iter().position(|f| peak(f) > QUIET).unwrap_or(0);
        let last = frames
            .iter()
            .rposition(|f| peak(f) > QUIET)
            .map_or(frames.len(), |i| i + 1);
        let pad = 10 * FRAMES_PER_MS;
        Some(Waveform {
            duration_ms: ms(frames.len()),
            peaks,
            silence_start_ms: ms(first.saturating_sub(pad)),
            silence_end_ms: ms((last + pad).min(frames.len())),
        })
    }

    /// Mixes the next `out.len() / 2` frames of every playing sound into
    /// `out` (what goes into the mix) and `monitor` (what you hear, which
    /// adds previews).
    fn mix(&self, out: &mut [f32], monitor: &mut [f32]) {
        out.fill(0.0);
        monitor.fill(0.0);
        let mut state = self.state.lock().unwrap();
        let frames = (out.len() / CHANNELS) as f32;
        for v in &mut state.voices {
            let target = f32::from_bits(v.clip.gain.load(Ordering::Relaxed));
            let step = (target - v.gain) / frames;
            let rest = v
                .clip
                .samples
                .get(v.pos * CHANNELS..v.end * CHANNELS)
                .unwrap_or(&[]);
            for ((o, m), frame) in out
                .as_chunks_mut::<CHANNELS>()
                .0
                .iter_mut()
                .zip(monitor.as_chunks_mut::<CHANNELS>().0.iter_mut())
                .zip(rest.as_chunks::<CHANNELS>().0)
            {
                v.gain += step;
                let g = match &mut v.fade {
                    Some(0) => break,
                    Some(left) => {
                        *left -= 1;
                        v.gain * *left as f32 / FADE_FRAMES as f32
                    }
                    None => v.gain,
                } * v.envelope();
                let (l, r) = (frame[0] * g, frame[1] * g);
                if !v.private {
                    o[0] += l;
                    o[1] += r;
                }
                m[0] += l;
                m[1] += r;
                v.pos += 1;
            }
            // Land exactly on the target despite rounding in the steps.
            v.gain = target;
        }
        state.voices.retain(|v| v.fade != Some(0) && v.pos < v.end);
    }
}

/// The soundboard source's audio: the playing sounds, mixed in real time.
pub struct Stream {
    player: Arc<Player>,
    next: Instant,
    buf: Vec<f32>,
    monitor: Vec<f32>,
}

impl Stream {
    pub fn new(player: Arc<Player>) -> Self {
        Self {
            player,
            next: Instant::now(),
            buf: vec![0.0; BLOCK_FRAMES * CHANNELS],
            monitor: vec![0.0; BLOCK_FRAMES * CHANNELS],
        }
    }
}

impl CaptureStream for Stream {
    fn read(&mut self, sink: &mut dyn FnMut(&[f32])) -> anyhow::Result<()> {
        let period = Duration::from_secs_f64(BLOCK_FRAMES as f64 / SAMPLE_RATE as f64);
        let now = Instant::now();
        if now > self.next + 20 * period {
            // Fell far behind (the PC was asleep or busy): don't burst.
            self.next = now;
        }
        if self.next > now {
            std::thread::sleep(self.next - now);
        }
        self.next += period;
        self.player.mix(&mut self.buf, &mut self.monitor);
        if let Ok(mut tap) = self.player.tap.try_lock()
            && let Some(tx) = tap.as_mut()
        {
            push_frames(tx, &self.monitor);
        }
        sink(&self.buf);
        Ok(())
    }

    fn describe(&self) -> String {
        "Soundboard".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mixes into `buf`, discarding the monitor copy.
    fn mix(player: &Player, buf: &mut [f32]) {
        let mut monitor = vec![0.0; buf.len()];
        player.mix(buf, &mut monitor);
    }

    /// A 16-bit PCM WAV file.
    fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut f = Vec::new();
        f.extend(b"RIFF");
        f.extend((36 + data_len).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(channels.to_le_bytes());
        f.extend(rate.to_le_bytes());
        f.extend((rate * channels as u32 * 2).to_le_bytes());
        f.extend((channels * 2).to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"data");
        f.extend(data_len.to_le_bytes());
        for s in samples {
            f.extend(s.to_le_bytes());
        }
        f
    }

    #[test]
    fn decodes_mono_wav_to_stereo_48k() {
        let mono: Vec<i16> = (0..44_100)
            .map(|i| {
                ((i as f32 * 440.0 / 44_100.0 * std::f32::consts::TAU).sin() * 16_000.0) as i16
            })
            .collect();
        let out = decode(wav(44_100, 1, &mono), Some("wav")).unwrap();
        assert_eq!(out.len() / 2, 48_000, "one second at 48 kHz");
        let mid = &out[40_000..41_000];
        assert!(
            mid.chunks(2).all(|f| f[0] == f[1]),
            "mono copied to both sides"
        );
        let peak = mid.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak - 16_000.0 / 32_768.0).abs() < 0.02,
            "level kept: {peak}"
        );
    }

    #[test]
    fn rejects_garbage_and_long_files() {
        let err = decode(b"definitely not audio".to_vec(), Some("mp3")).unwrap_err();
        assert!(err.to_string().contains("supported"), "{err}");
        let long = vec![0i16; 8_000 * 61];
        let err = decode(wav(8_000, 1, &long), None).unwrap_err();
        assert!(err.to_string().contains("60 seconds"), "{err}");
    }

    #[test]
    fn plays_restarts_and_stops() {
        let player = Player::default();
        player.insert("a", vec![0.5; 1_000 * 2], 1.0, Trim::default());
        assert!(!player.play("missing"));
        assert!(player.play("a"));
        let mut buf = vec![0.0; 960];
        mix(&player, &mut buf);
        assert!(buf.iter().all(|&s| s == 0.5));
        assert_eq!(player.playing(), vec!["a"]);

        // Restart: the old copy fades while the new one plays from the start.
        player.play("a");
        mix(&player, &mut buf);
        assert!(buf[0] > 0.5 && buf[0] < 1.0, "overlap during the fade");
        assert_eq!(buf[(FADE_FRAMES + 1) * 2], 0.5, "old copy gone");
        assert_eq!(player.playing(), vec!["a"]);

        // A volume change ramps across one block instead of jumping.
        player.set_gain("a", 0.0);
        mix(&player, &mut buf);
        assert!(buf[0] > 0.49 && buf[0] < 0.5, "starts near the old level");
        assert!(buf[478 * 2] > 0.0 && buf[478 * 2] < 0.01, "ends near zero");
        assert!(buf.windows(4).all(|w| w[2] <= w[0]), "smooth fall");
        mix(&player, &mut buf);
        assert!(buf.iter().all(|&s| s == 0.0), "then silent");

        player.stop_all();
        assert!(player.playing().is_empty());
        mix(&player, &mut buf);
        mix(&player, &mut buf);
        assert!(player.state.lock().unwrap().voices.is_empty());
    }

    #[test]
    fn removing_fades_and_clearing_drops() {
        let player = Player::default();
        player.insert("a", vec![0.5; 4_000 * 2], 1.0, Trim::default());
        player.play("a");
        player.remove("a");
        assert!(!player.contains("a"));
        let mut buf = vec![0.0; 960];
        mix(&player, &mut buf);
        assert!(buf[0] > 0.0, "fades rather than cuts");
        assert_eq!(buf[FADE_FRAMES * 2], 0.0);
        assert!(player.state.lock().unwrap().voices.is_empty());

        player.insert("b", vec![0.5; 4_000 * 2], 1.0, Trim::default());
        player.play("b");
        player.clear();
        assert!(player.playing().is_empty());
        mix(&player, &mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn sounds_end_on_their_own() {
        let player = Player::default();
        player.insert("a", vec![0.25; 100 * 2], 1.0, Trim::default());
        player.play("a");
        let mut buf = vec![0.0; 960];
        mix(&player, &mut buf);
        assert_eq!(buf[99 * 2], 0.25);
        assert_eq!(buf[100 * 2], 0.0);
        assert!(player.playing().is_empty());
    }

    #[test]
    fn sound_paths_stay_in_the_sounds_folder() {
        assert!(sound_path("snd1.mp3").unwrap().starts_with(sounds_dir()));
        for bad in [
            "",
            "../config.json",
            "a/b.mp3",
            "/etc/passwd",
            "..",
            "C:\\x.mp3",
        ] {
            if cfg!(windows) || !bad.contains('\\') {
                assert!(sound_path(bad).is_err(), "{bad:?}");
            }
        }
    }

    /// A clip whose frame `i` holds the value `i / 10_000` on both sides.
    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).flat_map(|i| [i as f32 / 10_000.0; 2]).collect()
    }

    #[test]
    fn trim_plays_only_its_range() {
        let player = Player::default();
        let trim = Trim {
            start: 100,
            end: Some(300),
            ..Trim::default()
        };
        player.insert("a", ramp(1_000), 1.0, trim);
        player.play("a");
        let mut buf = vec![0.0; 960];
        mix(&player, &mut buf);
        assert_eq!(buf[0], 0.0, "starts at frame 100, fading in from the cut");
        assert_eq!(
            buf[EDGE_FRAMES * 2],
            0.0196,
            "full level after the edge fade"
        );
        assert!(buf[199 * 2] > 0.0, "frame 299 is the last");
        assert_eq!(buf[200 * 2], 0.0, "then stops");
        assert!(player.playing().is_empty());
    }

    #[test]
    fn trim_fades_in_and_out() {
        let player = Player::default();
        let trim = Trim {
            fade_in: 100,
            fade_out: 100,
            end: Some(400),
            ..Trim::default()
        };
        player.insert("a", vec![0.5; 1_000 * 2], 1.0, trim);
        player.play("a");
        let mut buf = vec![0.0; 960];
        mix(&player, &mut buf);
        assert_eq!(buf[0], 0.0, "fades in from silence");
        assert!((buf[50 * 2] - 0.25).abs() < 1e-6, "halfway in");
        assert_eq!(buf[200 * 2], 0.5, "full level in the middle");
        assert!((buf[350 * 2] - 0.25).abs() < 1e-6, "halfway out");
        assert_eq!(buf[400 * 2], 0.0);
    }

    #[test]
    fn previews_are_heard_but_not_mixed() {
        let player = Player::default();
        player.insert("a", vec![0.5; 1_000 * 2], 1.0, Trim::default());
        assert!(player.preview("a"));
        assert!(
            player.playing().is_empty(),
            "not shown as playing in the mix"
        );
        let (mut out, mut monitor) = (vec![0.0; 960], vec![0.0; 960]);
        player.mix(&mut out, &mut monitor);
        assert!(out.iter().all(|&s| s == 0.0), "nothing goes into the mic");
        assert_eq!(monitor[0], 0.5, "but you hear it");
    }

    #[test]
    fn waveform_finds_the_silence_at_the_ends() {
        let player = Player::default();
        let mut clip = vec![0.0; 4_800 * 2]; // 100 ms silence
        clip.extend(vec![0.5; 4_800 * 2]); // 100 ms sound
        clip.extend(vec![0.0; 9_600 * 2]); // 200 ms silence
        player.insert("a", clip, 1.0, Trim::default());
        let w = player.waveform("a", 40).unwrap();
        assert_eq!(w.duration_ms, 400);
        assert_eq!(w.peaks.len(), 40);
        assert_eq!(w.peaks[0], 0.0);
        assert_eq!(w.peaks[15], 0.5);
        assert_eq!(
            (w.silence_start_ms, w.silence_end_ms),
            (90, 210),
            "10 ms kept"
        );
        assert!(player.waveform("missing", 40).is_none());
    }

    #[test]
    fn extensions_are_sanitised() {
        assert_eq!(extension("Air Horn.MP3"), "mp3");
        assert_eq!(extension("x.flac"), "flac");
        assert_eq!(extension("noext"), "bin");
        assert_eq!(extension("../../evil.exe"), "bin");
        assert_eq!(extension("a.m p3"), "bin");
    }
}
