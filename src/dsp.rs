//! Microphone clean-up, applied in a device source's capture thread before
//! its audio reaches the mixer: RNNoise noise suppression, automatic level
//! and a noise gate.
//!
//! The gate is either manual (opens above a level you set) or automatic: it
//! opens when RNNoise's voice detector hears speech and the level is
//! clearly above the room's background noise. The voice detector alone is
//! fooled by loud steady noise such as a fan, which the noise floor check
//! rules out. It is also fooled by knocks and clicks (a glass put down, a
//! finger snap): "extra strength" ignores those, which leap up and then only
//! fade, unlike speech. To tell without cutting off the start of a word
//! that begins just as suddenly, it looks 30 ms ahead, delaying the mic by
//! that much; the mixer can delay the other sources to match.
//!
//! Automatic level keeps speech at a steady level, as voice chat apps' gain
//! control does: it measures the frames RNNoise's voice detector calls
//! speech (those the gate lets through) and turns the mic up or down
//! towards a target, slowly, and only while you talk, so background noise
//! isn't raised in the pauses.
//!
//! While any of them is on, the input is downmixed to mono
//! (microphones are mono anyway) and processed in 10 ms frames: see
//! [`delay_frames`] for the delay that adds (the mixer delays the other
//! sources to match). With all of them off, audio passes through untouched.

use crate::mixer::{CHANNELS, SAMPLE_RATE};
use nnnoiseless::DenoiseState;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// RNNoise works on 10 ms frames at 48 kHz.
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// The gate stays open this long after the level drops below the threshold,
/// so it doesn't chop the ends of words.
const HOLD_FRAMES: u32 = 20; // 200 ms
/// Gate gain change per sample: opens in ~5 ms, closes over ~100 ms.
const ATTACK_STEP: f32 = 1.0 / (SAMPLE_RATE as f32 * 0.005);
const RELEASE_STEP: f32 = 1.0 / (SAMPLE_RATE as f32 * 0.1);
/// RNNoise expects samples scaled like 16-bit integers.
const I16_SCALE: f32 = 32_768.0;
/// Automatic gate: RNNoise's voice probability needed to open.
const VOICE_PROBABILITY: f32 = 0.6;
/// Automatic gate: how far above the background noise speech must be.
const FLOOR_MARGIN_DB: f32 = 6.0;
/// The noise floor follows quieter frames at once, and creeps up 2 dB/s
/// otherwise, so it settles on the room's steady noise but not on speech.
const FLOOR_RISE_DB: f32 = 0.02;
/// Extra strength: frames held back so the gate can look ahead.
const LOOKAHEAD: usize = 3;
/// How much extra strength delays the mic (30 ms), in frames of audio.
pub const LOOKAHEAD_FRAMES: usize = LOOKAHEAD * FRAME;

/// How far behind its input a mic's processed audio comes out, in frames:
/// RNNoise's output lags by one frame (10 ms), and extra strength's
/// look-ahead adds 30 ms. (Gathering 10 ms frames adds up to another 10 ms,
/// but none while the input arrives in 10 ms blocks, as it usually does.)
pub fn delay_frames(suppress: bool, extra_strength: bool) -> usize {
    (if suppress { FRAME } else { 0 }) + if extra_strength { LOOKAHEAD_FRAMES } else { 0 }
}
/// Automatic level: the speech level (RMS of voiced frames, dBFS) to aim
/// for, a normal speaking level in voice chat.
const LEVEL_TARGET_DB: f32 = -18.0;
/// How far it may turn the mic down or up.
const LEVEL_MIN_DB: f32 = -12.0;
const LEVEL_MAX_DB: f32 = 20.0;
/// How quickly the measured speech level follows each voiced frame (about
/// half a second of speech).
const LEVEL_SMOOTHING: f32 = 0.02;
/// Gain change per 10 ms frame: up 3 dB/s, down 10 dB/s.
const LEVEL_RISE_DB: f32 = 0.03;
const LEVEL_FALL_DB: f32 = 0.1;
/// Peaks aren't turned up past this (-1 dBFS): it only limits the boost,
/// never turning a loud mic below where it was.
const LEVEL_CEILING: f32 = 0.89;
/// Gain changes per sample, scaled to the whole range (×10, +20 dB): a cut
/// for a peak lands within 1 ms (before the peak, as the whole frame is at
/// hand), other cuts take 5 ms, and rises 50 ms.
const LEVEL_CUT_STEP: f32 = 10.0 / (SAMPLE_RATE as f32 * 0.001);
const LEVEL_DOWN_STEP: f32 = 10.0 / (SAMPLE_RATE as f32 * 0.005);
const LEVEL_UP_STEP: f32 = 10.0 / (SAMPLE_RATE as f32 * 0.05);

