//! Visible current-user setup and maintenance for a self-contained bundle.
#[cfg(windows)]
#[path = "../packaging/user_setup.rs"]
mod user_setup;

fn main() {
    #[cfg(windows)]
    {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let no_dialog = arguments.iter().any(|argument| argument == "--no-dialog");
        let actions: Vec<_> = arguments
            .iter()
            .filter(|argument| argument.as_str() != "--no-dialog")
            .collect();
        let action = actions
            .first()
            .map_or("--install", |argument| argument.as_str());
        let quiet = no_dialog || action == "--login" || action == "--verify";
        let result = if actions.len() > 1 {
            Err("Specify one SmartKey action, optionally followed by --no-dialog.".into())
        } else {
            run(action)
        };
        match result {
            Ok(message) => {
                println!("{message}");
                if !quiet {
                    show(&message, false);
                }
            }
            Err(message) => {
                eprintln!("SmartKey: {message}");
                // Login failures are visible too; a broken install must not
                // appear silently successful just because it ran at login.
                if !no_dialog && action != "--verify" {
                    show(&message, true);
                }
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("SmartKey Windows setup requires Windows 10/11 x64.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run(action: &str) -> Result<String, String> {
    use smartkey_win::registration;
    use std::path::PathBuf;
    let local =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?);
    let roaming = PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?);
    let install = local.join("SmartKey");
    let installed_exe = install.join("smartkey-register.exe");
    let expected_dll = install.join("smartkey_win.dll");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe.parent().ok_or("Cannot locate SmartKey bundle")?;
    let _com = Com::new()?;
    let api_error = |e: windows::core::Error| {
        format!("{} (HRESULT 0x{:08X}).\nNo administrator fallback was attempted. Existing keyboards remain available through Win+Space.", e, e.code().0 as u32)
    };
    match action {
        "--install" => {
            user_setup::stage(bundle, &install, &roaming.join("smartkey"))
                .map_err(|e| format!("Cannot stage the SmartKey bundle: {e}. Close apps using SmartKey before updating."))?;
            let dll = expected_dll
                .to_str()
                .ok_or("DLL path is not valid Unicode")?;
            registration::register(dll).map_err(api_error)?;
            // A separate process queries the actual persisted TSF profile.
            // A process-local registration cannot pass this check.
            let verification = std::process::Command::new(&installed_exe)
                .arg("--verify")
                .output()
                .map_err(|e| format!("Cannot verify in a fresh process: {e}"))?;
            if !verification.status.success() {
                return Err(format!("Fresh-process SmartKey verification failed.\n{}\nRun Uninstall-SmartKey.cmd to remove any partial registration.", String::from_utf8_lossy(&verification.stderr)));
            }
            set_login(true, &installed_exe).map_err(api_error)?;
            let menu = roaming.join("Microsoft/Windows/Start Menu/Programs/SmartKey");
            std::fs::create_dir_all(&menu).map_err(|e| e.to_string())?;
            for name in [
                "Status-SmartKey.cmd",
                "Enable-SmartKey.cmd",
                "Disable-SmartKey.cmd",
                "Uninstall-SmartKey.cmd",
            ] {
                std::fs::copy(install.join(name), menu.join(name)).map_err(|e| e.to_string())?;
            }
            Ok("SmartKey installed for this user.\n\nPress Win+Space and select SmartKey. Press Win+Space again to return to your usual keyboard. Your existing layouts and default choice were preserved.\n\nSmartKey is available again at Windows login. Start menu > SmartKey provides Status, Enable, Disable and Uninstall. Existing personal data/configuration were preserved.\n\nOpen a new text app to begin.".into())
        }
        "--verify" | "--status" => {
            verify_com_path(&expected_dll).map_err(api_error)?;
            let enabled = registration::profile_enabled().map_err(api_error)?;
            if action == "--verify" && !enabled {
                return Err("SmartKey profile exists but is disabled".into());
            }
            Ok(format!("SmartKey current-user COM path verified.\nTSF profile: {}.\nBinary folder: {}\nData folder: {}\nUse Win+Space to choose SmartKey or an existing ordinary keyboard.", if enabled { "enabled" } else { "disabled" }, install.display(), roaming.join("smartkey").display()))
        }
        "--enable" | "--login" => {
            verify_com_path(&expected_dll).map_err(api_error)?;
            registration::set_user_enabled(true).map_err(api_error)?;
            if !registration::profile_enabled().map_err(api_error)? {
                return Err("Windows did not enable the SmartKey profile".into());
            }
            if action == "--enable" {
                set_login(true, &installed_exe).map_err(api_error)?;
            }
            Ok("SmartKey enabled for this user and available at login. Select it with Win+Space; your default keyboard was preserved.".into())
        }
        "--disable" => {
            verify_com_path(&expected_dll).map_err(api_error)?;
            registration::set_user_enabled(false).map_err(api_error)?;
            set_login(false, &installed_exe).map_err(api_error)?;
            if registration::profile_enabled().map_err(api_error)? {
                return Err("Windows still reports SmartKey enabled".into());
            }
            Ok("SmartKey disabled and its login entry removed. Use Win+Space to select your usual keyboard. Enable-SmartKey restores it; personal data is unchanged.".into())
        }
        "--uninstall" => {
            // Remove only this helper's exact Run value, even if TSF cleanup fails.
            let login = set_login(false, &installed_exe).map_err(api_error);
            let removal = registration::unregister().map_err(api_error);
            login?;
            removal?;
            let menu = roaming.join("Microsoft/Windows/Start Menu/Programs/SmartKey");
            // Keep the currently running uninstall CMD until its final block.
            for name in [
                "Status-SmartKey.cmd",
                "Enable-SmartKey.cmd",
                "Disable-SmartKey.cmd",
            ] {
                let path = menu.join(name);
                if path.exists() {
                    std::fs::remove_file(path).map_err(|e| e.to_string())?;
                }
            }
            Ok("SmartKey registration and its login entry were removed. Your ordinary keyboards and personal data remain.\n\nThe Uninstall command removes staged binaries after this window closes. If an app still holds the DLL, close that app and repeat file cleanup. Data/configuration under AppData\\smartkey are preserved.".into())
        }
        _ => Err("Use --install, --status, --enable, --disable or --uninstall.".into()),
    }
}

#[cfg(windows)]
struct Com;
#[cfg(windows)]
impl Com {
    fn new() -> Result<Self, String> {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
        }
        .ok()
        .map_err(|e| format!("COM initialization failed: {e}"))?;
        Ok(Self)
    }
}
#[cfg(windows)]
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}

