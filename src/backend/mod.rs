//! Audio I/O abstraction. All streams use interleaved stereo f32 at
//! [`crate::mixer::SAMPLE_RATE`]; backends convert as needed.

use serde::Serialize;

#[cfg(not(windows))]
mod dummy;
#[cfg(windows)]
mod wasapi;

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub exe: String,
    pub pid: u32,
    /// Whether the app's audio session is currently playing.
    pub active: bool,
}

#[derive(Debug, Clone)]
pub enum CaptureTarget {
    /// `None` = current default capture device.
    Device(Option<String>),
    /// Process loopback including the process' children.
    Process(u32),
    /// Everything playing on an output device (loopback). `None` = current
    /// default output.
    Output(Option<String>),
}

pub trait CaptureStream {
    /// Waits (briefly) for audio and hands every available packet to `sink`.
    /// Returns `Ok` on timeout so callers can check their stop flag.
    fn read(&mut self, sink: &mut dyn FnMut(&[f32])) -> anyhow::Result<()>;
    /// Short human readable description (e.g. resolved device name).
    fn describe(&self) -> String;
}

pub trait RenderStream {
    /// Waits until the device wants audio, then lets `fill` write it.
    fn render(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> anyhow::Result<()>;
}

pub trait Backend: Send + Sync + 'static {
    fn input_devices(&self) -> anyhow::Result<Vec<DeviceInfo>>;
    fn output_devices(&self) -> anyhow::Result<Vec<DeviceInfo>>;
    /// The current Windows default capture device. Cheap enough to poll.
    fn default_input(&self) -> Option<DeviceInfo>;
    /// The current Windows default playback device.
    fn default_output(&self) -> Option<DeviceInfo>;
    /// Processes that currently own an audio session, collapsed to the
    /// top-most process of each executable.
    fn audio_apps(&self) -> anyhow::Result<Vec<AppInfo>>;
    /// Finds the root process for an executable name (case-insensitive).
    fn find_process(&self, exe: &str) -> Option<u32>;
    fn process_alive(&self, pid: u32) -> bool;
    /// Must be called on the thread that will use the stream.
    fn open_capture(&self, target: &CaptureTarget) -> anyhow::Result<Box<dyn CaptureStream>>;
    /// Must be called on the thread that will use the stream.
    fn open_render(&self, device_id: &str) -> anyhow::Result<Box<dyn RenderStream>>;
    /// Raises the calling thread's scheduling priority for audio work.
    fn promote_audio_thread(&self) {}
}

#[cfg(windows)]
pub fn create() -> Box<dyn Backend> {
    Box::new(wasapi::Wasapi::new())
}

#[cfg(not(windows))]
pub fn create() -> Box<dyn Backend> {
    Box::new(dummy::Dummy::new())
}

pub const PLATFORM_SUPPORTED: bool = cfg!(windows);

/// Renders into nothing on a fixed 10 ms clock. Used while no output device
/// is selected (or it failed) so meters keep working.
pub struct NullRender {
    next: std::time::Instant,
    buf: Vec<f32>,
}

impl Default for NullRender {
    fn default() -> Self {
        Self {
            next: std::time::Instant::now(),
            buf: vec![0.0; 480 * 2],
        }
    }
}

impl RenderStream for NullRender {
    fn render(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> anyhow::Result<()> {
        let now = std::time::Instant::now();
        if self.next > now {
            std::thread::sleep(self.next - now);
        } else if now - self.next > std::time::Duration::from_millis(100) {
            self.next = now;
        }
        self.next += std::time::Duration::from_millis(10);
        fill(&mut self.buf);
        Ok(())
    }
}
