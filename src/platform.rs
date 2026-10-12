//! Small OS integrations: start-with-Windows (optionally as administrator),
//! elevation, and opening URLs/files.
//!
//! Starting as administrator matters for hotkeys: while an elevated app (some
//! games) is focused, Windows hides its keys from apps that aren't elevated.
//! A `Run` registry entry can't start an app elevated without a UAC prompt at
//! every sign-in, so that mode registers a Task Scheduler task with "highest
//! privileges" instead. Registering it needs elevation once, so a non-elevated
//! mixer re-launches itself elevated (`--admin-autostart on|off --user …`) to
//! do it, which shows one UAC prompt.

/// Name of the scheduled task.
#[cfg_attr(not(windows), allow(dead_code))]
pub const TASK_NAME: &str = "VirtualAudioMixer";

/// The scheduled task that starts the mixer elevated when `user` signs in.
/// Settings that matter, each a Task Scheduler default we must override:
/// no 3-day time limit, runs on battery, and normal (not below-normal)
/// priority for the audio.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn admin_task_xml(exe: &str, user: &str) -> String {
    let (exe, user) = (xml_escape(exe), xml_escape(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Virtual Audio Mixer in the background, as administrator, when you sign in, so its hotkeys also work in apps running as administrator.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>--background</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

#[cfg_attr(not(windows), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(windows)]
mod imp {
    use anyhow::{Context, bail};
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_SUCCESS, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        SEE_MASK_NO_CONSOLE, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        ShellExecuteW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    use windows::core::{HSTRING, PCWSTR, w};

    /// Hides the console window schtasks.exe would otherwise flash.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

    /// Opens a URL or file with its default app. An elevated mixer hands
    /// it to Explorer, so the browser doesn't start elevated too.
    pub fn shell_open(target: &str) {
        unsafe {
            if is_elevated() {
                ShellExecuteW(
                    None,
                    w!("open"),
                    w!("explorer.exe"),
                    &HSTRING::from(format!("\"{target}\"")),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            } else {
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

    /// Whether this process runs as administrator.
    pub fn is_elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut len = 0;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// The signed-in user, as DOMAIN\name.
    fn current_user() -> String {
        let domain = std::env::var("USERDOMAIN").unwrap_or_default();
        let name = std::env::var("USERNAME").unwrap_or_default();
        if domain.is_empty() {
            name
        } else {
            format!("{domain}\\{name}")
        }
    }

    /// Registers or removes the elevated sign-in task, asking for
    /// administrator permission (UAC) if this process isn't elevated.
    /// Blocks until the user answers the prompt.
    pub fn set_admin_autostart(enabled: bool) -> anyhow::Result<()> {
        let user = current_user();
        if is_elevated() {
            return admin_task(enabled, &user);
        }
        let exe = std::env::current_exe()?;
        let args = format!(
            "--admin-autostart {} --user \"{user}\"",
            if enabled { "on" } else { "off" }
        );
        let error_file = error_file();
        let _ = std::fs::remove_file(&error_file);
        unsafe {
            let (file, params) = (HSTRING::from(exe.as_os_str()), HSTRING::from(args));
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE,
                lpVerb: w!("runas"),
                lpFile: PCWSTR(file.as_ptr()),
                lpParameters: PCWSTR(params.as_ptr()),
                nShow: SW_HIDE.0,
                ..Default::default()
            };
            if let Err(e) = ShellExecuteExW(&mut info) {
                if e.code() == ERROR_CANCELLED.to_hresult() {
                    bail!("administrator permission was declined");
                }
                return Err(e).context("asking for administrator permission");
            }
            // The helper only runs schtasks; give up after 2 minutes.
            WaitForSingleObject(info.hProcess, 120_000);
            let mut code = 1;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            if code != 0 {
                let why = std::fs::read_to_string(&error_file).unwrap_or_default();
                let _ = std::fs::remove_file(&error_file);
                bail!(
                    "couldn't {} the sign-in task: {}",
                    if enabled { "create" } else { "remove" },
                    if why.is_empty() {
                        format!("exit code {code}")
                    } else {
                        why
                    }
                );
            }
        }
        Ok(())
    }

    /// Where the elevated helper leaves its error for the mixer to show.
    fn error_file() -> std::path::PathBuf {
        crate::config::data_dir().join("admin-autostart-error.txt")
    }

    /// The elevated helper's work (`--admin-autostart on|off --user …`).
    pub fn admin_task_helper(enabled: bool, user: &str) -> anyhow::Result<()> {
        let result = admin_task(enabled, user);
        if let Err(e) = &result {
            let _ = std::fs::create_dir_all(crate::config::data_dir());
            let _ = std::fs::write(error_file(), format!("{e:#}"));
        }
        result
    }

    fn schtasks(args: &[&std::ffi::OsStr]) -> anyhow::Result<std::process::Output> {
        std::process::Command::new("schtasks.exe")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("running schtasks.exe")
    }

    /// Creates or deletes the task. Needs to run elevated.
    fn admin_task(enabled: bool, user: &str) -> anyhow::Result<()> {
        let name = std::ffi::OsStr::new(super::TASK_NAME);
        if !enabled {
            let exists = schtasks(&["/Query".as_ref(), "/TN".as_ref(), name])?
                .status
                .success();
            if exists {
                let out = schtasks(&["/Delete".as_ref(), "/TN".as_ref(), name, "/F".as_ref()])?;
                if !out.status.success() {
                    bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
                }
            }
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        let xml = super::admin_task_xml(&exe.display().to_string(), user);
        // schtasks wants the definition as UTF-16 with a byte order mark.
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        let path = std::env::temp_dir().join("virtual-audio-mixer-task.xml");
        std::fs::write(&path, bytes).context("writing the task definition")?;
        let out = schtasks(&[
            "/Create".as_ref(),
            "/TN".as_ref(),
            name,
            "/XML".as_ref(),
            path.as_os_str(),
            "/F".as_ref(),
        ]);
        let _ = std::fs::remove_file(&path);
        let out = out?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
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

    pub fn is_elevated() -> bool {
        false
    }

    pub fn set_admin_autostart(_enabled: bool) -> anyhow::Result<()> {
        anyhow::bail!("start with Windows is only available on Windows")
    }

    pub fn admin_task_helper(_enabled: bool, _user: &str) -> anyhow::Result<()> {
        anyhow::bail!("only available on Windows")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_task_runs_elevated_at_sign_in_without_limits() {
        let xml = admin_task_xml(r"C:\Apps\Mixer & Co\virtual-audio.exe", r"PC\Tiger");
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains(r"<UserId>PC\Tiger</UserId>"));
        assert!(
            xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"),
            "no 3-day limit"
        );
        assert!(
            xml.contains("<DisallowStartIfOnBatteries>false"),
            "laptops on battery"
        );
        assert!(xml.contains("<Priority>4</Priority>"), "not below normal");
        assert!(xml.contains(r"<Command>C:\Apps\Mixer &amp; Co\virtual-audio.exe</Command>"));
        assert!(xml.contains("<Arguments>--background</Arguments>"));
    }
}