#[cfg(windows)]
fn show(message: &str, failed: bool) {
    use windows::core::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("SmartKey"),
            MB_OK
                | if failed {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}

#[cfg(windows)]
fn read_value(key: &str, name: Option<&str>) -> windows::core::Result<Option<String>> {
    use windows::core::*;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::*;
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let name: Option<Vec<u16>> = name.map(|s| s.encode_utf16().chain(Some(0)).collect());
    let name = name.as_ref().map_or(PCWSTR::null(), |s| PCWSTR(s.as_ptr()));
    let mut data = vec![0u16; 32768];
    let mut size = (data.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            name,
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if result.0 != 0 {
        return Err(Error::from(HRESULT::from_win32(result.0)));
    }
    let length = data.iter().position(|&v| v == 0).unwrap_or(data.len());
    Ok(Some(String::from_utf16_lossy(&data[..length])))
}

#[cfg(windows)]
fn verify_com_path(expected: &std::path::Path) -> windows::core::Result<()> {
    use windows::core::Error;
    use windows::Win32::Foundation::E_FAIL;
    let key = format!(
        "SOFTWARE\\Classes\\CLSID\\{{{}}}\\InProcServer32",
        smartkey_win::config::CLSID_SMARTKEY_STR
    );
    let value = read_value(&key, None)?
        .ok_or_else(|| Error::new(E_FAIL, "SmartKey HKCU registration is missing"))?;
    if std::path::Path::new(&value) != expected || !expected.is_file() {
        return Err(Error::new(
            E_FAIL,
            "SmartKey registration does not point to the installed DLL",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn set_login(enabled: bool, exe: &std::path::Path) -> windows::core::Result<()> {
    use windows::core::*;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, E_FAIL};
    use windows::Win32::System::Registry::*;
    const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const NAME: &str = "SmartKey";
    let expected = format!("\"{}\" --login", exe.display());
    let existing = read_value(KEY, Some(NAME))?;
    if existing.as_ref().is_some_and(|value| value != &expected) {
        return Err(Error::new(
            E_FAIL,
            "An unrelated SmartKey login value already exists; it was preserved",
        ));
    }
    if !enabled && existing.is_none() {
        return Ok(());
    }
    let key: Vec<u16> = KEY.encode_utf16().chain(Some(0)).collect();
    let mut handle = HKEY::default();
    let opened = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), &mut handle) };
    if opened.0 != 0 {
        return Err(Error::from(HRESULT::from_win32(opened.0)));
    }
    let value: Vec<u16> = expected.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        if enabled {
            RegSetValueExW(
                handle,
                w!("SmartKey"),
                0,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    value.as_ptr().cast(),
                    value.len() * 2,
                )),
            )
        } else {
            RegDeleteValueW(handle, w!("SmartKey"))
        }
    };
    let close = unsafe { RegCloseKey(handle) };
    if result.0 != 0 && result != ERROR_FILE_NOT_FOUND {
        return Err(Error::from(HRESULT::from_win32(result.0)));
    }
    if close.0 != 0 {
        return Err(Error::from(HRESULT::from_win32(close.0)));
    }
    Ok(())
}
