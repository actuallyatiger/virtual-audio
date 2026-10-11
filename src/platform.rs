//! Small OS integrations: start-with-Windows and opening URLs/files.

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const VALUE: PCWSTR = w!("VirtualAudioMixer");

    pub fn autostart_enabled() -> bool {
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            ) == ERROR_SUCCESS
        }
    }

    pub fn set_autostart(enabled: bool) -> anyhow::Result<()> {
        unsafe {
            if enabled {
                let exe = std::env::current_exe()?;
                let cmd: Vec<u16> = format!("\"{}\" --background", exe.display())
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    VALUE,
                    REG_SZ.0,
                    Some(cmd.as_ptr() as *const _),
                    (cmd.len() * 2) as u32,
                )
                .ok()?;
            } else if autostart_enabled() {
                RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE).ok()?;
            }
        }
        Ok(())
    }

    pub fn shell_open(target: &str) {
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(target),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn autostart_enabled() -> bool {
        false
    }

    pub fn set_autostart(_enabled: bool) -> anyhow::Result<()> {
        anyhow::bail!("start with Windows is only available on Windows")
    }

    pub fn shell_open(target: &str) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(target).spawn();
    }
}

pub use imp::*;
