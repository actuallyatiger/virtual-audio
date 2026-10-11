//! Fake backend used on non-Windows hosts so the engine and web UI can be
//! developed and tested anywhere. Sources generate tones.

use super::*;
use crate::mixer::SAMPLE_RATE;
use std::time::{Duration, Instant};

pub struct Dummy;

impl Dummy {
    pub fn new() -> Self {
        Dummy
    }
}

const APPS: &[(&str, u32, f32)] = &[("chrome.exe", 1000, 440.0), ("soundboard.exe", 2000, 660.0)];

impl Backend for Dummy {
    fn input_devices(&self) -> anyhow::Result<Vec<DeviceInfo>> {
        Ok(vec![
            DeviceInfo {
                id: "dummy-mic".into(),
                name: "Microphone (Dummy USB Mic)".into(),
                is_default: true,
            },
            DeviceInfo {
                id: "dummy-line".into(),
                name: "Line In (Dummy)".into(),
                is_default: false,
            },
            DeviceInfo {
                id: "dummy-cable-out".into(),
                name: "CABLE Output (VB-Audio Virtual Cable)".into(),
                is_default: false,
            },
        ])
    }

    /// The first input, or the one whose id is in `<data dir>/dummy-default-input`
    /// (lets you simulate changing the Windows default).
    fn default_input(&self) -> Option<DeviceInfo> {
        let wanted =
            std::fs::read_to_string(crate::config::data_dir().join("dummy-default-input")).ok();
        let inputs = self.input_devices().ok()?;
        let pick = wanted
            .and_then(|id| inputs.iter().position(|d| d.id == id.trim()))
            .unwrap_or(0);
        inputs.into_iter().nth(pick)
    }

    fn output_devices(&self) -> anyhow::Result<Vec<DeviceInfo>> {
        Ok(vec![
            DeviceInfo {
                id: "dummy-speakers".into(),
                name: "Speakers (Dummy)".into(),
                is_default: true,
            },
            DeviceInfo {
                id: "dummy-cable".into(),
                name: "CABLE Input (VB-Audio Virtual Cable)".into(),
                is_default: false,
            },
        ])
    }

    fn audio_apps(&self) -> anyhow::Result<Vec<AppInfo>> {
        Ok(APPS
            .iter()
            .map(|&(exe, pid, _)| AppInfo {
                exe: exe.into(),
                pid,
                active: true,
            })
            .collect())
    }

    fn find_process(&self, exe: &str) -> Option<u32> {
        APPS.iter()
            .find(|a| a.0.eq_ignore_ascii_case(exe))
            .map(|a| a.1)
    }

    fn process_alive(&self, pid: u32) -> bool {
        APPS.iter().any(|a| a.1 == pid)
    }

    fn open_capture(&self, target: &CaptureTarget) -> anyhow::Result<Box<dyn CaptureStream>> {
        let (freq, name, bursty) = match target {
            CaptureTarget::Device(id) => (
                220.0,
                id.clone().unwrap_or_else(|| "dummy-mic".into()),
                false,
            ),
            CaptureTarget::Process(pid) => {
                let app = APPS
                    .iter()
                    .find(|a| a.1 == *pid)
                    .ok_or_else(|| anyhow::anyhow!("no such process"))?;
                (app.2, app.0.to_string(), true)
            }
        };
        Ok(Box::new(Tone {
            freq,
            phase: 0.0,
            n: 0,
            next: Instant::now(),
            name,
            bursty,
        }))
    }

    fn open_render(&self, device_id: &str) -> anyhow::Result<Box<dyn RenderStream>> {
        anyhow::ensure!(device_id.starts_with("dummy-"), "device not found");
        Ok(Box::new(NullRender::default()))
    }
}

struct Tone {
    freq: f32,
    phase: f32,
    n: u64,
    next: Instant,
    name: String,
    bursty: bool,
}

impl CaptureStream for Tone {
    fn read(&mut self, sink: &mut dyn FnMut(&[f32])) -> anyhow::Result<()> {
        let now = Instant::now();
        if self.next > now {
            std::thread::sleep(self.next - now);
        }
        self.next += Duration::from_millis(10);
        let mut buf = [0f32; 960];
        for frame in buf.as_chunks_mut::<2>().0 {
            self.n += 1;
            let on = !self.bursty || (self.n / (SAMPLE_RATE as u64 / 2)).is_multiple_of(3);
            let v = if on {
                (self.phase * std::f32::consts::TAU).sin() * 0.3
            } else {
                0.0
            };
            self.phase = (self.phase + self.freq / SAMPLE_RATE as f32).fract();
            frame[0] = v;
            frame[1] = v;
        }
        sink(&buf);
        Ok(())
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}