/// A knock or click: the level leaps this much from one frame to the next.
const KNOCK_JUMP_DB: f32 = 20.0;
/// ...and it's over once the level rises again by this much (someone talks
/// over the fading sound), or falls less than 1 dB over 30 ms (it holds).
const KNOCK_RISE_DB: f32 = 3.0;

/// Settings and live readings shared between the UI and a capture thread.
#[derive(Debug)]
pub struct ProcessingControl {
    suppress: AtomicBool,
    gate: AtomicBool,
    /// Automatic (voice detection) rather than a manual threshold.
    gate_auto: AtomicBool,
    threshold_db: AtomicU32,
    /// Extra strength: the automatic gate also ignores knocks and clicks.
    extra_strength: AtomicBool,
    auto_level: AtomicBool,
    /// Automatic level's adjustment now (dB), shown in the UI and where it
    /// starts again after the capture restarts.
    level_db: AtomicU32,
    /// Peak input level (linear, decaying) after suppression, before the gate.
    level: AtomicU32,
    /// Peak of the raw input in the latest block, before any processing.
    input_level: AtomicU32,
    gate_open: AtomicBool,
}

impl ProcessingControl {
    pub fn new(suppress: bool, gate: bool, gate_auto: bool, threshold_db: f32) -> Self {
        Self {
            suppress: AtomicBool::new(suppress),
            gate: AtomicBool::new(gate),
            gate_auto: AtomicBool::new(gate_auto),
            threshold_db: AtomicU32::new(threshold_db.to_bits()),
            extra_strength: AtomicBool::new(false),
            auto_level: AtomicBool::new(false),
            level_db: AtomicU32::new(0f32.to_bits()),
            level: AtomicU32::new(0f32.to_bits()),
            input_level: AtomicU32::new(0f32.to_bits()),
            gate_open: AtomicBool::new(true),
        }
    }
    pub fn set_suppress(&self, on: bool) {
        self.suppress.store(on, Ordering::Relaxed);
    }
    pub fn set_gate(&self, on: bool) {
        self.gate.store(on, Ordering::Relaxed);
    }
    pub fn set_gate_auto(&self, auto: bool) {
        self.gate_auto.store(auto, Ordering::Relaxed);
    }
    pub fn set_extra_strength(&self, on: bool) {
        self.extra_strength.store(on, Ordering::Relaxed);
    }
    pub fn set_auto_level(&self, on: bool) {
        self.auto_level.store(on, Ordering::Relaxed);
    }
    pub fn auto_level(&self) -> bool {
        self.auto_level.load(Ordering::Relaxed)
    }
    /// Automatic level's adjustment (dB), also while it is off (where it
    /// starts when turned back on).
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level_db.load(Ordering::Relaxed))
    }
    /// Where automatic level starts (e.g. where it settled last time).
    pub fn set_level_db(&self, db: f32) {
        let db = if db.is_finite() {
            db.clamp(LEVEL_MIN_DB, LEVEL_MAX_DB)
        } else {
            0.0
        };
        self.level_db.store(db.to_bits(), Ordering::Relaxed);
    }
    pub fn set_threshold_db(&self, db: f32) {
        self.threshold_db.store(db.to_bits(), Ordering::Relaxed);
    }
    fn threshold_db(&self) -> f32 {
        f32::from_bits(self.threshold_db.load(Ordering::Relaxed))
    }
    pub fn gate_enabled(&self) -> bool {
        self.gate.load(Ordering::Relaxed)
    }
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    /// Raw input peak (linear): unlike [`Self::level`], it isn't lowered by
    /// noise suppression, so it tells a quiet room from a dead mic.
    pub fn input_level(&self) -> f32 {
        f32::from_bits(self.input_level.load(Ordering::Relaxed))
    }
    /// Whether the gate currently lets audio through (true while it is off).
    pub fn gate_open(&self) -> bool {
        self.gate_open.load(Ordering::Relaxed)
    }
}

