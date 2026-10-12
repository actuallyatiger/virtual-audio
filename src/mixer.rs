//! Real-time mixing core. Platform independent so it can be unit tested.
//!
//! Every source delivers interleaved stereo f32 at [`SAMPLE_RATE`] into its
//! own single-producer/single-consumer ring buffer. The render thread calls
//! [`Mixer::process`] whenever the output device wants more audio.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// A source starts playing once this much audio is buffered (30 ms), and
/// drift correction steers each source's buffer back towards this level.
const TARGET_FRAMES: usize = 1_440;
/// How far the smoothed fill level may stray before drift correction acts.
const DRIFT_DEADBAND: f32 = 240.0;
/// Smoothing for the fill level (per process call, ~1 s time constant).
const FILL_SMOOTHING: f32 = 0.01;
/// If a source gets this far ahead (e.g. after a stall) it is trimmed back.
const MAX_FRAMES: usize = 4_800;
/// Largest render period we pre-allocate for (100 ms; WASAPI uses ~10-20 ms),
/// so the audio thread doesn't hit the allocator.
const MAX_BLOCK_FRAMES: usize = 4_800;
/// Ring buffer size in samples (0.5 s of stereo audio).
pub const RING_SAMPLES: usize = SAMPLE_RATE as usize;

/// Lock-free controls shared between the UI/hotkey side and the audio thread.
#[derive(Debug)]
pub struct ChannelControl {
    gain: AtomicU32,
    muted: AtomicBool,
    /// Closed by push-to-talk / push-to-mute hotkeys.
    gate_open: AtomicBool,
    peak: AtomicU32,
}

impl ChannelControl {
    pub fn new(gain: f32, muted: bool) -> Self {
        Self {
            gain: AtomicU32::new(gain.to_bits()),
            muted: AtomicBool::new(muted),
            gate_open: AtomicBool::new(true),
            peak: AtomicU32::new(0f32.to_bits()),
        }
    }
    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    pub fn set_gate_open(&self, open: bool) {
        self.gate_open.store(open, Ordering::Relaxed);
    }
    pub fn gate_open(&self) -> bool {
        self.gate_open.load(Ordering::Relaxed)
    }
    /// Peak level (linear, decaying) for meters.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }
    fn reset_peak(&self) {
        self.peak.store(0f32.to_bits(), Ordering::Relaxed);
    }
    fn update_peak(&self, block_peak: f32, decay: f32) {
        let held = self.peak() * decay;
        self.peak
            .store(block_peak.max(held).to_bits(), Ordering::Relaxed);
    }
    fn target_gain(&self) -> f32 {
        if self.muted() || !self.gate_open() {
            0.0
        } else {
            self.gain()
        }
    }
}

struct Input {
    id: String,
    ctrl: Arc<ChannelControl>,
    rx: rtrb::Consumer<f32>,
    current_gain: f32,
    primed: bool,
    /// Smoothed number of buffered frames, for drift correction.
    fill: f32,
}

pub struct Mixer {
    inputs: Vec<Input>,
    master: Arc<ChannelControl>,
    master_gain: f32,
    scratch: Vec<f32>,
    /// Holds a source's raw frames before they are stretched to fit.
    read_buf: Vec<f32>,
}

/// Creates the ring buffer a capture thread writes into.
pub fn source_ring() -> (rtrb::Producer<f32>, rtrb::Consumer<f32>) {
    rtrb::RingBuffer::new(RING_SAMPLES)
}

/// Pushes as many whole frames as fit; drops the rest (consumer stalled).
pub fn push_frames(tx: &mut rtrb::Producer<f32>, data: &[f32]) {
    let n = data.len().min(tx.slots());
    let n = n - n % CHANNELS;
    if n == 0 {
        return;
    }
    if let Ok(chunk) = tx.write_chunk_uninit(n) {
        chunk.fill_from_iter(data[..n].iter().copied());
    }
}

impl Mixer {
    pub fn new(master: Arc<ChannelControl>) -> Self {
        let master_gain = master.target_gain();
        Self {
            inputs: Vec::new(),
            master,
            master_gain,
            scratch: Vec::with_capacity(MAX_BLOCK_FRAMES * CHANNELS),
            read_buf: Vec::with_capacity((MAX_BLOCK_FRAMES + 1) * CHANNELS),
        }
    }

