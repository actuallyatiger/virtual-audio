//! Small OS integrations: start-with-Windows (optionally as administrator),
//! elevation, and opening URLs/files.
//!
//! Starting as administrator matters for hotkeys: while an elevated app (some
//! games) is focused, Windows hides its keys from apps that aren't elevated.
//! A `Run` registry entry can't start an app elevated without a UAC prompt at
//! every sign-in, so that mode registers a Task Scheduler task with "highest
//! privileges" instead. Registering it needs elevation once, so a non-elevated
//! mixer re-launches itself elevated (`--admin-autostart on|off --user …`) to
//! do it, which shows one UAC prompt. The task starts a copy of the mixer in
//! Program Files, never a file the user's own programs could replace.

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
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_CANCELLED, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::Security::{
        GetTokenInformation, LookupAccountSidW, SID_NAME_USE, TOKEN_ELEVATION, TOKEN_QUERY,
        TOKEN_USER, TokenElevation, TokenUser,
    };
    use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    };
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        FOLDERID_ProgramFiles, FOLDERID_System, FOLDERID_Windows, KF_FLAG_DEFAULT,
        SEE_MASK_NO_CONSOLE, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        SHGetKnownFolderPath, ShellExecuteExW, ShellExecuteW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    use windows::core::{GUID, HSTRING, PCWSTR, PWSTR, w};

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
        let target = target.to_owned();
        // The shell wants a single-threaded COM apartment; this caller may
        // have none or a multithreaded one.
        let _ = std::thread::spawn(move || unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            if is_elevated() {
                if let Ok(windows) = known_folder(&FOLDERID_Windows) {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        &HSTRING::from(windows.join("explorer.exe").as_os_str()),
                        &HSTRING::from(format!("\"{target}\"")),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    );
                }
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
            if com.is_ok() {
                CoUninitialize();
            }
        })
        .join();
    }

    /// A known folder such as Program Files or System32.
    fn known_folder(id: &GUID) -> anyhow::Result<PathBuf> {
        unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
            let path = p.to_string();
            CoTaskMemFree(Some(p.0 as *const _));
            Ok(PathBuf::from(path?))
        }
    }

    /// The process token, closed when dropped.
    struct Token(HANDLE);

    impl Token {
        fn open() -> Option<Self> {
            let mut token = HANDLE::default();
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
            Some(Self(token))
        }
    }

    impl Drop for Token {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Whether this process runs as administrator.
    pub fn is_elevated() -> bool {
        let Some(token) = Token::open() else {
            return false;
        };
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0;
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                Some(&mut elevation as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
        }
        .is_ok();
        ok && elevation.TokenIsElevated != 0
    }

    /// The account this process runs as, as DOMAIN\name.
    fn current_user() -> anyhow::Result<String> {
        let token = Token::open().context("opening the process token")?;
        unsafe {
            let mut len = 0;
            let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut len);
            // u64s keep the buffer aligned for TOKEN_USER.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buf.as_mut_ptr() as *mut _),
                len,
                &mut len,
            )
            .context("reading the account")?;
            let sid = (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid;
            let (mut name, mut domain) = ([0u16; 256], [0u16; 256]);
            let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
            let mut kind = SID_NAME_USE::default();
            LookupAccountSidW(
                PCWSTR::null(),
                sid,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                Some(PWSTR(domain.as_mut_ptr())),
                &mut domain_len,
                &mut kind,
            )
            .context("looking up the account")?;
            let name = String::from_utf16_lossy(&name[..name_len as usize]);
            let domain = String::from_utf16_lossy(&domain[..domain_len as usize]);
            Ok(if domain.is_empty() {
                name
            } else {
                format!("{domain}\\{name}")
            })
        }
    }

    /// Where the elevated mixer is installed: a folder in Program Files,
    /// which only administrators can change. The task must not start a
    /// file the user (or anything running as them) can replace, such as
    /// a download, or it would run that as administrator.
    fn install_dir() -> anyhow::Result<PathBuf> {
        Ok(known_folder(&FOLDERID_ProgramFiles)?.join("Virtual Audio Mixer"))
    }

    /// Only one change to the task at a time.
    static CHANGING: Mutex<()> = Mutex::new(());

    /// Registers or removes the elevated sign-in task, asking for
    /// administrator permission (UAC) if this process isn't elevated.
    /// Blocks until the user answers the prompt.
    pub fn set_admin_autostart(enabled: bool) -> anyhow::Result<()> {
        let _changing = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
        let user = current_user()?;
        if is_elevated() {
            return admin_task(enabled, &user);
        }
        let exe = std::env::current_exe()?;
        let args = format!(
            "--admin-autostart {} --user \"{user}\"",
            if enabled { "on" } else { "off" }
        );
        let error_file = install_dir()?.join(ERROR_FILE);
        // ShellExecuteEx wants a single-threaded COM apartment, which this
        // (pool) thread may not have, so ask from a thread of our own.
        let code = std::thread::spawn(move || -> anyhow::Result<Option<u32>> {
            unsafe {
                let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
                let (file, params) = (HSTRING::from(exe.as_os_str()), HSTRING::from(args));
                let mut info = SHELLEXECUTEINFOW {
                    cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                    fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE | SEE_MASK_NOASYNC,
                    lpVerb: w!("runas"),
                    lpFile: PCWSTR(file.as_ptr()),
                    lpParameters: PCWSTR(params.as_ptr()),
                    nShow: SW_HIDE.0,
                    ..Default::default()
                };
                let launched = ShellExecuteExW(&mut info);
                if com.is_ok() {
                    CoUninitialize();
                }
                if let Err(e) = launched {
                    if e.code() == ERROR_CANCELLED.to_hresult() {
                        bail!("administrator permission was declined");
                    }
                    return Err(e).context("asking for administrator permission");
                }
                // The helper only copies a file and runs schtasks.
                let waited = WaitForSingleObject(info.hProcess, 120_000);
                let mut code = 1;
                let _ = GetExitCodeProcess(info.hProcess, &mut code);
                let _ = CloseHandle(info.hProcess);
                Ok((waited == WAIT_OBJECT_0).then_some(code))
            }
        })
        .join()
        .map_err(|_| anyhow::anyhow!("asking for administrator permission failed"))??;
        let action = if enabled { "create" } else { "remove" };
        match code {
            None => bail!("the sign-in task is taking too long to {action}; check again later"),
            Some(0) => Ok(()),
            Some(code) => {
                let why = std::fs::read_to_string(&error_file).unwrap_or_default();
                bail!(
                    "couldn't {action} the sign-in task: {}",
                    if why.is_empty() {
                        format!("exit code {code}")
                    } else {
                        why
                    }
                )
            }
        }
    }

    /// Where the elevated helper leaves its error for the mixer to show, in
    /// the install folder so the helper never writes where the user can
    /// redirect it.
    const ERROR_FILE: &str = "admin-autostart-error.txt";

    /// The elevated helper's work (`--admin-autostart on|off --user …`).
    pub fn admin_task_helper(enabled: bool, user: &str) -> anyhow::Result<()> {
        let dir = install_dir()?;
        let _ = std::fs::remove_file(dir.join(ERROR_FILE));
        let result = (|| {
            // Whoever answered the prompt: an administrator's password typed
            // in for a standard account runs this as that administrator,
            // and a task for the standard account couldn't start elevated.
            let me = current_user()?;
            if !me.eq_ignore_ascii_case(user) {
                bail!(
                    "Windows gave permission as {me}, not {user}. Starting as administrator needs your own account to be an administrator."
                );
            }
            admin_task(enabled, user)
        })();
        if let Err(e) = &result {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join(ERROR_FILE), format!("{e:#}"));
        }
        result
    }

    fn schtasks(args: &[&std::ffi::OsStr]) -> anyhow::Result<std::process::Output> {
        let exe = known_folder(&FOLDERID_System)?.join("schtasks.exe");
        std::process::Command::new(exe)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("running schtasks.exe")
    }

    /// schtasks' error message.
    fn failure(out: &std::process::Output) -> anyhow::Error {
        anyhow::anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim())
    }

    /// Copies this exe into `dir` (unless it is that copy) and returns the
    /// copy's path. A running copy can't be overwritten, but it can be
    /// renamed out of the way.
    fn install(dir: &Path) -> anyhow::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let dest = dir.join("virtual-audio.exe");
        if dest.exists() {
            if std::fs::canonicalize(&exe)? == std::fs::canonicalize(&dest)? {
                return Ok(dest);
            }
            let old = dir.join("virtual-audio.old.exe");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&dest, &old)
                .context("replacing the installed copy (quit other copies of the mixer, or restart, and try again)")?;
        }
        std::fs::copy(&exe, &dest).context("copying the mixer to Program Files")?;
        Ok(dest)
    }

    /// Deletes a file now, or at the next restart if it's running.
    fn remove(path: &Path) {
        if path.exists() && std::fs::remove_file(path).is_err() {
            let _ = unsafe {
                MoveFileExW(
                    &HSTRING::from(path.as_os_str()),
                    PCWSTR::null(),
                    MOVEFILE_DELAY_UNTIL_REBOOT,
                )
            };
        }
    }

    /// Creates or deletes the task and the installed copy it starts. Needs
    /// to run elevated.
    fn admin_task(enabled: bool, user: &str) -> anyhow::Result<()> {
        let name = std::ffi::OsStr::new(super::TASK_NAME);
        let dir = install_dir()?;
        if !enabled {
            let out = schtasks(&["/Delete".as_ref(), "/TN".as_ref(), name, "/F".as_ref()])?;
            // Failing is fine only if there was no task to delete.
            if !out.status.success()
                && schtasks(&["/Query".as_ref(), "/TN".as_ref(), name])?
                    .status
                    .success()
            {
                return Err(failure(&out));
            }
            for file in ["virtual-audio.exe", "virtual-audio.old.exe"] {
                remove(&dir.join(file));
            }
            return Ok(());
        }
        std::fs::create_dir_all(&dir).context("creating the Program Files folder")?;
        let exe = install(&dir)?;
        let xml = super::admin_task_xml(&exe.display().to_string(), user);
        // schtasks wants the definition as UTF-16 with a byte order mark. It
        // goes in the install folder, which only administrators can change.
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        let path = dir.join("task.xml");
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
            return Err(failure(&out));
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
