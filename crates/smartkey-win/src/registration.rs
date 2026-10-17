//! TSF IME registration — writes registry keys and registers TIP profile.
//!
//! Used by both DllRegisterServer (self-registration via regsvr32) and
//! the standalone smartkey-register.exe binary.
//!
//! Registration creates:
//!   1. HKCU\SOFTWARE\Classes\CLSID\{CLSID}\InProcServer32 → DLL path + Apartment
//!   2. TSF TIP profile via ITfInputProcessorProfileMgr::RegisterProfile
//!   3. TSF keyboard/display categories via ITfCategoryMgr::RegisterCategory
//!   4. Current-user availability via input.dll InstallLayoutOrTipUserReg
//!
//! Precondition: COM must be initialized (CoInitializeEx) before calling.

use windows::core::*;
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::*;

use crate::config::{CLSID_SMARTKEY, CLSID_SMARTKEY_STR, GUID_PROFILE};

/// Bulgarian language ID (bg-BG = 0x0402).
const LANGID_BG: u16 = 0x0402;

/// Display name shown in Windows language settings.
const DISPLAY_NAME: &str = "SmartKey";

// Persistent registration uses flags 0. TF_RP_LOCALPROCESS is NOT per-user:
// it vanishes with the installer process. User availability is configured by
// the documented input.dll InstallLayoutOrTipUserReg API below.

/// Register COM only in HKCU, then the persistent TSF profile/categories and
/// enable exactly this profile for the current user. No elevation fallback.
/// Returns false for compatibility with DllRegisterServer's existing caller.
/// Any denied TSF operation is a real installation failure, not a skipped step.
pub fn register(dll_path: &str) -> Result<bool> {
    register_com_server_hkcu(dll_path).map_err(|e| step_error("HKCU COM registration", e))?;
    register_tip_profile(dll_path, 0, true).map_err(|e| step_error("Persistent TSF profile", e))?;
    register_categories().map_err(|e| step_error("TSF categories", e))?;
    set_user_enabled(true)?;
    Ok(false)
}

/// Explicit elevated machine phase. The caller must stage and validate a
/// protected machine DLL first. Never changes any user's availability or data.
pub fn register_machine(dll_path: &str) -> Result<()> {
    register_com_server(HKEY_LOCAL_MACHINE, dll_path)
        .map_err(|e| step_error("Machine COM registration", e))?;
    register_tip_profile(dll_path, 0, false)
        .map_err(|e| step_error("Persistent machine TSF profile", e))?;
    register_categories().map_err(|e| step_error("Machine TSF categories", e))
}

/// Remove only the executing user's availability. Shared catalog/COM remain.
pub fn unregister_user() -> Result<()> {
    install_layout_or_tip(1).map_err(|e| step_error("Current-user profile removal", e))
}

/// Explicit elevated machine cleanup; never changes a user's Run/data/menu.
pub fn unregister_machine() -> Result<()> {
    collect_removal_errors([
        ("Machine categories", unregister_categories()),
        ("Machine text service", unregister_text_service()),
        ("Machine COM", unregister_com_server(HKEY_LOCAL_MACHINE)),
    ])
}

/// Attempt each own-identity cleanup and return failures rather than claiming
/// success after partial removal. Never deletes HKLM or another keyboard.
pub fn unregister() -> Result<()> {
    let operations = [
        ("Current-user profile removal", install_layout_or_tip(1)),
        ("Categories", unregister_categories()),
        ("TSF profile", unregister_tip_profile()),
        ("HKCU COM", unregister_com_server_hkcu()),
    ];
    collect_removal_errors(operations)
}

