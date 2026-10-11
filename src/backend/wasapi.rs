//! WASAPI implementation: shared-mode, event-driven capture/render and
//! per-process loopback capture (Windows 10 2004+ / Windows 11).

use super::*;
use crate::mixer::{CHANNELS, SAMPLE_RATE};
use anyhow::{Context, anyhow, bail};
use std::cell::Cell;
use std::collections::HashMap;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantToStringAlloc};
use windows::Win32::System::Com::*;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    AvSetMmThreadCharacteristicsW, CreateEventW, GetExitCodeProcess, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, SetEvent, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, HSTRING, IUnknown, Interface, PWSTR, implement, w};

/// 20 ms in 100 ns units.
const BUFFER_DURATION: i64 = 200_000;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

fn ensure_com() {
    COM_READY.with(|ready| {
        if !ready.get() {
            // RPC_E_CHANGED_MODE just means the thread already has an apartment.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            ready.set(true);
        }
    });
}

#[derive(Clone, Copy, Debug)]
enum SampleFormat {
    F32,
    I16,
}

fn wave_format(fmt: SampleFormat) -> WAVEFORMATEX {
    let (tag, bits) = match fmt {
        SampleFormat::F32 => (WAVE_FORMAT_IEEE_FLOAT, 32u16),
        SampleFormat::I16 => (WAVE_FORMAT_PCM, 16u16),
    };
    let block_align = CHANNELS as u16 * bits / 8;
    WAVEFORMATEX {
        wFormatTag: tag,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

/// Initializes a client in our canonical format, letting Windows convert
/// sample rate/channels. Falls back to 16-bit PCM if float is rejected.
fn initialize(
    make_client: &dyn Fn() -> anyhow::Result<IAudioClient>,
    extra_flags: u32,
) -> anyhow::Result<(IAudioClient, SampleFormat)> {
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
        | extra_flags;
    let mut last_err = None;
    for fmt in [SampleFormat::F32, SampleFormat::I16] {
        let client = make_client()?;
        let wf = wave_format(fmt);
        match unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                BUFFER_DURATION,
                0,
                &wf,
                None,
            )
        } {
            Ok(()) => {
                tracing::info!("initialized stream as {fmt:?}");
                return Ok((client, fmt));
            }
            Err(e) => {
                tracing::warn!("Initialize as {fmt:?} rejected: {e}");
                last_err = Some(e);
            }
        }
    }
    Err(anyhow!(
        "IAudioClient::Initialize failed: {}",
        last_err.unwrap()
    ))
}

struct Event(HANDLE);

