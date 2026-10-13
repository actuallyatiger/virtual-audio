//! Audio file decoding (Symphonia) to interleaved stereo f32 at
//! [`SAMPLE_RATE`], in pieces: the soundboard decodes whole sounds with it,
//! and music streams tracks from disk a packet at a time.

use crate::mixer::{CHANNELS, SAMPLE_RATE};
use anyhow::{Context, anyhow, bail};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

/// Audio file extensions we try to play.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "ogg", "oga", "flac", "m4a", "mp4", "aac", "mka", "mkv", "webm",
];

/// Reads an audio file packet by packet.
pub struct TrackDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    /// Created at the first packet, when the sample rate is known for sure.
    resampler: Option<Resampler>,
    rate: Option<u32>,
    pub duration_ms: Option<u64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    ended: bool,
    stereo: Vec<f32>,
}

impl TrackDecoder {
    /// `extension` helps detect the format, which is otherwise probed.
    pub fn open(source: Box<dyn MediaSource>, extension: Option<&str>) -> anyhow::Result<Self> {
        let mss = MediaSourceStream::new(source, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = extension {
            hint.with_extension(ext);
        }
        let mut probed = symphonia::default::get_probe()
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
        let params = track.codec_params.clone();
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|_| {
                anyhow!("this file's audio format isn't supported (use MP3, WAV, OGG, FLAC or M4A)")
            })?;
        let duration_ms = match (params.n_frames, params.sample_rate) {
            (Some(n), Some(rate)) if rate > 0 => Some(n * 1000 / rate as u64),
            _ => None,
        };

        // Tags can be in the file's header (probe) or its container.
        let (mut title, mut artist) = (None, None);
        let mut read_tags = |rev: Option<&symphonia::core::meta::MetadataRevision>| {
            for tag in rev.map(|r| r.tags()).unwrap_or(&[]) {
                match tag.std_key {
                    Some(StandardTagKey::TrackTitle) if title.is_none() => {
                        title = Some(tag.value.to_string())
                    }
                    Some(StandardTagKey::Artist) if artist.is_none() => {
                        artist = Some(tag.value.to_string())
                    }
                    _ => {}
                }
            }
        };
        if let Some(meta) = probed.metadata.get() {
            read_tags(meta.current());
        }
        read_tags(format.metadata().current());

        Ok(Self {
            format,
            decoder,
            track_id,
            resampler: None,
            rate: None,
            duration_ms,
            title: title.filter(|t| !t.trim().is_empty()),
            artist: artist.filter(|a| !a.trim().is_empty()),
            ended: false,
            stereo: Vec::new(),
        })
    }

    pub fn open_path(path: &std::path::Path) -> anyhow::Result<Self> {
        let file =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        Self::open(Box::new(file), ext.as_deref())
    }

    /// Jumps to `ms` from the start.
    pub fn seek(&mut self, ms: u64) -> anyhow::Result<()> {
        let time = Time::new(ms / 1000, (ms % 1000) as f64 / 1000.0);
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .context("seeking")?;
        self.decoder.reset();
        // Start resampling afresh from the new position.
        self.resampler = self.rate.map(Resampler::new);
        self.ended = false;
        Ok(())
    }

