//! Real-time mixing core. Platform independent so it can be unit tested.
//!
//! Every source delivers interleaved stereo f32 at [`SAMPLE_RATE`] into its
//! own single-producer/single-consumer ring buffer. The render thread calls
//! [`Mixer::process`] whenever the output device wants more audio.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// A source starts playing once this much audio is buffered (20 ms).
const PRIME_FRAMES: usize = 960;
/// If a source gets this far ahead (clock drift, stalls) it is trimmed back.
const MAX_FRAMES: usize = 4_800;
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
}

pub struct Mixer {
    inputs: Vec<Input>,
    master: Arc<ChannelControl>,
    master_gain: f32,
    scratch: Vec<f32>,
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
            scratch: Vec::new(),
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
        });
    }

    pub fn remove_input(&mut self, id: &str) {
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

        for input in &mut self.inputs {
            let mut avail = input.rx.slots() / CHANNELS;
            if !input.primed {
                if avail < PRIME_FRAMES {
                    input.ctrl.update_peak(0.0, decay);
                    continue;
                }
                input.primed = true;
            }
            if avail > MAX_FRAMES {
                let skip = (avail - PRIME_FRAMES) * CHANNELS;
                if let Ok(chunk) = input.rx.read_chunk(skip) {
                    chunk.commit_all();
                }
                avail = PRIME_FRAMES;
            }

            let n = avail.min(frames) * CHANNELS;
            let buf = &mut self.scratch[..out.len()];
            if let Ok(chunk) = input.rx.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                buf[..a.len()].copy_from_slice(a);
                buf[a.len()..n].copy_from_slice(b);
                chunk.commit_all();
            }
            buf[n..].fill(0.0);
            if n < out.len() {
                // Underrun: wait until we have a cushion again.
                input.primed = false;
            }

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
        assert_eq!(tx.slots(), RING_SAMPLES - (PRIME_FRAMES - 480) * CHANNELS);
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