pub struct Processor {
    ctrl: std::sync::Arc<ProcessingControl>,
    /// Created while suppression or the automatic gate (which uses its
    /// voice detector) needs it, dropped otherwise so turning them back on
    /// starts from a clean state.
    denoise: Option<Box<DenoiseState<'static>>>,
    /// Mono samples waiting for a full frame.
    pending: Vec<f32>,
    frame: Vec<f32>,
    /// Background noise level (dBFS) for the automatic gate.
    floor_db: f32,
    /// Input level (dBFS) of the last few frames, newest last.
    recent_db: [f32; 4],
    /// Frames since a knock or click began, while it is still fading; with
    /// extra strength, it can't open the automatic gate.
    knock: Option<u32>,
    /// Extra strength's look-ahead: processed frames not yet let out,
    /// oldest first, each with whether it would open the gate.
    ahead: VecDeque<([f32; FRAME], bool)>,
    out: Vec<f32>,
    gain: f32,
    hold: u32,
    level: f32,
    /// Automatic level: the speech level measured so far (dBFS), its
    /// adjustment (dB), and the gain the last frame ended on.
    speech_db: Option<f32>,
    level_db: f32,
    level_gain: f32,
    /// The most gain each sample of the frame may have (see `auto_level`).
    level_caps: [f32; FRAME],
}

impl Processor {
    pub fn new(ctrl: std::sync::Arc<ProcessingControl>) -> Self {
        Self {
            denoise: None,
            pending: Vec::with_capacity(FRAME),
            frame: vec![0.0; FRAME],
            floor_db: 0.0,
            recent_db: [-100.0; 4],
            knock: None,
            ahead: VecDeque::with_capacity(LOOKAHEAD + 1),
            out: Vec::with_capacity((LOOKAHEAD + 1) * FRAME * CHANNELS),
            gain: 1.0,
            hold: 0,
            level: 0.0,
            speech_db: None,
            level_db: ctrl.level_db(),
            level_gain: 1.0,
            level_caps: [0.0; FRAME],
            ctrl,
        }
    }

    /// Processes interleaved stereo `data`, handing the result to `sink`
    /// (possibly in different-sized pieces).
    pub fn process(&mut self, data: &[f32], sink: &mut dyn FnMut(&[f32])) {
        let raw = data.iter().fold(0f32, |m, s| m.max(s.abs()));
        self.ctrl
            .input_level
            .store(raw.to_bits(), Ordering::Relaxed);
        let suppress = self.ctrl.suppress.load(Ordering::Relaxed);
        let gate = self.ctrl.gate.load(Ordering::Relaxed);
        let levelling = self.ctrl.auto_level();
        // (While extra strength's held-back frames remain, or automatic
        // level's gain is still returning to 1, frames go through run_frame
        // until that's done.)
        if !suppress && !gate && !levelling && self.ahead.is_empty() && self.level_gain == 1.0 {
            self.denoise = None;
            self.gain = 1.0;
            self.hold = 0;
            self.ctrl.gate_open.store(true, Ordering::Relaxed);
            // Release what was buffered when processing was switched off.
            if !self.pending.is_empty() {
                self.out.clear();
                self.out
                    .extend(self.pending.drain(..).flat_map(|s| [s; CHANNELS]));
                sink(&self.out);
            }
            let peak = data.iter().fold(0f32, |m, s| m.max(s.abs()));
            self.publish_level(peak, data.len() / CHANNELS);
            sink(data);
            return;
        }
        for frame in data.as_chunks::<CHANNELS>().0 {
            self.pending
                .push(frame.iter().sum::<f32>() / CHANNELS as f32);
            if self.pending.len() == FRAME {
                self.run_frame(suppress, gate, levelling);
                if !self.out.is_empty() {
                    sink(&self.out);
                }
            }
        }
    }