    /// Adds a source, replacing any existing one with the same id.
    pub fn set_input(&mut self, id: &str, ctrl: Arc<ChannelControl>, rx: rtrb::Consumer<f32>) {
        self.remove_input(id);
        let current_gain = ctrl.target_gain();
        self.inputs.push(Input {
            id: id.to_owned(),
            ctrl,
            rx,
            current_gain,
            primed: false,
            fill: 0.0,
        });
    }

    pub fn remove_input(&mut self, id: &str) {
        // Meters only decay while an input is mixed, so zero a removed one
        // rather than leave it frozen at its last level.
        for input in self.inputs.iter().filter(|i| i.id == id) {
            input.ctrl.reset_peak();
        }
        self.inputs.retain(|i| i.id != id);
    }

    /// Fills `out` (interleaved stereo) with the current mix.
    pub fn process(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        // Meters fall by half every 150 ms.
        let decay = 0.5f32.powf(frames as f32 / (SAMPLE_RATE as f32 * 0.15));
        if self.scratch.len() < out.len() {
            self.scratch.resize(out.len(), 0.0);
        }

        let Mixer {
            inputs,
            scratch,
            read_buf,
            ..
        } = self;
        for input in inputs.iter_mut() {
            let mut avail = input.rx.slots() / CHANNELS;
            if !input.primed {
                if avail < TARGET_FRAMES {
                    input.ctrl.update_peak(0.0, decay);
                    continue;
                }
                input.primed = true;
                input.fill = avail as f32;
                // Fade in from silence rather than starting mid-waveform.
                input.current_gain = 0.0;
            }
            if avail > MAX_FRAMES {
                let skip = (avail - TARGET_FRAMES) * CHANNELS;
                if let Ok(chunk) = input.rx.read_chunk(skip) {
                    chunk.commit_all();
                }
                avail = TARGET_FRAMES;
                input.fill = avail as f32;
            }

            // Clock drift: the source and output devices run on separate
            // clocks, so the buffer slowly fills or drains. Consume one frame
            // more or less than needed and stretch it to fit (a ~0.2% pitch
            // change for one block) to steer back towards the target.
            input.fill += (avail as f32 - input.fill) * FILL_SMOOTHING;
            let want = if frames < 64 {
                frames
            } else if input.fill > TARGET_FRAMES as f32 + DRIFT_DEADBAND {
                frames + 1
            } else if input.fill < TARGET_FRAMES as f32 - DRIFT_DEADBAND {
                frames - 1
            } else {
                frames
            };

            let got = avail.min(want) * CHANNELS;
            read_buf.resize(got, 0.0);
            if let Ok(chunk) = input.rx.read_chunk(got) {
                let (a, b) = chunk.as_slices();
                read_buf[..a.len()].copy_from_slice(a);
                read_buf[a.len()..].copy_from_slice(b);
                chunk.commit_all();
            }
            let buf = &mut scratch[..out.len()];
            let n = if got == want * CHANNELS {
                stretch(read_buf, buf);
                out.len()
            } else {
                // Underrun: play what we have, fade it out instead of cutting
                // off, then wait until there is a cushion again (and fade in).
                let n = got.min(out.len());
                buf[..n].copy_from_slice(&read_buf[..n]);
                buf[n..].fill(0.0);
                fade_out_tail(&mut buf[..n]);
                input.primed = false;
                n
            };

            let user_gain = input.ctrl.gain();
            let peak = buf[..n].iter().fold(0f32, |m, s| m.max(s.abs())) * user_gain;
            input.ctrl.update_peak(peak, decay);

            let target = input.ctrl.target_gain();
            apply_ramped(buf, input.current_gain, target, out);
            input.current_gain = target;
        }

        let target = self.master.target_gain();
        let start = self.master_gain;
        let step = (target - start) / frames as f32;
        let mut peak = 0f32;
        for (f, frame) in out.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
            let g = start + step * (f + 1) as f32;
            for s in frame {
                *s = soft_clip(*s * g);
                peak = peak.max(s.abs());
            }
        }
        self.master_gain = target;
        self.master.update_peak(peak, decay);
    }
}

/// Adds `src * gain` into `out`, ramping gain linearly to avoid clicks.
fn apply_ramped(src: &[f32], from: f32, to: f32, out: &mut [f32]) {
    let frames = out.len() / CHANNELS;
    if from == to {
        if to == 0.0 {
            return;
        }
        for (o, s) in out.iter_mut().zip(src) {
            *o += s * to;
        }
        return;
    }
    let step = (to - from) / frames as f32;
    for (f, (o, s)) in out
        .as_chunks_mut::<CHANNELS>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<CHANNELS>().0)
        .enumerate()
    {
        let g = from + step * (f + 1) as f32;
        for (o, s) in o.iter_mut().zip(s) {
            *o += s * g;
        }
    }
}

