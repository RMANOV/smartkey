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
    let install = machine_install_dir()?;
    let installed_exe = install.join("smartkey-register.exe");
    let expected_dll = install.join("smartkey_win.dll");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe.parent().ok_or("Cannot locate SmartKey bundle")?;
    let _com = Com::new()?;
    match action {
        "--install-machine" => {
            require_elevation(true)?;
            let parent = install.parent().ok_or("Cannot locate Program Files")?;
            validate_protected_path(parent)?;
            if registry_class_exists(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE)
                .map_err(api_error)? || install.exists() {
                return Err("A SmartKey machine installation or destination already exists. It was preserved. Run Uninstall-Machine-SmartKey.cmd from the extracted archive before installing a replacement.".into());
            }
            user_setup::stage_machine(bundle, &install)
                .map_err(|e| format!("Cannot stage protected machine bundle: {e}. Any partial destination was retained for explicit machine removal."))?;
            validate_machine_objects(&install)?;
            registration::register_machine(expected_dll.to_str().ok_or("DLL path is not Unicode")?)
                .map_err(api_error)?;
            fresh_verification(&installed_exe, "--verify-machine")?;
            Ok("SmartKey machine files and TSF catalog installed. No user's data, keyboard availability or login entry was configured.\n\nClose this administrator window, then double-click Install-SmartKey.cmd normally as each user who wants SmartKey.".into())
        }
        "--verify-machine" => {
            verify_machine_install(&install)?;
            registration::profile_present().map_err(api_error)?;
            Ok("SmartKey protected machine COM path and persistent TSF profile verified. User enablement was not required or changed.".into())
        }
        "--uninstall-machine" => {
            require_elevation(true)?;
            if exe.starts_with(&install) {
                return Err("Run Uninstall-Machine-SmartKey.cmd from the extracted original archive, outside Program Files\\SmartKey, so the running helper can remove all machine files.".into());
            }
            validate_protected_path(install.parent().ok_or("Cannot locate Program Files")?)?;
            if install.exists() { validate_machine_objects(&install)?; }
            if registry_class_exists(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE).map_err(api_error)? {
                verify_com_path(&expected_dll).map_err(api_error)?;
            }
            registration::unregister_machine().map_err(api_error)?;
            user_setup::remove_machine_files(&install)
                .map_err(|e| format!("Machine registration removed, but machine file removal failed: {e}. Close apps using SmartKey and retry this command from the extracted archive."))?;
            Ok("SmartKey machine catalog, COM registration and protected files removed. User data, other keyboards and user login entries were preserved. Each configured user can run Uninstall-SmartKey.cmd to remove their own login/menu entries.".into())
        }
        "--install" | "--enable" | "--disable" | "--login" | "--uninstall" => {
            require_elevation(false)?;
            refuse_user_com_override()?;
            if action != "--uninstall" { verify_machine_install(&install)?; }
            let roaming = known_folder(&windows::Win32::UI::Shell::FOLDERID_RoamingAppData)?;
            let menu = roaming.join("Microsoft/Windows/Start Menu/Programs/SmartKey");
            match action {
                "--install" => {
                    // Machine prerequisite above precedes every user mutation.
                    registration::profile_present().map_err(api_error)?;
                    user_setup::stage_user_data(&install, &roaming.join("smartkey"))
                        .map_err(|e| format!("Cannot stage missing user data: {e}"))?;
                    registration::set_user_enabled(true).map_err(api_error)?;
                    fresh_verification(&installed_exe, "--verify")?;
                    set_login(true, &installed_exe).map_err(api_error)?;
                    std::fs::create_dir_all(&menu).map_err(|e| e.to_string())?;
                    for name in ["Status-SmartKey.cmd", "Enable-SmartKey.cmd", "Disable-SmartKey.cmd", "Uninstall-SmartKey.cmd"] {
                        std::fs::copy(install.join(name), menu.join(name)).map_err(|e| e.to_string())?;
                    }
                    Ok("SmartKey enabled for this user.\n\nPress Win+Space and select SmartKey; press Win+Space again to return to your ordinary keyboard. Existing layouts and default choice were preserved.\n\nSmartKey is available at Windows login. Start menu > SmartKey provides Status, Enable, Disable and Uninstall. Existing personal data/configuration were preserved. Open a new text app to begin.".into())
                }
                "--enable" | "--login" => {
                    registration::set_user_enabled(true).map_err(api_error)?;
                    if !registration::profile_enabled().map_err(api_error)? { return Err("Windows did not enable the SmartKey profile".into()); }
                    if action == "--enable" { set_login(true, &installed_exe).map_err(api_error)?; }
                    Ok("SmartKey enabled for this user and available at login. Select it with Win+Space; your default keyboard was preserved.".into())
                }
                "--disable" => {
                    registration::set_user_enabled(false).map_err(api_error)?;
                    set_login(false, &installed_exe).map_err(api_error)?;
                    if registration::profile_enabled().map_err(api_error)? { return Err("Windows still reports SmartKey enabled".into()); }
                    Ok("SmartKey disabled and its login entry removed. Use Win+Space to select your ordinary keyboard. Enable-SmartKey restores it; personal data is unchanged.".into())
                }
                "--uninstall" => {
                    // User removal remains usable after the machine package is removed.
                    set_login(false, &installed_exe).map_err(api_error)?;
                    if install.exists() {
                        verify_machine_install(&install)?;
                        registration::unregister_user().map_err(api_error)?;
                        if registration::profile_enabled().map_err(api_error)? { return Err("Windows still reports this user's SmartKey profile enabled after removal".into()); }
                    }
                    for name in ["Status-SmartKey.cmd", "Enable-SmartKey.cmd", "Disable-SmartKey.cmd"] {
                        let path = menu.join(name);
                        if path.exists() { std::fs::remove_file(path).map_err(|e| e.to_string())?; }
                    }
                    Ok("SmartKey removed for this user. Its login entry was removed; ordinary keyboards, personal data and shared machine files were preserved. Use Uninstall-Machine-SmartKey.cmd from the extracted archive as administrator only when shared machine removal is wanted.".into())
                }
                _ => unreachable!(),
            }
        }
        "--verify" | "--status" => {
            refuse_user_com_override()?;
            verify_machine_install(&install)?;
            let enabled = registration::profile_enabled().map_err(api_error)?;
            if action == "--verify" && !enabled { return Err("SmartKey profile exists but is disabled".into()); }
            Ok(format!("SmartKey protected machine COM path verified.\nTSF profile: {}.\nUse Win+Space to choose SmartKey or an existing ordinary keyboard.", if enabled { "enabled" } else { "disabled" }))
        }
        _ => Err("Use --install, --status, --enable, --disable, --uninstall, --install-machine or --uninstall-machine.".into()),
    }
}