    fn run_frame(&mut self, suppress: bool, gate: bool, levelling: bool) {
        let auto = gate && self.ctrl.gate_auto.load(Ordering::Relaxed);
        // Input level, before suppression, for the noise floor.
        let rms = (self.pending.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        let input_db = (20.0 * rms.log10()).max(-100.0);
        let mut voice = 0.0;
        if suppress || auto || levelling {
            let denoise = self.denoise.get_or_insert_with(DenoiseState::new);
            for s in &mut self.pending {
                *s *= I16_SCALE;
            }
            voice = denoise.process_frame(&mut self.frame, &self.pending);
            if suppress {
                for s in &mut self.frame {
                    *s = (*s / I16_SCALE).clamp(-1.0, 1.0);
                }
            } else {
                // Only the voice detector was wanted: keep the input.
                for (f, p) in self.frame.iter_mut().zip(&self.pending) {
                    *f = p / I16_SCALE;
                }
            }
        } else {
            self.denoise = None;
            self.frame.copy_from_slice(&self.pending);
        }
        self.pending.clear();

        if input_db < self.floor_db {
            self.floor_db = input_db;
        } else {
            self.floor_db += FLOOR_RISE_DB;
        }
        let audible = input_db >= self.floor_db + FLOOR_MARGIN_DB;
        self.track_knocks(input_db, audible);

        let peak = self.frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        self.publish_level(peak, FRAME);
        let speech = voice >= VOICE_PROBABILITY && audible;
        let above_threshold = peak > 0.0 && 20.0 * peak.log10() >= self.ctrl.threshold_db();
        // Automatic level learns only from speech the gate lets through: a
        // manual gate mutes quieter talk (someone else in the room), which
        // mustn't set your level. After the meter and the gate's level,
        // which are about the mic as it is.
        let heard = speech && self.knock.is_none() && (!gate || auto || above_threshold);
        self.auto_level(levelling, heard);
        self.out.clear();
        if auto && self.ctrl.extra_strength.load(Ordering::Relaxed) {
            // Hold this frame back and let out the one from 30 ms ago: it
            // opens the gate if it or a frame after it is speech (and not a
            // knock), so a word's start isn't cut while the gate makes sure.
            let mut frame = [0.0; FRAME];
            frame.copy_from_slice(&self.frame);
            self.ahead
                .push_back((frame, speech && self.knock.is_none()));
            if self.ahead.len() > LOOKAHEAD {
                let open = self.ahead.iter().any(|&(_, speech)| speech);
                let target = self.gate_target(open);
                if let Some((frame, _)) = self.ahead.pop_front() {
                    emit(&mut self.out, &mut self.gain, &frame, target);
                }
            } else {
                // Still filling up: nothing has been let out yet.
                self.ctrl.gate_open.store(self.hold > 0, Ordering::Relaxed);
            }
            return;
        }
        let target = if gate {
            let open = if auto { speech } else { above_threshold };
            self.gate_target(open)
        } else {
            self.ctrl.gate_open.store(true, Ordering::Relaxed);
            1.0
        };
        // Extra strength was just switched off. Letting out the 30 ms it held
        // back would leave the mic that late for seconds (the mixer only
        // trims a backlog slowly), so drop it, crossfading from where the
        // output left off into this frame.
        if let Some((held, _)) = self.ahead.pop_front() {
            for (i, (s, h)) in self.frame.iter_mut().zip(held).enumerate() {
                let t = (i + 1) as f32 / FRAME as f32;
                *s = h * (1.0 - t) + *s * t;
            }
            self.ahead.clear();
        }
        emit(&mut self.out, &mut self.gain, &self.frame, target);
    }

    /// Automatic level: learns the speech level from frames of `speech`
    /// and, during them, turns towards the target. Switched off, it returns
    /// to 1.
    fn auto_level(&mut self, on: bool, speech: bool) {
        let rms = (self.frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        if on && speech && rms > 0.0 {
            let db = 20.0 * rms.log10();
            let s = self.speech_db.get_or_insert(db);
            *s += (db - *s) * LEVEL_SMOOTHING;
            let want = (LEVEL_TARGET_DB - *s).clamp(LEVEL_MIN_DB, LEVEL_MAX_DB);
            self.level_db += (want - self.level_db).clamp(-LEVEL_FALL_DB, LEVEL_RISE_DB);
            self.ctrl.set_level_db(self.level_db);
        }
        let target = if on {
            10f32.powf(self.level_db / 20.0)
        } else {
            1.0
        };
        if self.level_gain == 1.0 && target == 1.0 {
            return;
        }
        // The most gain each sample may have so it doesn't pass the ceiling
        // (never below 1: this only limits the boost). Worked out backwards,
        // so the cut eases in before a loud sample rather than after it.
        let mut cap = f32::INFINITY;
        for (c, s) in self.level_caps.iter_mut().zip(&self.frame).rev() {
            cap = (cap + LEVEL_CUT_STEP).min((LEVEL_CEILING / s.abs()).max(1.0));
            *c = cap;
        }
        let mut gain = self.level_gain;
        for (s, &cap) in self.frame.iter_mut().zip(&self.level_caps) {
            gain = if target > gain {
                (gain + LEVEL_UP_STEP).min(target)
            } else {
                (gain - LEVEL_DOWN_STEP).max(target)
            };
            gain = gain.min(cap);
            *s *= gain;
        }
        self.level_gain = gain;
    }

    /// The gate's gain to head for, given whether this frame opens it: it
    /// stays open for the hold time after the last frame that did.
    fn gate_target(&mut self, open: bool) -> f32 {
        if open {
            self.hold = HOLD_FRAMES;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        self.ctrl.gate_open.store(self.hold > 0, Ordering::Relaxed);
        if self.hold > 0 { 1.0 } else { 0.0 }
    }

    /// Spots knocks and clicks: a leap in level, then nothing but fading.
    /// Speech builds, holds and wobbles instead.
    fn track_knocks(&mut self, input_db: f32, audible: bool) {
        let previous = self.recent_db[3];
        let thirty_ms_ago = self.recent_db[1];
        self.recent_db.rotate_left(1);
        self.recent_db[3] = input_db;
        if input_db - previous >= KNOCK_JUMP_DB {
            self.knock = Some(0);
        } else if let Some(frames) = self.knock {
            let rising = input_db >= previous + KNOCK_RISE_DB;
            let holding = frames >= 2 && input_db > thirty_ms_ago - 1.0;
            self.knock = (!rising && !holding && audible).then_some(frames + 1);
        }
    }

    /// Updates the decaying level meter (halves every 150 ms, like the
    /// mixer's meters).
    fn publish_level(&mut self, peak: f32, frames: usize) {
        let decay = 0.5f32.powf(frames as f32 / (SAMPLE_RATE as f32 * 0.15));
        self.level = peak.max(self.level * decay);
        self.ctrl
            .level
            .store(self.level.to_bits(), Ordering::Relaxed);
    }
}

/// Appends a mono frame to `out` as stereo, with `gain` ramping towards
/// `target`.
fn emit(out: &mut Vec<f32>, gain: &mut f32, frame: &[f32], target: f32) {
    for &s in frame {
        *gain += (target - *gain).clamp(-RELEASE_STEP, ATTACK_STEP);
        out.extend([s * *gain; CHANNELS]);
    }
}

impl Drop for Processor {
    /// A stopped capture (e.g. an unplugged mic) shouldn't leave its last
    /// level frozen on the meter.
    fn drop(&mut self) {
        self.ctrl.level.store(0f32.to_bits(), Ordering::Relaxed);
        self.ctrl
            .input_level
            .store(0f32.to_bits(), Ordering::Relaxed);
        self.ctrl.gate_open.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tone(frames: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (i as f32 * 440.0 / SAMPLE_RATE as f32 * std::f32::consts::TAU).sin() * amp;
                [v, v]
            })
            .collect()
    }

    fn run(p: &mut Processor, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for piece in input.chunks(chunk) {
            p.process(piece, &mut |d| out.extend_from_slice(d));
        }
        out
    }

    #[test]
    fn passes_through_untouched_when_off() {
        let ctrl = Arc::new(ProcessingControl::new(false, false, false, -40.0));
        let mut p = Processor::new(ctrl.clone());
        let input = tone(1000, 0.5);
        assert_eq!(run(&mut p, &input, 314), input);
        assert!(ctrl.level() > 0.45);
    }

    #[test]
    fn gate_silences_quiet_input_and_opens_for_loud() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, false, -30.0));
        let mut p = Processor::new(ctrl.clone());
        // Quiet (-40 dBFS) for 1 s: after the hold and release it is silent.
        let quiet = run(&mut p, &tone(48_000, 0.01), 334);
        assert_eq!(quiet.len(), 48_000 * 2, "whole frames out");
        let tail = &quiet[quiet.len() - 4_800..];
        assert!(tail.iter().all(|s| s.abs() < 1e-6), "gate closed");
        assert!(!ctrl.gate_open());
        // Loud (-6 dBFS): opens within one frame.
        let loud = run(&mut p, &tone(4_800, 0.5), 480);
        assert!(ctrl.gate_open());
        let late = &loud[loud.len() - 960..];
        assert!(late.iter().any(|s| s.abs() > 0.4), "gate open");
    }

    #[test]
    fn switching_off_flushes_buffered_audio() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, false, -100.0));
        let mut p = Processor::new(ctrl.clone());
        let a = run(&mut p, &tone(700, 0.5), 1400); // one frame out, 220 buffered
        assert_eq!(a.len(), FRAME * 2);
        ctrl.set_gate(false);
        let b = run(&mut p, &tone(100, 0.5), 200);
        assert_eq!(a.len() + b.len(), 800 * 2, "nothing lost");
    }

    #[test]
    fn suppression_reduces_steady_noise() {
        let ctrl = Arc::new(ProcessingControl::new(true, false, false, -40.0));
        let mut p = Processor::new(ctrl);
        // Deterministic white-ish noise at about -30 dBFS for 2 s.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..96_000)
            .flat_map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.06;
                [v, v]
            })
            .collect();
        let out = run(&mut p, &noise, 960);
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let half = noise.len() / 2;
        assert!(
            rms(&out[half..]) < rms(&noise[half..]) * 0.5,
            "noise reduced: {} vs {}",
            rms(&out[half..]),
            rms(&noise[half..])
        );
    }
}