impl Event {
    fn new() -> anyhow::Result<Self> {
        Ok(Self(unsafe { CreateEventW(None, false, false, None)? }))
    }
    /// Returns true if signalled, false on timeout.
    fn wait(&self, ms: u32) -> anyhow::Result<bool> {
        match unsafe { WaitForSingleObject(self.0, ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            other => bail!("WaitForSingleObject failed ({other:?})"),
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn enumerator() -> anyhow::Result<IMMDeviceEnumerator> {
    ensure_com();
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

fn take_pwstr(p: PWSTR) -> anyhow::Result<String> {
    let s = unsafe { p.to_string() };
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    Ok(s?)
}

fn device_id(device: &IMMDevice) -> anyhow::Result<String> {
    take_pwstr(unsafe { device.GetId()? })
}

fn device_name(device: &IMMDevice) -> anyhow::Result<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        // Cleared by PROPVARIANT's Drop.
        let value: PROPVARIANT = store.GetValue(&PKEY_Device_FriendlyName)?;
        PropVariantToStringAlloc(&value)
            .map_err(anyhow::Error::from)
            .and_then(take_pwstr)
    }
}

fn list_devices(flow: EDataFlow) -> anyhow::Result<Vec<DeviceInfo>> {
    let e = enumerator()?;
    unsafe {
        let default_id = e
            .GetDefaultAudioEndpoint(flow, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let coll = e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        let mut out = Vec::new();
        for i in 0..coll.GetCount()? {
            let dev = coll.Item(i)?;
            let id = device_id(&dev)?;
            let name = device_name(&dev).unwrap_or_else(|_| id.clone());
            out.push(DeviceInfo {
                is_default: Some(&id) == default_id.as_ref(),
                id,
                name,
            });
        }
        out.sort_by_key(|d| d.name.to_lowercase());
        Ok(out)
    }
}

fn get_device(flow: EDataFlow, id: Option<&str>) -> anyhow::Result<IMMDevice> {
    let e = enumerator()?;
    let dev = unsafe {
        match id {
            Some(id) => e
                .GetDevice(&HSTRING::from(id))
                .context("device not found")?,
            None => e
                .GetDefaultAudioEndpoint(flow, eConsole)
                .context("no default device")?,
        }
    };
    let state = unsafe { dev.GetState()? };
    if state != DEVICE_STATE_ACTIVE {
        bail!("device is disabled or unplugged");
    }
    Ok(dev)
}

// ---------------------------------------------------------------------------
// Processes

struct Proc {
    pid: u32,
    parent: u32,
    exe: String,
}

fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            out.push(Proc {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
            });
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Walks up the parent chain while the parent runs the same executable, so
/// e.g. a Chrome renderer process maps to the main chrome.exe.
fn root_of(pid: u32, by_pid: &HashMap<u32, &Proc>) -> u32 {
    let mut current = pid;
    for _ in 0..64 {
        let Some(p) = by_pid.get(&current) else { break };
        match by_pid.get(&p.parent) {
            Some(parent) if parent.pid != current && parent.exe.eq_ignore_ascii_case(&p.exe) => {
                current = parent.pid
            }
            _ => break,
        }
    }
    current
}

/// (pid, active) for every non-system audio session on active render devices.
fn session_pids() -> anyhow::Result<Vec<(u32, bool)>> {
    let e = enumerator()?;
    let mut out = Vec::new();
    unsafe {
        let coll = e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..coll.GetCount()? {
            let Ok(dev) = coll.Item(i) else { continue };
            let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = mgr.GetSessionEnumerator() else {
                continue;
            };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(ctl) = sessions.GetSession(j) else {
                    continue;
                };
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                if ctl2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    continue;
                }
                let Ok(pid) = ctl2.GetProcessId() else {
                    continue;
                };
                if pid == 0 {
                    continue;
                }
                let active = ctl
                    .GetState()
                    .map(|s| s == AudioSessionStateActive)
                    .unwrap_or(false);
                out.push((pid, active));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Process loopback activation

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    event: usize,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _op: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        unsafe { SetEvent(HANDLE(self.event as *mut _)) }
    }
}

fn activate_process_loopback(pid: u32) -> anyhow::Result<IAudioClient> {
    ensure_com();
    tracing::info!("activating process loopback for PID {pid}");
    unsafe {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // PROPVARIANT's Drop calls PropVariantClear, which would CoTaskMemFree
        // the blob. It points at `params` on our stack, so never drop it.
        let mut prop = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        {
            let inner = &mut *prop.Anonymous.Anonymous;
            inner.vt = VT_BLOB;
            inner.Anonymous.blob.cbSize = size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
            inner.Anonymous.blob.pBlobData = &mut params as *mut _ as *mut u8;
        }

        let done = Event::new()?;
        let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
            event: done.0.0 as usize,
        }
        .into();
        let op = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*prop),
            &handler,
        )?;
        if !done.wait(5_000)? {
            // The handler may still fire later; keep its event handle valid.
            std::mem::forget(done);
            bail!("timed out activating process loopback");
        }
        tracing::info!("process loopback activation completed");
        let mut hr = HRESULT(0);
        let mut unknown: Option<IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unknown)?;
        hr.ok().context("process loopback activation failed")?;
        Ok(unknown
            .ok_or_else(|| anyhow!("no interface returned"))?
            .cast()?)
    }
}

// ---------------------------------------------------------------------------
// Streams

struct Capture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Event,
    format: SampleFormat,
    buf: Vec<f32>,
    name: String,
}

impl Capture {
    fn start(client: IAudioClient, format: SampleFormat, name: String) -> anyhow::Result<Self> {
        let event = Event::new()?;
        unsafe {
            client.SetEventHandle(event.0)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self {
                client,
                capture,
                event,
                format,
                buf: Vec::new(),
                name,
            })
        }
    }
}