#[cfg(windows)]
fn api_error(error: windows::core::Error) -> String {
    format!(
        "{} (HRESULT 0x{:08X}). Existing keyboards remain available through Win+Space.",
        error,
        error.code().0 as u32
    )
}

#[cfg(windows)]
fn fresh_verification(helper: &std::path::Path, action: &str) -> Result<(), String> {
    let result = std::process::Command::new(helper)
        .arg(action)
        .arg("--no-dialog")
        .output()
        .map_err(|e| format!("Cannot verify in a fresh process: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "Fresh-process SmartKey verification failed.\n{}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn machine_install_dir() -> Result<std::path::PathBuf, String> {
    Ok(known_folder(&windows::Win32::UI::Shell::FOLDERID_ProgramFiles)?.join("SmartKey"))
}

#[cfg(windows)]
fn known_folder(id: &windows::core::GUID) -> Result<std::path::PathBuf, String> {
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    let raw = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.map_err(api_error)?;
    let result = unsafe { raw.to_string() }.map_err(|e| e.to_string());
    unsafe {
        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0.cast()));
    }
    Ok(std::path::PathBuf::from(result?))
}

#[cfg(windows)]
fn require_elevation(required: bool) -> Result<(), String> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Security::{
        CheckTokenMembership, CreateWellKnownSid, WinBuiltinAdministratorsSid, PSID,
    };
    let mut storage = [0u32; 17];
    let sid = PSID(storage.as_mut_ptr().cast());
    let mut size = std::mem::size_of_val(&storage) as u32;
    unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, PSID::default(), sid, &mut size) }
        .map_err(api_error)?;
    let mut elevated = BOOL::default();
    unsafe { CheckTokenMembership(None, sid, &mut elevated) }.map_err(api_error)?;
    if elevated.as_bool() != required {
        return Err(if required {
            "This explicit machine action requires administrator rights. Right-click the matching Machine-SmartKey.cmd in the extracted archive and choose Run as administrator."
        } else {
            "Run this user action normally, without Run as administrator, in the account that will use SmartKey."
        }.into());
    }
    Ok(())
}

#[cfg(windows)]
fn missing_machine_error() -> String {
    "SmartKey machine installation is missing. Ask an administrator to right-click Install-Machine-SmartKey.cmd and choose Run as administrator, then run Install-SmartKey.cmd normally.".into()
}

#[cfg(windows)]
fn verify_machine_install(install: &std::path::Path) -> Result<(), String> {
    if !install.is_dir() {
        return Err(missing_machine_error());
    }
    validate_protected_path(install.parent().ok_or("Cannot locate Program Files")?)?;
    validate_machine_objects(install)?;
    user_setup::validate_bundle(install)
        .map_err(|e| format!("SmartKey machine installation is incomplete: {e}"))?;
    verify_com_path(&install.join("smartkey_win.dll")).map_err(api_error)
}