#[cfg(test)]
mod auto_gate_tests {
    use super::*;
    use std::sync::Arc;

    /// Synthesised speech: "Hello there, can you hear me?" (with a short
    /// pause at the comma), 1.5 s of silence, "Testing one two three."
    fn speech() -> Vec<f32> {
        let file = include_bytes!("testdata/speech.ogg").to_vec();
        crate::soundboard::decode(file, Some("ogg")).unwrap()
    }

    /// Deterministic white noise, interleaved stereo.
    fn noise(frames: usize, amp: f32) -> Vec<f32> {
        let mut seed = 7u32;
        (0..frames)
            .flat_map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp;
                [v, v]
            })
            .collect()
    }

    /// Whether the gate is open after each 10 ms of `input`.
    fn trace(p: &mut Processor, ctrl: &ProcessingControl, input: &[f32]) -> Vec<bool> {
        input
            .chunks(FRAME * CHANNELS)
            .map(|chunk| {
                p.process(chunk, &mut |_| {});
                ctrl.gate_open()
            })
            .collect()
    }

    /// Lengths of the runs of open gate, ignoring the first `skip` frames.
    fn open_runs(trace: &[bool], skip: usize) -> Vec<usize> {
        let mut runs = Vec::new();
        let mut run = 0;
        for &open in &trace[skip..] {
            if open {
                run += 1;
            } else if run > 0 {
                runs.push(run);
                run = 0;
            }
        }
        if run > 0 {
            runs.push(run);
        }
        runs
    }

    fn with_noise(mut audio: Vec<f32>, amp: f32) -> Vec<f32> {
        let bg = noise(audio.len() / CHANNELS, amp);
        for (a, n) in audio.iter_mut().zip(bg) {
            *a += n;
        }
        audio
    }

    #[test]
    fn auto_gate_opens_for_speech_and_closes_between_phrases() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
        let mut p = Processor::new(ctrl.clone());
        let t = trace(&mut p, &ctrl, &with_noise(speech(), 0.003));
        let runs = open_runs(&t, 0);
        assert_eq!(runs.len(), 3, "one open run per phrase: {runs:?}");
        assert!(runs.iter().all(|&r| r > 50), "{runs:?}");
    }

    #[test]
    fn auto_gate_ignores_loud_steady_noise() {
        // RNNoise's voice detector alone scores loud hiss as likely voice.
        let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
        let mut p = Processor::new(ctrl.clone());
        let t = trace(&mut p, &ctrl, &noise(96_000, 0.3));
        assert!(t.iter().all(|&open| !open), "stays shut");
    }

    #[test]
    fn auto_gate_hears_speech_over_background_hiss() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
        let mut p = Processor::new(ctrl.clone());
        // Settle on the hiss first, as in a real room.
        trace(&mut p, &ctrl, &noise(48_000, 0.05));
        let t = trace(&mut p, &ctrl, &with_noise(speech(), 0.05));
        let runs = open_runs(&t, 0);
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert!(runs.iter().all(|&r| r > 50), "{runs:?}");
    }

    #[test]
    fn auto_gate_without_suppression_keeps_the_input() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
        let mut p = Processor::new(ctrl);
        let input = speech();
        let mut out = Vec::new();
        p.process(&input, &mut |d| out.extend_from_slice(d));
        // Where the gate is fully open, audio is untouched (just delayed by
        // the partial frame, here zero as the input is whole frames).
        let i = (0..out.len())
            .step_by(2)
            .find(|&i| (out[i] - input[i]).abs() < 1e-6 && input[i].abs() > 0.05);
        assert!(i.is_some(), "speech passes through unchanged while open");
    }

    /// A knock or click, 0.5 s into 1 s of quiet room: a glass put down
    /// (ringing partials), a finger snap, a thud on a desk or a clap.
    fn knock(kind: &str) -> Vec<f32> {
        let mut seed = 11u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let sine = |f: f32, t: f32| (std::f32::consts::TAU * f * t).sin();
        (0..48_000)
            .flat_map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32 - 0.5;
                let room = rnd() * 0.004;
                let event = if t < 0.0 {
                    0.0
                } else {
                    match kind {
                        "glass" => {
                            [2400.0, 3800.0, 5100.0, 7300.0]
                                .iter()
                                .map(|&f| sine(f, t))
                                .sum::<f32>()
                                * 0.15
                                * (-t / 0.12).exp()
                        }
                        "snap" => (rnd() * 1.6 + sine(1800.0, t) * 0.3) * (-t / 0.012).exp(),
                        "thud" => {
                            (sine(140.0, t) * 0.6 + rnd() * 0.8 * (-t / 0.004).exp())
                                * (-t / 0.035).exp()
                        }
                        _ => rnd() * 1.4 * (-t / 0.03).exp(), // clap
                    }
                };
                [room + event; 2]
            })
            .collect()
    }

    #[test]
    fn extra_strength_ignores_knocks_and_clicks() {
        // RNNoise's voice detector scores each of these as likely voice for
        // 20 ms (a snap) to the whole ring of a glass.
        for kind in ["glass", "snap", "thud", "clap"] {
            let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
            ctrl.set_extra_strength(true);
            let mut p = Processor::new(ctrl.clone());
            let t = trace(&mut p, &ctrl, &knock(kind));
            assert!(t[30..].iter().all(|&open| !open), "{kind} stays shut");
        }
    }

    #[test]
    fn extra_strength_hears_speech_over_a_fading_knock() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
        ctrl.set_extra_strength(true);
        let mut p = Processor::new(ctrl.clone());
        let mut audio = knock("glass");
        // Start talking 50 ms after the glass is put down.
        let words = speech();
        let start = (0.55 * SAMPLE_RATE as f32) as usize * CHANNELS;
        let skip = 25 * FRAME * CHANNELS; // the speech file's lead-in
        for (a, w) in audio[start..].iter_mut().zip(&words[skip..]) {
            *a += w;
        }
        let t = trace(&mut p, &ctrl, &audio);
        assert!(t[60..].iter().any(|&open| open), "opens for the speech");
    }

    #[test]
    fn extra_strength_does_not_cut_the_start_of_words() {
        let first_open = |extra: bool| {
            let ctrl = Arc::new(ProcessingControl::new(false, true, true, -45.0));
            ctrl.set_extra_strength(extra);
            let mut p = Processor::new(ctrl.clone());
            let t = trace(&mut p, &ctrl, &with_noise(speech(), 0.003));
            // Frames come out 30 ms late with extra strength.
            let delay = if extra { LOOKAHEAD } else { 0 };
            t.iter().position(|&open| open).unwrap() - delay
        };
        assert!(first_open(true) <= first_open(false));
    }

    #[test]
    fn extra_strength_holds_back_30_ms_and_catches_up_when_off() {
        for suppress in [false, true] {
            let ctrl = Arc::new(ProcessingControl::new(suppress, true, true, -45.0));
            ctrl.set_extra_strength(true);
            let mut p = Processor::new(ctrl.clone());
            let input = with_noise(speech(), 0.003);
            let block = FRAME * CHANNELS;
            let frames = input.len() / block;
            let mut out = Vec::new();
            for piece in input.chunks_exact(block).take(frames - 1) {
                p.process(piece, &mut |d| out.extend_from_slice(d));
            }
            let fed = (frames - 1) * block;
            assert_eq!(
                out.len(),
                fed - LOOKAHEAD_FRAMES * CHANNELS,
                "30 ms held back"
            );
            // Switched off (or the gate and suppression off altogether): the
            // held-back audio is dropped, so the mic isn't left behind.
            ctrl.set_extra_strength(false);
            if !suppress {
                ctrl.set_gate(false);
            }
            p.process(&input[fed..fed + block], &mut |d| out.extend_from_slice(d));
            assert_eq!(
                out.len(),
                fed - LOOKAHEAD_FRAMES * CHANNELS + block,
                "caught up"
            );
            assert!(p.ahead.is_empty());
        }
    }
}