/// Linearly resamples interleaved audio from `src` to fill `dst`, mapping the
/// first and last frames onto each other. Copies when the lengths match.
fn stretch(src: &[f32], dst: &mut [f32]) {
    let (m, n) = (src.len() / CHANNELS, dst.len() / CHANNELS);
    if m == n || m < 2 || n < 2 {
        let len = src.len().min(dst.len());
        dst[..len].copy_from_slice(&src[..len]);
        dst[len..].fill(0.0);
        return;
    }
    let ratio = (m - 1) as f32 / (n - 1) as f32;
    let src = src.as_chunks::<CHANNELS>().0;
    for (i, frame) in dst.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
        let pos = i as f32 * ratio;
        let j = (pos as usize).min(m - 1);
        let t = pos - j as f32;
        let next = src[(j + 1).min(m - 1)];
        for (c, s) in frame.iter_mut().enumerate() {
            *s = src[j][c] * (1.0 - t) + next[c] * t;
        }
    }
}

/// Ramps the last (up to) 1 ms of interleaved audio down to silence.
fn fade_out_tail(buf: &mut [f32]) {
    let frames = buf.len() / CHANNELS;
    let fade = frames.min(SAMPLE_RATE as usize / 1000);
    let start = frames - fade;
    for (i, frame) in buf.as_chunks_mut::<CHANNELS>().0[start..]
        .iter_mut()
        .enumerate()
    {
        let g = 1.0 - (i + 1) as f32 / fade as f32;
        for s in frame {
            *s *= g;
        }
    }
}