    /// Decodes the next packet, appending stereo audio at [`SAMPLE_RATE`]
    /// to `out`. Returns `false` once the file has ended (after flushing
    /// the resampler).
    pub fn next(&mut self, out: &mut Vec<f32>) -> anyhow::Result<bool> {
        if self.ended {
            return Ok(false);
        }
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(self.end(out));
                }
                Err(Error::ResetRequired) => return Ok(self.end(out)),
                Err(e) => return Err(e).context("reading the file"),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
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
            if *self.rate.get_or_insert(spec.rate) != spec.rate {
                bail!("the sample rate changes partway through the file");
            }
            let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            buf.copy_interleaved_ref(decoded);
            let channels = spec.channels.count();
            self.stereo.clear();
            for frame in buf.samples().chunks_exact(channels) {
                let (l, r) = match frame {
                    [m] => (*m, *m),
                    [l, r, ..] => (*l, *r),
                    [] => unreachable!(),
                };
                self.stereo.extend([l, r]);
            }
            self.resampler
                .get_or_insert_with(|| Resampler::new(spec.rate))
                .push(&self.stereo, out);
            return Ok(true);
        }
    }

    fn end(&mut self, out: &mut Vec<f32>) -> bool {
        self.ended = true;
        if let Some(r) = &mut self.resampler {
            r.finish(out);
        }
        false
    }

    /// Whether any audio was decoded so far.
    pub fn has_audio(&self) -> bool {
        self.rate.is_some()
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Converts interleaved stereo from one rate to [`SAMPLE_RATE`] with a
/// windowed-sinc (Blackman) filter, using a precomputed kernel per phase.
/// Streaming: feed it pieces with [`Resampler::push`], then
/// [`Resampler::finish`] for the tail.
pub struct Resampler {
    up: u64,
    down: u64,
    half: usize,
    taps: usize,
    table: Vec<f32>,
    passthrough: bool,
    /// Input frames still needed; `input[0]` is input frame `first`.
    input: Vec<f32>,
    first: u64,
    received: u64,
    /// The next output frame.
    next: u64,
}

impl Resampler {
    pub fn new(from: u32) -> Self {
        let to = SAMPLE_RATE;
        let passthrough = from == to || from == 0;
        let g = gcd(from.max(1), to);
        let (up, down) = ((to / g) as usize, (from.max(1) / g) as usize);
        // Cut off just below the lower of the two Nyquist frequencies, as a
        // fraction of the input's.
        let cutoff = (to as f64 / from.max(1) as f64).min(1.0) * 0.95;
        let half = (16.0 / cutoff).ceil() as usize;
        let taps = 2 * half;
        let mut table = Vec::new();
        if !passthrough {
            table = vec![0f32; up * taps];
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
        }
        Self {
            up: up as u64,
            down: down as u64,
            half,
            taps,
            table,
            passthrough,
            input: Vec::new(),
            first: 0,
            received: 0,
            next: 0,
        }
    }

    pub fn push(&mut self, frames: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(frames);
            return;
        }
        self.input.extend_from_slice(frames);
        self.received += (frames.len() / CHANNELS) as u64;
        self.produce(out, self.received, u64::MAX);
    }

    /// Produces the remaining output, treating the input after its end as
    /// silence.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.passthrough {
            return;
        }
        let total = self.received * self.up / self.down;
        self.produce(out, u64::MAX, total);
    }

    /// Computes output frames whose input window lies before `available`,
    /// stopping at output frame `limit`.
    fn produce(&mut self, out: &mut Vec<f32>, available: u64, limit: u64) {
        let half = self.half as u64;
        while self.next < limit {
            let pos = self.next * self.down;
            let base = pos / self.up;
            if base + half >= available {
                break;
            }
            let row = &self.table[(pos % self.up) as usize * self.taps..][..self.taps];
            let first = base as i64 + 1 - half as i64;
            let (mut l, mut r) = (0f32, 0f32);
            for (j, &k) in row.iter().enumerate() {
                let i = first + j as i64;
                if i < self.first as i64 || i as u64 >= self.received {
                    continue; // before the start or past the end: silence
                }
                let i = (i as u64 - self.first) as usize * CHANNELS;
                l += self.input[i] * k;
                r += self.input[i + 1] * k;
            }
            out.extend([l, r]);
            self.next += 1;
        }
        // Drop input that no later output frame needs.
        let needed = (self.next * self.down / self.up) as i64 + 1 - half as i64;
        if needed > self.first as i64 {
            let drop = ((needed as u64 - self.first) as usize).min(self.input.len() / CHANNELS);
            self.input.drain(..drop * CHANNELS);
            self.first += drop as u64;
        }
    }
}

/// Resamples a whole buffer.
/// A 16-bit mono 48 kHz WAV file `frames` long, for tests.
#[cfg(test)]
pub fn test_wav(frames: usize) -> Vec<u8> {
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

#[cfg(test)]
pub fn resample(input: &[f32], from: u32) -> Vec<f32> {
    let mut r = Resampler::new(from);
    let mut out = Vec::new();
    r.push(input, &mut out);
    r.finish(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, frames: usize, freq: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (i as f32 * freq / rate as f32 * std::f32::consts::TAU).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn resampling_keeps_a_tone_clean() {
        // 1 kHz at 44.1 kHz → 48 kHz: still 1 kHz, no loss of level.
        let out = resample(&tone(44_100, 44_100, 1000.0), 44_100);
        assert_eq!(out.len(), 48_000 * 2);
        let mut err = 0f32;
        for n in 1_000..47_000 {
            let want = (n as f32 * 1000.0 / 48_000.0 * std::f32::consts::TAU).sin() * 0.5;
            err = err.max((out[n * 2] - want).abs());
        }
        assert!(err < 0.005, "max error {err}");
    }

    #[test]
    fn streaming_matches_resampling_in_one_go() {
        let input = tone(22_050, 10_000, 440.0);
        let whole = resample(&input, 22_050);
        let mut r = Resampler::new(22_050);
        let mut pieces = Vec::new();
        for chunk in input.chunks(2 * 333) {
            r.push(chunk, &mut pieces);
        }
        r.finish(&mut pieces);
        assert_eq!(pieces.len(), whole.len());
        assert!(pieces.iter().zip(&whole).all(|(a, b)| (a - b).abs() < 1e-6));
        assert!(
            r.input.len() < 2 * r.taps * CHANNELS,
            "old input is dropped"
        );
    }

    #[test]
    fn same_rate_passes_through() {
        let input = tone(48_000, 1_000, 440.0);
        assert_eq!(resample(&input, 48_000), input);
    }
}