#[cfg(test)]
mod auto_level_tests {
    use super::*;
    use std::sync::Arc;

    fn speech(gain: f32) -> Vec<f32> {
        let file = include_bytes!("testdata/speech.ogg").to_vec();
        let mut audio = crate::soundboard::decode(file, Some("ogg")).unwrap();
        for s in &mut audio {
            *s *= gain;
        }
        audio
    }

    fn levelled(start_db: f32) -> (Arc<ProcessingControl>, Processor) {
        let ctrl = Arc::new(ProcessingControl::new(false, false, false, -45.0));
        ctrl.set_auto_level(true);
        ctrl.set_level_db(start_db);
        let p = Processor::new(ctrl.clone());
        (ctrl, p)
    }

    fn run(p: &mut Processor, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in input.chunks(FRAME * CHANNELS) {
            p.process(chunk, &mut |d| out.extend_from_slice(d));
        }
        out
    }

    /// RMS (dBFS) of the loudest tenth of the 10 ms frames: the speech.
    fn speech_db(audio: &[f32]) -> f32 {
        let mut frames: Vec<f32> = audio
            .chunks(FRAME * CHANNELS)
            .map(|f| f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32)
            .collect();
        frames.sort_by(|a, b| b.total_cmp(a));
        let top = &frames[..frames.len() / 10];
        10.0 * (top.iter().sum::<f32>() / top.len() as f32).log10()
    }