fn collect_removal_errors<const N: usize>(operations: [(&str, Result<()>); N]) -> Result<()> {
    let errors: Vec<_> = operations
        .into_iter()
        .filter_map(|(name, result)| result.err().map(|e| (e.code(), format!("{name}: {e}"))))
        .collect();
    if let Some((code, _)) = errors.first() {
        return Err(Error::new(
            *code,
            errors
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    Ok(())
}

fn step_error(step: &str, error: Error) -> Error {
    Error::new(error.code(), format!("{step}: {error}"))
}

/// Microsoft documents NULL user path as HKCU. No DEFPROFILE/CLEANINSTALL flag:
/// existing layouts and default selection remain under the user's control.
pub fn set_user_enabled(enabled: bool) -> Result<()> {
    install_layout_or_tip(if enabled { 0 } else { 0x80 })
}

fn install_layout_or_tip(flags: u32) -> Result<()> {
    use windows::Win32::Foundation::{FreeLibrary, BOOL, E_FAIL, HANDLE};
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    type Install = unsafe extern "system" fn(PCWSTR, PCWSTR, PCWSTR, PCWSTR, u32) -> BOOL;
    // Restrict resolution to the Windows system directory; do not load input.dll
    // from the portable bundle/current working directory.
    let module = unsafe {
        LoadLibraryExW(
            w!("input.dll"),
            HANDLE::default(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )?
    };
    let result = (|| {
        let pointer = unsafe { GetProcAddress(module, s!("InstallLayoutOrTipUserReg")) }
            .ok_or_else(|| {
                Error::new(
                    E_FAIL,
                    "input.dll does not export InstallLayoutOrTipUserReg",
                )
            })?;
        let install: Install = unsafe { std::mem::transmute(pointer) };
        let profile = format!(
            "0x0402:{{{}}}{{{}}}",
            CLSID_SMARTKEY_STR,
            crate::config::GUID_PROFILE_STR
        );
        let profile_w: Vec<u16> = profile.encode_utf16().chain(Some(0)).collect();
        let ok = unsafe {
            install(
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR(profile_w.as_ptr()),
                flags,
            )
        };
        if ok.as_bool() {
            Ok(())
        } else {
            // Documented BOOL failure has no guaranteed last-error contract.
            Err(Error::new(E_FAIL, "InstallLayoutOrTipUserReg returned FALSE; current-user availability was not applied"))
        }
    })();
    unsafe {
        FreeLibrary(module)?;
    }
    result
}

/// Verify exact registration metadata before querying current-user enablement.
/// API failures remain failures; a missing profile is never treated as disabled.
pub fn profile_enabled() -> Result<bool> {
    profile_present()?;
    let profiles =
        input_processor_profiles("Enabled-state CoCreateInstance(ITfInputProcessorProfiles)")?;
    unsafe { profiles.IsEnabledLanguageProfile(&CLSID_SMARTKEY, LANGID_BG, &GUID_PROFILE) }
        .map(|enabled| enabled.as_bool())
        .map_err(|e| step_error("ITfInputProcessorProfiles::IsEnabledLanguageProfile", e))
}

/// Machine verification reads registered metadata without requiring enablement.
/// The caller's fresh process must see this exact CLSID/language/profile tuple.
pub fn profile_present() -> Result<()> {
    let profiles = input_processor_profiles(
        "Registration metadata CoCreateInstance(ITfInputProcessorProfiles)",
    )?;
    let description = unsafe {
        profiles.GetLanguageProfileDescription(&CLSID_SMARTKEY, LANGID_BG, &GUID_PROFILE)
    }
    .map_err(|e| {
        step_error(
            "ITfInputProcessorProfiles::GetLanguageProfileDescription",
            e,
        )
    })?;
    if description != DISPLAY_NAME {
        return Err(Error::new(
            windows::Win32::Foundation::E_FAIL,
            "Registered SmartKey profile description does not match the expected metadata",
        ));
    }
    Ok(())
}

fn input_processor_profiles(operation: &str) -> Result<ITfInputProcessorProfiles> {
    unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| step_error(operation, e))
}

/// Explicit machine removal owns the entire SmartKey text service. This is not
/// called by ordinary user removal, and does not edit CTF registry keys directly.
fn unregister_text_service() -> Result<()> {
    let profiles = input_processor_profiles(
        "Machine service removal CoCreateInstance(ITfInputProcessorProfiles)",
    )?;
    unsafe { profiles.Unregister(&CLSID_SMARTKEY) }
        .map_err(|e| step_error("ITfInputProcessorProfiles::Unregister", e))
}

// -- COM server registration (registry) --------------------------------

fn register_com_server_hkcu(dll_path: &str) -> Result<()> {
    register_com_server(HKEY_CURRENT_USER, dll_path)
}

fn register_com_server(root: HKEY, dll_path: &str) -> Result<()> {
    let subkey = format!(
        "SOFTWARE\\Classes\\CLSID\\{{{}}}\\InProcServer32",
        CLSID_SMARTKEY_STR
    );
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();

    let mut hkey = HKEY::default();
    check_win32(unsafe { RegCreateKeyW(root, PCWSTR(subkey_w.as_ptr()), &mut hkey) })?;

    let result = set_reg_sz(hkey, None, dll_path)
        .and_then(|_| set_reg_sz(hkey, Some("ThreadingModel"), "Apartment"));
    let closed = check_win32(unsafe { RegCloseKey(hkey) });
    result.and(closed)
}

fn unregister_com_server_hkcu() -> Result<()> {
    unregister_com_server(HKEY_CURRENT_USER)
}

fn unregister_com_server(root: HKEY) -> Result<()> {
    let subkey = format!("SOFTWARE\\Classes\\CLSID\\{{{}}}", CLSID_SMARTKEY_STR);
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe { RegDeleteTreeW(root, PCWSTR(subkey_w.as_ptr())) };
    if result == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        check_win32(result)
    }
}

// -- TSF TIP profile registration --------------------------------------

fn register_tip_profile(dll_path: &str, flags: u32, enabled_by_default: bool) -> Result<()> {
    let profile_mgr: ITfInputProcessorProfileMgr =
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| step_error("CoCreateInstance(ITfInputProcessorProfileMgr)", e))?;

    let name_w: Vec<u16> = DISPLAY_NAME.encode_utf16().collect();
    let icon_w: Vec<u16> = dll_path.encode_utf16().collect();

    unsafe {
        profile_mgr
            .RegisterProfile(
                &CLSID_SMARTKEY,
                LANGID_BG,
                &GUID_PROFILE,
                &name_w,
                &icon_w,
                0,              // icon index
                HKL::default(), // no substitute layout
                0,              // no preferred layout
                enabled_by_default,
                flags,
            )
            .map_err(|e| step_error("ITfInputProcessorProfileMgr::RegisterProfile", e))?;
    }

    Ok(())
}

fn unregister_tip_profile() -> Result<()> {
    let profile_mgr: ITfInputProcessorProfileMgr =
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)? };

    unsafe {
        profile_mgr.UnregisterProfile(&CLSID_SMARTKEY, LANGID_BG, &GUID_PROFILE, 0)?;
    }

    Ok(())
}