#[cfg(windows)]
fn validate_machine_objects(install: &std::path::Path) -> Result<(), String> {
    validate_protected_path(install)?;
    let allowed: Vec<_> = user_setup::BINARIES
        .iter()
        .chain(user_setup::DATA)
        .chain(user_setup::SUPPORT)
        .copied()
        .collect();
    for entry in std::fs::read_dir(install).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !allowed
            .iter()
            .any(|name| entry.file_name() == std::ffi::OsStr::new(name))
        {
            return Err("Unexpected object in machine destination; it was preserved".into());
        }
        validate_protected_path(&entry.path())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("Machine payload contains a non-file object".into());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn validate_protected_path(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    use windows::core::*;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::*;
    use windows::Win32::Security::*;
    // Reject junctions/symlinks anywhere in the path before copying/loading.
    for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Refusing a reparse point in the machine destination".into());
        }
    }
    let text: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut owner = PSID::default();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(text.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        )
    };
    if status.0 != 0 {
        return Err(api_error(Error::from(HRESULT::from_win32(status.0))));
    }
    let checked = (|| {
        let trusted_sid = |sid: PSID| -> std::result::Result<bool, String> {
            let mut string = PWSTR::null();
            unsafe { ConvertSidToStringSidW(sid, &mut string) }.map_err(api_error)?;
            let sid_text = unsafe { string.to_string() }.map_err(|e| e.to_string());
            unsafe {
                LocalFree(HLOCAL(string.0.cast()));
            }
            Ok(matches!(
                sid_text?.as_str(),
                "S-1-5-18"
                    | "S-1-5-32-544"
                    | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
            ))
        };
        if !trusted_sid(owner)? {
            return Err(
                "Machine object is not owned by SYSTEM, Administrators or TrustedInstaller".into(),
            );
        }
        if dacl.is_null() {
            return Err("Refusing an unprotected machine destination (NULL DACL)".into());
        }
        for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
            let mut raw = std::ptr::null_mut();
            unsafe { GetAce(dacl, index, &mut raw) }.map_err(api_error)?;
            let header = unsafe { &*(raw as *const ACE_HEADER) };
            if header.AceFlags & 8 != 0 || header.AceType == 1 {
                continue;
            } // inherit-only / deny
            if header.AceType != 0 {
                return Err("Machine destination has an unsupported access-grant ACE".into());
            }
            let ace = unsafe { &*(raw as *const ACCESS_ALLOWED_ACE) };
            // Write/delete/permission-change rights; read/synchronize are allowed.
            if ace.Mask & 0x500D0156 != 0 {
                let sid = PSID(std::ptr::addr_of!(ace.SidStart).cast_mut().cast());
                if !trusted_sid(sid)? {
                    return Err(
                        "Machine destination grants write/delete access to an untrusted principal"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    })();
    unsafe {
        LocalFree(HLOCAL(descriptor.0));
    }
    checked
}

#[cfg(windows)]
fn refuse_user_com_override() -> Result<(), String> {
    if registry_class_exists(windows::Win32::System::Registry::HKEY_CURRENT_USER)
        .map_err(api_error)?
    {
        return Err("An existing per-user SmartKey COM registration could override the protected machine DLL. It was preserved. Ask an administrator to review this exact old user registration before retrying. Do not run a legacy SmartKey uninstaller after machine installation: it may remove the shared TSF catalog.".into());
    }
    Ok(())
}

#[cfg(windows)]
fn registry_class_exists(
    root: windows::Win32::System::Registry::HKEY,
) -> windows::core::Result<bool> {
    use windows::core::*;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::*;
    let key = format!(
        "SOFTWARE\\Classes\\CLSID\\{{{}}}",
        smartkey_win::config::CLSID_SMARTKEY_STR
    );
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let mut opened = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(key.as_ptr()), 0, KEY_READ, &mut opened) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status.0 != 0 {
        return Err(Error::from(HRESULT::from_win32(status.0)));
    }
    let closed = unsafe { RegCloseKey(opened) };
    if closed.0 != 0 {
        return Err(Error::from(HRESULT::from_win32(closed.0)));
    }
    Ok(true)
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
fn read_value(
    root: windows::Win32::System::Registry::HKEY,
    key: &str,
    name: Option<&str>,
) -> windows::core::Result<Option<String>> {
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
            root,
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
    let value = read_value(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        &key,
        None,
    )
    .map_err(|e| {
        Error::new(
            e.code(),
            format!("Verification HKLM protected machine COM path / RegGetValueW: {e}"),
        )
    })?
    .ok_or_else(|| {
        Error::new(
            E_FAIL,
            "Verification HKLM protected machine COM path: SmartKey registration is missing",
        )
    })?;
    if std::path::Path::new(&value) != expected || !expected.is_file() {
        return Err(Error::new(
            E_FAIL,
            "Verification HKLM protected machine COM path: registration does not point to the installed DLL",
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
    let existing = read_value(HKEY_CURRENT_USER, KEY, Some(NAME))?;
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