/// Linear up to 0.8, then smoothly saturates towards 1.0.
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let y = KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh();
        y.copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tx: &mut rtrb::Producer<f32>, frames: usize, value: f32) {
        push_frames(tx, &vec![value; frames * CHANNELS]);
    }

    #[test]
    fn waits_for_prime_then_mixes_sources() {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master.clone());
        let (mut tx1, rx1) = source_ring();
        let (mut tx2, rx2) = source_ring();
        mixer.set_input("a", Arc::new(ChannelControl::new(1.0, false)), rx1);
        mixer.set_input("b", Arc::new(ChannelControl::new(0.5, false)), rx2);

        let mut out = vec![0.0; 480 * CHANNELS];
        feed(&mut tx1, 480, 0.2);
        mixer.process(&mut out);
        assert!(out.iter().all(|&s| s == 0.0), "not primed yet");

        feed(&mut tx1, 960, 0.2);
        feed(&mut tx2, 2000, 0.4);
        mixer.process(&mut out);
        assert!(out[0].abs() < 0.01, "fades in after priming");
        mixer.process(&mut out);
        for s in &out {
            assert!((s - 0.4).abs() < 1e-6, "0.2*1 + 0.4*0.5, got {s}");
        }
        assert!(master.peak() > 0.39);
    }

    #[test]
    fn mute_ramps_to_silence() {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master);
        let ctrl = Arc::new(ChannelControl::new(1.0, false));
        let (mut tx, rx) = source_ring();
        mixer.set_input("a", ctrl.clone(), rx);
        feed(&mut tx, 4000, 0.5);

        let mut out = vec![0.0; 480 * CHANNELS];
        mixer.process(&mut out);
        ctrl.set_muted(true);
        mixer.process(&mut out);
        assert!(out[0] > 0.4, "ramp starts near previous gain");
        assert!(out[out.len() - 1].abs() < 1e-6, "ramp ends silent");
        mixer.process(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
        // Meter still shows the (muted) input so the user can see it is live.
        assert!(ctrl.peak() > 0.4);
    }

    #[test]
    fn gate_closes_master() {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master.clone());
        let (mut tx, rx) = source_ring();
        mixer.set_input("a", Arc::new(ChannelControl::new(1.0, false)), rx);
        feed(&mut tx, 4000, 0.5);
        master.set_gate_open(false);
        let mut out = vec![0.0; 480 * CHANNELS];
        mixer.process(&mut out);
        mixer.process(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn trims_excess_latency() {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master);
        let (mut tx, rx) = source_ring();
        mixer.set_input("a", Arc::new(ChannelControl::new(1.0, false)), rx);
        feed(&mut tx, MAX_FRAMES * 2, 0.1);
        let mut out = vec![0.0; 480 * CHANNELS];
        mixer.process(&mut out);
        assert_eq!(tx.slots(), RING_SAMPLES - (TARGET_FRAMES - 480) * CHANNELS);
    }

    #[test]
    fn underrun_fades_out_then_back_in() {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master);
        let (mut tx, rx) = source_ring();
        mixer.set_input("a", Arc::new(ChannelControl::new(1.0, false)), rx);
        let mut out = vec![0.0; 480 * CHANNELS];

        // Three full blocks (the first fades in), then only 200 frames left.
        feed(&mut tx, 3 * 480 + 200, 0.5);
        for _ in 0..3 {
            mixer.process(&mut out);
        }
        assert!((out[0] - 0.5).abs() < 1e-6, "steady before the underrun");
        mixer.process(&mut out);
        assert!(
            (out[100 * CHANNELS] - 0.5).abs() < 1e-6,
            "audio before the fade is intact"
        );
        assert!(
            out[199 * CHANNELS].abs() < 0.01,
            "faded out at the end of the data"
        );
        assert!(out[300 * CHANNELS..].iter().all(|&s| s == 0.0));

        feed(&mut tx, TARGET_FRAMES, 0.5);
        mixer.process(&mut out); // re-primed
        assert!(out[0].abs() < 0.01, "fades back in");
        assert!((out[out.len() - 1] - 0.5).abs() < 1e-6);
    }

    /// Simulates a source whose clock runs `ppm` fast or slow relative to the
    /// output, delivering 10 ms packets with up to 4 ms of jitter, for ten
    /// virtual minutes (at 200 ppm that is ~5800 frames of drift, far more than
    /// the buffer could absorb). With drift correction it must never glitch.
    fn simulate_drift(ppm: f64) {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master);
        let ctrl = Arc::new(ChannelControl::new(1.0, false));
        let (mut tx, rx) = source_ring();
        mixer.set_input("a", ctrl, rx);
        let mut out = vec![0.0; 480 * CHANNELS];

        let packet_ms = 10.0 / (1.0 + ppm * 1e-6);
        let mut seed = 0x2545_f491_u32;
        let mut jitter = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 4000) as f64 / 1000.0
        };
        let mut next_packet = 0.0;
        let mut due = next_packet + jitter();
        for ms in 0..600_000u64 {
            let now = ms as f64;
            while due <= now {
                feed(&mut tx, 480, 0.5);
                next_packet += packet_ms;
                due = next_packet + jitter();
            }
            if ms % 10 == 5 {
                mixer.process(&mut out);
                if ms > 1_000 {
                    let bad = out.iter().position(|s| (s - 0.5).abs() > 1e-4);
                    assert!(bad.is_none(), "glitch at {ms} ms with {ppm} ppm drift");
                }
            }
        }
        let buffered = (RING_SAMPLES - tx.slots()) / CHANNELS;
        assert!(
            buffered.abs_diff(TARGET_FRAMES) < 1_000,
            "buffer drifted to {buffered} frames with {ppm} ppm"
        );
    }

    #[test]
    fn drift_correction_slow_source() {
        simulate_drift(-200.0);
    }

    #[test]
    fn drift_correction_fast_source() {
        simulate_drift(200.0);
    }

    #[test]
    fn stretch_resamples_linearly() {
        let src: Vec<f32> = (0..4).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let mut dst = vec![0.0; 7 * CHANNELS];
        stretch(&src, &mut dst);
        let left: Vec<f32> = dst.iter().step_by(2).copied().collect();
        assert_eq!(left, vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0]);
        assert_eq!(dst[13], -3.0);
    }

    #[test]
    fn soft_clip_is_bounded_and_continuous() {
        assert_eq!(soft_clip(0.5), 0.5);
        assert!(soft_clip(10.0) <= 1.0);
        assert!(soft_clip(-10.0) >= -1.0);
        assert!((soft_clip(0.8001) - 0.8001).abs() < 1e-3);
    }

    #[test]
    fn replacing_input_keeps_single_entry() {
        let master = Arc::new(ChannelControl::new(1.0, false));
        let mut mixer = Mixer::new(master);
        let ctrl = Arc::new(ChannelControl::new(1.0, false));
        mixer.set_input("a", ctrl.clone(), source_ring().1);
        mixer.set_input("a", ctrl, source_ring().1);
        assert_eq!(mixer.inputs.len(), 1);
    }
}