impl CaptureStream for Capture {
    fn read(&mut self, sink: &mut dyn FnMut(&[f32])) -> anyhow::Result<()> {
        // Loopback streams may not signal at all while the app is silent.
        self.event.wait(100)?;
        unsafe {
            loop {
                if self.capture.GetNextPacketSize()? == 0 {
                    return Ok(());
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let samples = frames as usize * CHANNELS;
                self.buf.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    self.buf.resize(samples, 0.0);
                } else {
                    match self.format {
                        SampleFormat::F32 => self.buf.extend_from_slice(
                            std::slice::from_raw_parts(data as *const f32, samples),
                        ),
                        SampleFormat::I16 => self.buf.extend(
                            std::slice::from_raw_parts(data as *const i16, samples)
                                .iter()
                                .map(|&s| s as f32 / 32768.0),
                        ),
                    }
                }
                self.capture.ReleaseBuffer(frames)?;
                sink(&self.buf);
            }
        }
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

struct Render {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: Event,
    format: SampleFormat,
    buffer_frames: u32,
    scratch: Vec<f32>,
    timeouts: u32,
}

impl RenderStream for Render {
    fn render(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> anyhow::Result<()> {
        if !self.event.wait(200)? {
            self.timeouts += 1;
            if self.timeouts > 10 {
                bail!("output device stopped responding");
            }
            return Ok(());
        }
        self.timeouts = 0;
        unsafe {
            let padding = self.client.GetCurrentPadding()?;
            let frames = self.buffer_frames.saturating_sub(padding);
            if frames == 0 {
                return Ok(());
            }
            let data = self.render.GetBuffer(frames)?;
            let samples = frames as usize * CHANNELS;
            match self.format {
                SampleFormat::F32 => {
                    fill(std::slice::from_raw_parts_mut(data as *mut f32, samples))
                }
                SampleFormat::I16 => {
                    self.scratch.resize(samples, 0.0);
                    fill(&mut self.scratch);
                    let out = std::slice::from_raw_parts_mut(data as *mut i16, samples);
                    for (o, s) in out.iter_mut().zip(&self.scratch) {
                        *o = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                }
            }
            self.render.ReleaseBuffer(frames, 0)?;
        }
        Ok(())
    }
}

impl Drop for Render {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

// ---------------------------------------------------------------------------

pub struct Wasapi;

impl Wasapi {
    pub fn new() -> Self {
        Wasapi
    }
}

impl Backend for Wasapi {
    fn input_devices(&self) -> anyhow::Result<Vec<DeviceInfo>> {
        list_devices(eCapture)
    }

    fn output_devices(&self) -> anyhow::Result<Vec<DeviceInfo>> {
        list_devices(eRender)
    }

    fn audio_apps(&self) -> anyhow::Result<Vec<AppInfo>> {
        let procs = processes();
        let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
        let me = std::process::id();
        let mut apps: Vec<AppInfo> = Vec::new();
        for (pid, active) in session_pids()? {
            let root = root_of(pid, &by_pid);
            if root == me {
                continue;
            }
            let Some(proc_) = by_pid.get(&root) else {
                continue;
            };
            match apps
                .iter_mut()
                .find(|a| a.exe.eq_ignore_ascii_case(&proc_.exe))
            {
                Some(existing) => existing.active |= active,
                None => apps.push(AppInfo {
                    exe: proc_.exe.clone(),
                    pid: root,
                    active,
                }),
            }
        }
        apps.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then_with(|| a.exe.to_lowercase().cmp(&b.exe.to_lowercase()))
        });
        Ok(apps)
    }

    fn find_process(&self, exe: &str) -> Option<u32> {
        let procs = processes();
        let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
        // Prefer the instance that actually has an audio session.
        if let Ok(sessions) = session_pids() {
            for (pid, _) in sessions {
                let root = root_of(pid, &by_pid);
                if by_pid
                    .get(&root)
                    .is_some_and(|p| p.exe.eq_ignore_ascii_case(exe))
                {
                    return Some(root);
                }
            }
        }
        procs
            .iter()
            .filter(|p| p.exe.eq_ignore_ascii_case(exe))
            .map(|p| root_of(p.pid, &by_pid))
            .next()
    }

    fn process_alive(&self, pid: u32) -> bool {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut code = 0u32;
            let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
            let _ = CloseHandle(h);
            alive
        }
    }

    fn open_capture(&self, target: &CaptureTarget) -> anyhow::Result<Box<dyn CaptureStream>> {
        ensure_com();
        match target {
            CaptureTarget::Device(id) => {
                let dev = get_device(eCapture, id.as_deref())?;
                let name = device_name(&dev).unwrap_or_default();
                tracing::info!("opening capture device {name:?}");
                let make = || -> anyhow::Result<IAudioClient> {
                    Ok(unsafe { dev.Activate(CLSCTX_ALL, None)? })
                };
                let (client, fmt) = initialize(&make, 0)?;
                Ok(Box::new(Capture::start(client, fmt, name)?))
            }
            CaptureTarget::Process(pid) => {
                let make = || activate_process_loopback(*pid);
                let (client, fmt) = initialize(&make, AUDCLNT_STREAMFLAGS_LOOPBACK)?;
                Ok(Box::new(Capture::start(client, fmt, format!("PID {pid}"))?))
            }
        }
    }

    fn open_render(&self, device_id: &str) -> anyhow::Result<Box<dyn RenderStream>> {
        ensure_com();
        let dev = get_device(eRender, Some(device_id))?;
        tracing::info!(
            "opening output device {:?}",
            device_name(&dev).unwrap_or_default()
        );
        let make =
            || -> anyhow::Result<IAudioClient> { Ok(unsafe { dev.Activate(CLSCTX_ALL, None)? }) };
        let (client, format) = initialize(&make, 0)?;
        let event = Event::new()?;
        unsafe {
            client.SetEventHandle(event.0)?;
            let buffer_frames = client.GetBufferSize()?;
            let render: IAudioRenderClient = client.GetService()?;
            // Pre-roll silence so the first period does not glitch.
            render.GetBuffer(buffer_frames)?;
            render.ReleaseBuffer(buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
            client.Start()?;
            Ok(Box::new(Render {
                client,
                render,
                event,
                format,
                buffer_frames,
                scratch: Vec::new(),
                timeouts: 0,
            }))
        }
    }

    fn promote_audio_thread(&self) {
        let mut index = 0u32;
        unsafe {
            let _ = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index);
        }
    }
}