    #[test]
    fn quiet_speech_is_turned_up_towards_a_speaking_level() {
        let (ctrl, mut p) = levelled(0.0);
        let quiet = speech(0.1);
        let before = speech_db(&quiet);
        let mut out = Vec::new();
        for _ in 0..4 {
            out = run(&mut p, &quiet);
        }
        let after = speech_db(&out);
        assert!(ctrl.level_db() > 10.0, "turned up {} dB", ctrl.level_db());
        assert!(after - before > 10.0, "{before} → {after} dB");
        // Its peaks stay below the ceiling.
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak <= LEVEL_CEILING + 1e-6, "peak {peak}");
    }

    #[test]
    fn loud_speech_is_turned_down_but_not_past_the_limit() {
        let (ctrl, mut p) = levelled(0.0);
        let loud = speech(2.5);
        for _ in 0..6 {
            run(&mut p, &loud);
        }
        let db = ctrl.level_db();
        assert!((LEVEL_MIN_DB..-3.0).contains(&db), "{db}");
    }

    #[test]
    fn a_sudden_loud_sound_isnt_boosted_past_the_ceiling() {
        let (_, mut p) = levelled(20.0);
        // Quiet, then a loud burst starting just inside a frame, where the
        // gain is still ×10.
        let mut input: Vec<f32> = (0..9_600)
            .flat_map(|i| [(i as f32 * 0.06).sin() * 0.01; CHANNELS])
            .collect();
        let start = 4_803;
        for i in start..start + 2_400 {
            let v = (i as f32 * 0.03).sin() * 0.5;
            input[i * CHANNELS] = v;
            input[i * CHANNELS + 1] = v;
        }
        let out = run(&mut p, &input);
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak <= LEVEL_CEILING + 1e-6, "peak {peak}");
        // And the quiet part before it was boosted as usual.
        let before = out[..4_000 * CHANNELS]
            .iter()
            .fold(0f32, |m, s| m.max(s.abs()));
        assert!(before > 0.09, "{before}");
    }

    #[test]
    fn it_only_moves_while_you_talk() {
        let (ctrl, mut p) = levelled(0.0);
        // Half a phrase of quiet speech: it has started turning up.
        let quiet = speech(0.05);
        run(&mut p, &quiet[..60_000 * CHANNELS]);
        let mid = ctrl.level_db();
        assert!(mid > 0.0, "{mid}");
        // Then silence: it holds where it is.
        run(&mut p, &vec![0.0; 96_000 * CHANNELS]);
        assert_eq!(ctrl.level_db(), mid);
    }

    #[test]
    fn speech_a_manual_gate_mutes_doesnt_set_the_level() {
        let ctrl = Arc::new(ProcessingControl::new(false, true, false, -30.0));
        ctrl.set_auto_level(true);
        let mut p = Processor::new(ctrl.clone());
        // Someone talking quietly in the room, below the gate's line.
        run(&mut p, &speech(0.02));
        assert_eq!(ctrl.level_db(), 0.0, "not learned");
        run(&mut p, &speech(0.5));
        let db = ctrl.level_db();
        assert!(db > 0.0 && db < 15.0, "learned from your voice only: {db}");
    }

    #[test]
    fn silence_and_noise_between_words_dont_change_it() {
        let (ctrl, mut p) = levelled(6.0);
        let mut seed = 5u32;
        let hiss: Vec<f32> = (0..96_000)
            .flat_map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.01;
                [v, v]
            })
            .collect();
        run(&mut p, &hiss);
        assert_eq!(ctrl.level_db(), 6.0, "no speech, no change");
    }

    #[test]
    fn starts_where_it_left_off_and_ramps_back_when_switched_off() {
        let (ctrl, mut p) = levelled(12.0);
        let quiet = speech(0.05);
        let out = run(&mut p, &quiet[..48_000 * CHANNELS]);
        let input = &quiet[..48_000 * CHANNELS];
        let i = (0..out.len()).max_by(|&a, &b| input[a].abs().total_cmp(&input[b].abs()));
        let i = i.unwrap();
        let ratio = out[i] / input[i];
        assert!(ratio > 3.0, "boosted from the start: ×{ratio}");

        ctrl.set_auto_level(false);
        let rest = &quiet[48_000 * CHANNELS..];
        let out = run(&mut p, rest);
        // One frame to ramp back to 1, then untouched.
        assert_eq!(&out[FRAME * CHANNELS..], &rest[FRAME * CHANNELS..]);
    }
}
