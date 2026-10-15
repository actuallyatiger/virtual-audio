//! Audio from a Bluetooth device such as a phone. Windows (10 version 2004
//! and later) can be a Bluetooth speaker for a paired phone, through
//! `AudioPlaybackConnection`, and plays what the phone sends itself.
//!
//! The connection is opened by a helper: this exe started with
//! `--bluetooth-host <device id>`, which prints `connected` (or `error …`)
//! and keeps the connection open until its input closes, that is until the
//! mixer stops the source or quits. The mixer then captures the helper's
//! audio as it does an app's (process loopback), so it gets the phone and
//! nothing else, not even its own output. What Windows does with that audio
//! is logged (`backend/wasapi.rs`), as it can only be checked on a real PC.

/// The flag that starts the helper.
pub const HOST_FLAG: &str = "--bluetooth-host";

/// The helper: connects to the device `id` and holds the connection until
/// its input closes or the device goes away.
#[cfg(windows)]
pub fn host(id: &str) -> anyhow::Result<()> {
    use anyhow::bail;
    use std::io::Write;
    use windows::Media::Audio::{
        AudioPlaybackConnection, AudioPlaybackConnectionOpenResultStatus,
        AudioPlaybackConnectionState,
    };
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::core::HSTRING;

    let connect = || -> anyhow::Result<AudioPlaybackConnection> {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let connection = AudioPlaybackConnection::TryCreateFromId(&HSTRING::from(id))
            // (Windows answers "nothing", not an error, for an unknown id.)
            .map_err(|e| match e.code().is_ok() {
                true => anyhow::anyhow!(
                    "it isn't paired any more. Pair it again in Windows' Bluetooth settings"
                ),
                false => e.into(),
            })?;
        connection.Start()?;
        let opened = connection.Open()?;
        match opened.Status()? {
            AudioPlaybackConnectionOpenResultStatus::Success => Ok(connection),
            AudioPlaybackConnectionOpenResultStatus::RequestTimedOut => {
                bail!("it didn't answer. Check it's nearby with Bluetooth on")
            }
            AudioPlaybackConnectionOpenResultStatus::DeniedBySystem => {
                bail!("Windows refused the connection")
            }
            _ => bail!("couldn't connect ({})", opened.ExtendedError()?.message()),
        }
    };
    let mut out = std::io::stdout();
    let connection = match connect() {
        Ok(c) => c,
        Err(e) => {
            writeln!(out, "error {e:#}")?;
            return Ok(());
        }
    };
    writeln!(out, "connected")?;
    out.flush()?;
    // The mixer closes our input to stop us (or it quits, which closes it
    // too). Meanwhile, quit if the device goes away, so it reconnects.
    let (done, closed) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        let _ = done.send(());
    });
    while closed
        .recv_timeout(std::time::Duration::from_secs(1))
        .is_err_and(|e| e == std::sync::mpsc::RecvTimeoutError::Timeout)
    {
        if connection.State()? == AudioPlaybackConnectionState::Closed {
            break;
        }
    }
    let _ = connection.Close();
    Ok(())
}

#[cfg(not(windows))]
pub fn host(_id: &str) -> anyhow::Result<()> {
    anyhow::bail!("Bluetooth audio is only available on Windows")
}
