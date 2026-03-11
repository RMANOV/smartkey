//! TSF IME registration — writes registry keys and registers TIP profile.
//!
//! Used by both DllRegisterServer (self-registration via regsvr32) and
//! the standalone smartkey-register.exe binary.
//!
//! Registration creates:
//!   1. HKLM\SOFTWARE\Classes\CLSID\{CLSID}\InProcServer32 → DLL path + Apartment
//!   2. TSF TIP profile via ITfInputProcessorProfileMgr::RegisterProfile
//!   3. TSF keyboard category via ITfCategoryMgr::RegisterCategory
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

// -- Public API ---------------------------------------------------------

/// Register the SmartKey IME. `dll_path` is the absolute path to the DLL.
pub fn register(dll_path: &str) -> Result<()> {
    register_com_server(dll_path)?;
    register_tip_profile(dll_path)?;
    register_categories()?;
    Ok(())
}

/// Unregister the SmartKey IME. Errors are non-fatal (best-effort cleanup).
pub fn unregister() -> Result<()> {
    let _ = unregister_categories();
    let _ = unregister_tip_profile();
    unregister_com_server()?;
    Ok(())
}

// -- COM server registration (registry) --------------------------------

fn register_com_server(dll_path: &str) -> Result<()> {
    let subkey = format!(
        "SOFTWARE\\Classes\\CLSID\\{{{}}}\\InProcServer32",
        CLSID_SMARTKEY_STR
    );
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();

    let mut hkey = HKEY::default();
    check_win32(unsafe {
        RegCreateKeyW(HKEY_LOCAL_MACHINE, PCWSTR(subkey_w.as_ptr()), &mut hkey)
    })?;

    // Default value = DLL path.
    set_reg_sz(hkey, None, dll_path)?;

    // ThreadingModel = Apartment (STA — required for TSF).
    set_reg_sz(hkey, Some("ThreadingModel"), "Apartment")?;

    let _ = unsafe { RegCloseKey(hkey) };
    Ok(())
}

fn unregister_com_server() -> Result<()> {
    let subkey = format!("SOFTWARE\\Classes\\CLSID\\{{{}}}", CLSID_SMARTKEY_STR);
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(subkey_w.as_ptr())) };
    Ok(())
}

// -- TSF TIP profile registration --------------------------------------

fn register_tip_profile(dll_path: &str) -> Result<()> {
    let profile_mgr: ITfInputProcessorProfileMgr =
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)? };

    let name_w: Vec<u16> = DISPLAY_NAME.encode_utf16().collect();
    let icon_w: Vec<u16> = dll_path.encode_utf16().collect();

    unsafe {
        profile_mgr.RegisterProfile(
            &CLSID_SMARTKEY,
            LANGID_BG,
            &GUID_PROFILE,
            &name_w,
            &icon_w,
            0,              // icon index
            HKL::default(), // no substitute layout
            0,              // no preferred layout
            true,           // enable by default
            0,              // flags
        )?;
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
    }

    Ok(())
}

fn unregister_categories() -> Result<()> {
    let cat_mgr: ITfCategoryMgr =
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };

    unsafe {
        cat_mgr.UnregisterCategory(&CLSID_SMARTKEY, &GUID_TFCAT_TIP_KEYBOARD, &CLSID_SMARTKEY)?;
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
