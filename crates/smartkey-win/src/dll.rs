//! COM DLL entry points and global state.
//!
//! Exports the standard COM DLL functions:
//!   - DllMain: stores HINSTANCE for DLL path resolution
//!   - DllGetClassObject: returns IClassFactory for SmartKeyTextService
//!   - DllCanUnloadNow: checks if DLL can be safely unloaded
//!   - DllRegisterServer / DllUnregisterServer: self-registration

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use windows::core::{IUnknown, Interface, GUID};
use windows::Win32::Foundation::{
    BOOL, E_FAIL, E_POINTER, HINSTANCE, HMODULE, MAX_PATH, S_FALSE, S_OK,
};

use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IClassFactory, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

use crate::class_factory::SmartKeyClassFactory;
use crate::config::CLSID_SMARTKEY;

const CLASS_E_CLASSNOTAVAILABLE: windows::core::HRESULT =
    windows::core::HRESULT(0x80040111_u32 as i32);
const E_NOINTERFACE: windows::core::HRESULT = windows::core::HRESULT(0x80004002_u32 as i32);

/// Raw HINSTANCE pointer, set once in DllMain. Using AtomicPtr because
/// HINSTANCE wraps a raw pointer (*mut c_void) which is !Send + !Sync.
static DLL_INSTANCE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Server lock count from IClassFactory::LockServer.
pub(crate) static LOCK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Live COM object count, incremented in CreateInstance, decremented in Deactivate.
pub(crate) static OBJECT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Get the full filesystem path of this DLL.
pub(crate) fn dll_path() -> Option<String> {
    let raw = DLL_INSTANCE.load(Ordering::SeqCst);
    if raw.is_null() {
        return None;
    }
    let hmod = HMODULE(raw);
    let mut buf = [0u16; MAX_PATH as usize];
    let len = unsafe { GetModuleFileNameW(hmod, &mut buf) } as usize;
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

const DLL_PROCESS_ATTACH: u32 = 1;

/// DLL entry point — stores HINSTANCE for later path resolution.
#[no_mangle]
pub extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        DLL_INSTANCE.store(hinst.0, Ordering::SeqCst);
    }
    BOOL::from(true)
}

/// Returns S_OK if the DLL can be safely unloaded (no live objects or server locks).
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> windows::core::HRESULT {
    if OBJECT_COUNT.load(Ordering::SeqCst) == 0 && LOCK_COUNT.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

/// Returns IClassFactory for the requested CLSID.
///
/// Called by COM runtime during CoCreateInstance. Returns
/// CLASS_E_CLASSNOTAVAILABLE if the CLSID doesn't match ours.
///
/// # Safety
/// Caller must provide valid pointers for `rclsid` and `ppv`.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> windows::core::HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();

    if rclsid.is_null() || *rclsid != CLSID_SMARTKEY {
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    // Honor the requested interface — only IClassFactory and IUnknown are valid here.
    if riid.is_null() || (*riid != IClassFactory::IID && *riid != IUnknown::IID) {
        return E_NOINTERFACE;
    }

    let factory: IClassFactory = SmartKeyClassFactory.into();
    *ppv = ManuallyDrop::new(factory).as_raw();
    S_OK
}

/// Self-registration entry point (called by `regsvr32 smartkey_win.dll`).
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> windows::core::HRESULT {
    let Some(path) = dll_path() else {
        return E_FAIL;
    };

    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return hr;
    }

    let result = crate::registration::register(&path);

    unsafe { CoUninitialize() };

    match result {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

/// Self-unregistration entry point (called by `regsvr32 /u smartkey_win.dll`).
#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> windows::core::HRESULT {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return hr;
    }

    let result = crate::registration::unregister();

    unsafe { CoUninitialize() };

    match result {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}