// -- TSF category registration ------------------------------------------

fn register_categories() -> Result<()> {
    let cat_mgr: ITfCategoryMgr =
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };

    unsafe {
        cat_mgr.RegisterCategory(&CLSID_SMARTKEY, &GUID_TFCAT_TIP_KEYBOARD, &CLSID_SMARTKEY)?;
        cat_mgr.RegisterCategory(
            &CLSID_SMARTKEY,
            &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
            &CLSID_SMARTKEY,
        )?;
    }

    Ok(())
}

fn unregister_categories() -> Result<()> {
    let cat_mgr: ITfCategoryMgr =
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };

    unsafe {
        let keyboard =
            cat_mgr.UnregisterCategory(&CLSID_SMARTKEY, &GUID_TFCAT_TIP_KEYBOARD, &CLSID_SMARTKEY);
        let display = cat_mgr.UnregisterCategory(
            &CLSID_SMARTKEY,
            &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
            &CLSID_SMARTKEY,
        );
        keyboard?;
        display?;
    }

    Ok(())
}

// -- Helpers ------------------------------------------------------------

/// Convert a WIN32_ERROR to a windows::core::Result.
fn check_win32(err: WIN32_ERROR) -> Result<()> {
    if err.0 == 0 {
        Ok(())
    } else {
        Err(Error::from(HRESULT::from_win32(err.0)))
    }
}

/// Write a REG_SZ value to an open registry key.
fn set_reg_sz(hkey: HKEY, name: Option<&str>, value: &str) -> Result<()> {
    let name_w: Option<Vec<u16>> =
        name.map(|n| n.encode_utf16().chain(std::iter::once(0)).collect());
    let value_w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();

    check_win32(unsafe {
        RegSetValueExW(
            hkey,
            name_w
                .as_ref()
                .map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            0,
            REG_SZ,
            Some(std::slice::from_raw_parts(
                value_w.as_ptr() as *const u8,
                value_w.len() * 2,
            )),
        )
    })
}
