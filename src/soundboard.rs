//! Built-in soundboard: audio files decoded into memory and played into the
//! mix as one source.
//!
//! The soundboard source's capture thread runs a [`Stream`], which mixes the
//! sounds that are playing into 10 ms blocks at real-time pace. From there
//! it is an ordinary source: volume, mute, meter, hotkeys and drift
//! correction all work as they do for a microphone.

use crate::backend::CaptureStream;
use crate::mixer::{CHANNELS, MonitorTap, SAMPLE_RATE, push_frames};
use anyhow::{Context, anyhow, bail};
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
    const KNOWN: &[&str] = &[
        "mp3", "wav", "ogg", "oga", "flac", "m4a", "mp4", "aac", "mka", "mkv", "webm",
    ];
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if KNOWN.contains(&ext.as_str()) {
        ext
    } else {
        "bin".into()
    }
}

/// Decodes an audio file (MP3, WAV, OGG Vorbis, FLAC, M4A/AAC, …) to
/// interleaved stereo f32 at [`SAMPLE_RATE`].
pub fn decode(bytes: Vec<u8>, extension: Option<&str>) -> anyhow::Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| anyhow!("not a supported audio file (use MP3, WAV, OGG, FLAC or M4A)"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("the file has no audio"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| {
            anyhow!("this file's audio format isn't supported (use MP3, WAV, OGG, FLAC or M4A)")
        })?;

    let mut stereo = Vec::new();
    let mut rate = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(e).context("reading the file"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A corrupt packet: skip it, as players do.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("decoding the file"),
        };
        let spec = *decoded.spec();
        // Also bounds the resampler's work and memory for a corrupt header.
        if !(8_000..=384_000).contains(&spec.rate) {
            bail!("unsupported sample rate ({} Hz)", spec.rate);
        }
        if *rate.get_or_insert(spec.rate) != spec.rate {
            bail!("the sample rate changes partway through the file");
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        let channels = spec.channels.count();
        for frame in buf.samples().chunks_exact(channels) {
            let (l, r) = match frame {
                [m] => (*m, *m),
                [l, r, ..] => (*l, *r),
                [] => unreachable!(),
            };
            stereo.extend([l, r]);
        }
        if stereo.len() as u64 / CHANNELS as u64 > MAX_SECONDS * spec.rate as u64 {
            bail!("sounds can be at most {MAX_SECONDS} seconds long");
        }
    }
    let rate = rate.ok_or_else(|| anyhow!("the file has no audio"))?;
    if stereo.is_empty() {
        bail!("the file has no audio");
    }
    Ok(resample(&stereo, rate))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Converts interleaved stereo from `from` Hz to [`SAMPLE_RATE`] with a
/// windowed-sinc (Blackman) filter, using a precomputed kernel per phase.
fn resample(input: &[f32], from: u32) -> Vec<f32> {
    let to = SAMPLE_RATE;
    if from == to || from == 0 {
        return input.to_vec();
    }
    let g = gcd(from, to);
    let (up, down) = ((to / g) as usize, (from / g) as usize);
    // Cut off just below the lower of the two Nyquist frequencies, as a
    // fraction of the input's.
    let cutoff = (to as f64 / from as f64).min(1.0) * 0.95;
    let half = (16.0 / cutoff).ceil() as usize;
    let taps = 2 * half;
    let mut table = vec![0f32; up * taps];
    for (phase, row) in table.chunks_exact_mut(taps).enumerate() {
        let frac = phase as f64 / up as f64;
        let mut sum = 0.0;
        let mut kernel = vec![0f64; taps];
        for (j, k) in kernel.iter_mut().enumerate() {
            // Distance from the output position to input sample j.
            let t = frac + half as f64 - 1.0 - j as f64;
            let x = std::f64::consts::PI * cutoff * t;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { x.sin() / x };
            let u = t / half as f64;
            let window = if u.abs() >= 1.0 {
                0.0
            } else {
                0.42 + 0.5 * (std::f64::consts::PI * u).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * u).cos()
            };
            *k = cutoff * sinc * window;
            sum += *k;
        }
        // Normalise each phase to unity gain so there is no DC ripple.
        for (r, k) in row.iter_mut().zip(&kernel) {
            *r = (k / sum) as f32;
        }
    }

    let in_frames = input.len() / CHANNELS;
    let out_frames = (in_frames as u64 * up as u64 / down as u64) as usize;
    let mut out = Vec::with_capacity(out_frames * CHANNELS);
    for n in 0..out_frames {
        let pos = n as u64 * down as u64;
        let base = (pos / up as u64) as usize;
        let row = &table[(pos % up as u64) as usize * taps..][..taps];
        let first = base as isize + 1 - half as isize;
        let (mut l, mut r) = (0f32, 0f32);
        for (j, &k) in row.iter().enumerate() {
            let i = first + j as isize;
            if i < 0 || i as usize >= in_frames {
                continue;
            }
            let i = i as usize * CHANNELS;
            l += input[i] * k;
            r += input[i + 1] * k;
        }
        out.extend([l, r]);
    }
    out
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
        let (start, end) = trim.range(clip.samples.len() / CHANNELS);
        state.voices.push(Voice {
            id: id.to_owned(),
            clip,
            gain,
            pos: start,
            start,
            end,
            fade_in: trim.fade_in,
            fade_out: trim.fade_out,
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
    fn resampling_keeps_a_tone_clean() {
        // 1 kHz at 44.1 kHz → 48 kHz: still 1 kHz, no loss of level.
        let input: Vec<f32> = (0..44_100)
            .flat_map(|i| {
                let v = (i as f32 * 1000.0 / 44_100.0 * std::f32::consts::TAU).sin() * 0.5;
                [v, v]
            })
            .collect();
        let out = resample(&input, 44_100);
        assert_eq!(out.len(), 48_000 * 2);
        let mut err = 0f32;
        for n in 1_000..47_000 {
            let want = (n as f32 * 1000.0 / 48_000.0 * std::f32::consts::TAU).sin() * 0.5;
            err = err.max((out[n * 2] - want).abs());
        }
        assert!(err < 0.005, "max error {err}");
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
        assert_eq!(buf[0], 0.01, "starts at frame 100");
        assert_eq!(buf[199 * 2], 0.0299, "frame 299 is the last");
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
