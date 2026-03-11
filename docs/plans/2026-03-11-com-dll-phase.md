# SmartKey COM DLL Phase Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Package the SmartKey TSF adapter as a COM in-process server DLL that Windows can load via CoCreateInstance, with self-registration and a standalone register.exe.

**Architecture:** Standard COM DLL pattern — `DllGetClassObject` exports an `IClassFactory` that creates `SmartKeyTextService` instances. Registration writes CLSID to registry and uses `ITfInputProcessorProfileMgr` for TSF profile setup. Global state uses atomics (thread-safe) while COM objects use `RefCell` (STA-safe). A standalone `smartkey-register.exe` provides admin-elevated registration as an alternative to `regsvr32`.

**Tech Stack:** Rust, windows-rs 0.58 (`#[implement]` macro), COM STA, Win32 Registry API, TSF registration APIs

**Reference:** [windows-rs issue #1819](https://github.com/microsoft/windows-rs/issues/1819) — `ManuallyDrop` pattern for COM pointer handoff

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/smartkey-win/Cargo.toml` | MODIFY | Add `cdylib` crate type + `Win32_System_LibraryLoader` feature |
| `crates/smartkey-win/src/lib.rs` | MODIFY | Add module declarations for new files |
| `crates/smartkey-win/src/dll.rs` | CREATE | DLL entry point (DllMain), global state, all 4 COM DLL exports |
| `crates/smartkey-win/src/class_factory.rs` | CREATE | `IClassFactory` impl — creates `SmartKeyTextService` on demand |
| `crates/smartkey-win/src/registration.rs` | CREATE | Registry + TSF profile + category registration logic |
| `crates/smartkey-win/src/register.rs` | MODIFY | Wire `--install`/`--uninstall` to actual registration code |

### Dependency Graph

```
dll.rs ──→ class_factory.rs ──→ tsf.rs (SmartKeyTextService)
  │
  └──→ registration.rs ──→ config.rs (CLSID, GUID_PROFILE)
                ↑
register.rs ────┘
```

---

## Chunk 1: DLL Scaffolding

### Task 1: Update Cargo.toml

**Files:**
- Modify: `crates/smartkey-win/Cargo.toml`

- [ ] **Step 1: Add `[lib]` section with cdylib crate type**

Add before `[[bin]]`:
```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

`cdylib` produces `smartkey_win.dll`. `rlib` allows the `smartkey-register` binary to link against the library.

- [ ] **Step 2: Add `Win32_System_LibraryLoader` feature**

Add to the `windows` features list (needed for `GetModuleFileNameW`):
```
"Win32_System_LibraryLoader",
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p smartkey-win
```

- [ ] **Step 4: Commit**

```bash
git add crates/smartkey-win/Cargo.toml
git commit -m "chore(win): add cdylib crate type for COM DLL output"
```

---

### Task 2: Create DLL globals + DllMain

**Files:**
- Create: `crates/smartkey-win/src/dll.rs`
- Modify: `crates/smartkey-win/src/lib.rs`

- [ ] **Step 1: Create `dll.rs` with DllMain and global state**

```rust
//! COM DLL entry points and global state.
//!
//! Exports the standard COM DLL functions:
//!   - DllMain: stores HINSTANCE for DLL path resolution
//!   - DllGetClassObject: returns IClassFactory for SmartKeyTextService
//!   - DllCanUnloadNow: checks if DLL can be safely unloaded
//!   - DllRegisterServer / DllUnregisterServer: self-registration

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows::Win32::Foundation::{BOOL, HINSTANCE, MAX_PATH};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

/// HINSTANCE of this DLL, set once in DllMain.
static DLL_INSTANCE: OnceLock<HINSTANCE> = OnceLock::new();

/// Server lock count from IClassFactory::LockServer.
pub(crate) static LOCK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Get the full filesystem path of this DLL.
pub(crate) fn dll_path() -> Option<String> {
    let hinst = DLL_INSTANCE.get().copied()?;
    let mut buf = [0u16; MAX_PATH as usize];
    let len = unsafe { GetModuleFileNameW(Some(hinst.into()), &mut buf) } as usize;
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

const DLL_PROCESS_ATTACH: u32 = 1;

/// DLL entry point — stores HINSTANCE for later path resolution.
#[no_mangle]
pub extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        let _ = DLL_INSTANCE.set(hinst);
    }
    BOOL::from(true)
}

/// Returns S_OK if the DLL can be safely unloaded (no server locks).
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> windows::core::HRESULT {
    if LOCK_COUNT.load(Ordering::SeqCst) == 0 {
        windows::Win32::Foundation::S_OK
    } else {
        windows::Win32::Foundation::S_FALSE
    }
}
```

> **Note:** `DllGetClassObject`, `DllRegisterServer`, and `DllUnregisterServer` are added in later tasks after their dependencies exist.

- [ ] **Step 2: Add `dll` module to `lib.rs`**

Add with `#[cfg(windows)]` guard alongside existing modules:
```rust
#[cfg(windows)]
pub mod dll;
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p smartkey-win
```

- [ ] **Step 4: Commit**

```bash
git add crates/smartkey-win/src/dll.rs crates/smartkey-win/src/lib.rs
git commit -m "feat(win): add DLL globals module with DllMain and DllCanUnloadNow"
```

---

## Chunk 2: Class Factory + DllGetClassObject

### Task 3: Create IClassFactory implementation

**Files:**
- Create: `crates/smartkey-win/src/class_factory.rs`
- Modify: `crates/smartkey-win/src/lib.rs`

- [ ] **Step 1: Create `class_factory.rs`**

```rust
//! COM Class Factory for SmartKey TSF Text Input Processor.
//!
//! Implements IClassFactory to create SmartKeyTextService instances
//! when Windows calls CoCreateInstance with CLSID_SMARTKEY.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::Ordering;

use windows::core::*;
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::Com::IClassFactory_Impl;

use crate::dll::LOCK_COUNT;
use crate::tsf::SmartKeyTextService;

/// Class factory that creates SmartKeyTextService instances.
#[implement(IClassFactory)]
pub(crate) struct SmartKeyClassFactory;

impl IClassFactory_Impl for SmartKeyClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        _riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        // COM aggregation not supported.
        if punkouter.is_some() {
            return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
        }

        unsafe {
            if ppvobject.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            *ppvobject = std::ptr::null_mut();
        }

        // Create the TIP and transfer ownership to the caller.
        let tip = SmartKeyTextService::new();
        let unknown: IUnknown = tip.into();
        unsafe {
            *ppvobject = ManuallyDrop::new(unknown).as_raw();
        }

        Ok(())
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            LOCK_COUNT.fetch_add(1, Ordering::SeqCst);
        } else {
            LOCK_COUNT.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
```

**Key pattern:** `ManuallyDrop::new(unknown).as_raw()` transfers the COM reference to the caller without Rust calling `Release`. The caller (Windows) manages the lifetime via `IUnknown::Release`.

- [ ] **Step 2: Add `class_factory` module to `lib.rs`**

```rust
#[cfg(windows)]
mod class_factory;
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p smartkey-win
```

> **Verify:** If `CLASS_E_NOAGGREGATION` or `E_POINTER` are not found, check imports. They may be in `windows::Win32::Foundation` or `windows::Win32::System::Com`. If not available, define manually:
> ```rust
> const CLASS_E_NOAGGREGATION: HRESULT = HRESULT(0x80040110_u32 as i32);
> const E_POINTER: HRESULT = HRESULT(0x80004003_u32 as i32);
> ```

- [ ] **Step 4: Commit**

```bash
git add crates/smartkey-win/src/class_factory.rs crates/smartkey-win/src/lib.rs
git commit -m "feat(win): add IClassFactory implementation for COM object creation"
```

---

### Task 4: Add DllGetClassObject export

**Files:**
- Modify: `crates/smartkey-win/src/dll.rs`

- [ ] **Step 1: Add DllGetClassObject to `dll.rs`**

Add imports and function:
```rust
use std::mem::ManuallyDrop;
use windows::core::GUID;
use windows::Win32::System::Com::IClassFactory;

use crate::class_factory::SmartKeyClassFactory;
use crate::config::CLSID_SMARTKEY;

/// Returns IClassFactory for the requested CLSID.
///
/// Called by COM runtime during CoCreateInstance. Returns
/// CLASS_E_CLASSNOTAVAILABLE if the CLSID doesn't match ours.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    _riid: *const GUID,
    ppv: *mut *mut c_void,
) -> windows::core::HRESULT {
    if ppv.is_null() {
        return windows::Win32::Foundation::E_POINTER;
    }
    *ppv = std::ptr::null_mut();

    if rclsid.is_null() || *rclsid != CLSID_SMARTKEY {
        return windows::Win32::System::Com::CLASS_E_CLASSNOTAVAILABLE;
    }

    let factory: IClassFactory = SmartKeyClassFactory.into();
    *ppv = ManuallyDrop::new(factory).as_raw();
    windows::Win32::Foundation::S_OK
}
```

> **Note:** `_riid` is intentionally ignored because `SmartKeyClassFactory.into()` produces an `IClassFactory` which inherits from `IUnknown`. Windows only ever requests `IID_IClassFactory` or `IID_IUnknown` here — both are satisfied by the vtable.

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p smartkey-win
```

- [ ] **Step 3: Build the DLL**

```bash
cargo build -p smartkey-win --lib
```

Verify DLL exists:
```bash
ls -la target/debug/smartkey_win.dll
```

- [ ] **Step 4: Commit**

```bash
git add crates/smartkey-win/src/dll.rs
git commit -m "feat(win): add DllGetClassObject COM DLL export"
```

---

## Chunk 3: Registration

### Task 5: Create registration module

**Files:**
- Create: `crates/smartkey-win/src/registration.rs`
- Modify: `crates/smartkey-win/src/lib.rs`

- [ ] **Step 1: Create `registration.rs`**

```rust
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
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
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
    unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )?;
    }

    // Default value = DLL path.
    set_reg_sz(hkey, None, dll_path)?;

    // ThreadingModel = Apartment (STA — required for TSF).
    set_reg_sz(hkey, Some("ThreadingModel"), "Apartment")?;

    unsafe { RegCloseKey(hkey)? };
    Ok(())
}

fn unregister_com_server() -> Result<()> {
    let subkey = format!(
        "SOFTWARE\\Classes\\CLSID\\{{{}}}",
        CLSID_SMARTKEY_STR
    );
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(subkey_w.as_ptr()));
    }
    Ok(())
}

// -- TSF TIP profile registration --------------------------------------

fn register_tip_profile(dll_path: &str) -> Result<()> {
    let profile_mgr: ITfInputProcessorProfileMgr = unsafe {
        CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?
    };

    let name_w: Vec<u16> = DISPLAY_NAME.encode_utf16().collect();
    let icon_w: Vec<u16> = dll_path.encode_utf16().collect();

    unsafe {
        profile_mgr.RegisterProfile(
            &CLSID_SMARTKEY,
            LANGID_BG,
            &GUID_PROFILE,
            &name_w,
            &icon_w,
            0,                    // icon index
            std::ptr::null(),     // no substitute HKL
            0,                    // no preferred layout
            true,                 // enable by default
            0,                    // flags
        )?;
    }

    Ok(())
}

fn unregister_tip_profile() -> Result<()> {
    let profile_mgr: ITfInputProcessorProfileMgr = unsafe {
        CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?
    };

    unsafe {
        profile_mgr.UnregisterProfile(&CLSID_SMARTKEY, LANGID_BG, &GUID_PROFILE, 0)?;
    }

    Ok(())
}

// -- TSF category registration ------------------------------------------

fn register_categories() -> Result<()> {
    let cat_mgr: ITfCategoryMgr = unsafe {
        CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?
    };

    unsafe {
        cat_mgr.RegisterCategory(
            &CLSID_SMARTKEY,
            &GUID_TFCAT_TIP_KEYBOARD,
            &CLSID_SMARTKEY,
        )?;
    }

    Ok(())
}

fn unregister_categories() -> Result<()> {
    let cat_mgr: ITfCategoryMgr = unsafe {
        CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?
    };

    unsafe {
        cat_mgr.UnregisterCategory(
            &CLSID_SMARTKEY,
            &GUID_TFCAT_TIP_KEYBOARD,
            &CLSID_SMARTKEY,
        )?;
    }

    Ok(())
}

// -- Registry helper ----------------------------------------------------

/// Write a REG_SZ value to an open registry key.
fn set_reg_sz(hkey: HKEY, name: Option<&str>, value: &str) -> Result<()> {
    let name_w: Option<Vec<u16>> = name.map(|n| {
        n.encode_utf16().chain(std::iter::once(0)).collect()
    });
    let value_w: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        RegSetValueExW(
            hkey,
            name_w.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            0,
            REG_SZ,
            Some(std::slice::from_raw_parts(
                value_w.as_ptr() as *const u8,
                value_w.len() * 2,
            )),
        )?;
    }
    Ok(())
}
```

> **API verification needed:** The exact signatures of `RegisterProfile`, `UnregisterProfile`, `RegisterCategory`, and `UnregisterCategory` in windows-rs 0.58 may differ from the plan. Check the docs at `microsoft.github.io/windows-docs-rs/`. If `CLSID_TF_InputProcessorProfiles` or `CLSID_TF_CategoryMgr` aren't available, define them manually:
> ```rust
> const CLSID_TF_InputProcessorProfiles: GUID =
>     GUID::from_u128(0x33C53A50_F456_4884_B049_85FD643ECFED);
> const CLSID_TF_CategoryMgr: GUID =
>     GUID::from_u128(0xA4B544A1_438D_4B41_9325_869523E2D6C7);
> ```

- [ ] **Step 2: Add `registration` module to `lib.rs`**

```rust
#[cfg(windows)]
pub mod registration;
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p smartkey-win
```

- [ ] **Step 4: Commit**

```bash
git add crates/smartkey-win/src/registration.rs crates/smartkey-win/src/lib.rs
git commit -m "feat(win): add TSF registration module (registry + profile + categories)"
```

---

### Task 6: Wire register.rs to registration module

**Files:**
- Modify: `crates/smartkey-win/src/register.rs`

- [ ] **Step 1: Replace TODO stubs with real registration**

```rust
// COM/TSF registration helper for SmartKey Windows IME.
//
// Usage:
//   smartkey-register.exe --install    Register as a TSF Text Input Processor
//   smartkey-register.exe --uninstall  Remove registration
//
// Requires: Administrator privileges (writes to HKLM).

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--install") => {
            println!("SmartKey IME registration");
            println!("CLSID: {{{}}}", smartkey_win::config::CLSID_SMARTKEY_STR);

            #[cfg(windows)]
            {
                let dll_path = find_dll_path();
                println!("DLL: {dll_path}");

                init_com();
                match smartkey_win::registration::register(&dll_path) {
                    Ok(()) => println!("SmartKey IME registered successfully."),
                    Err(e) => {
                        eprintln!("Registration failed: {e}");
                        uninit_com();
                        std::process::exit(1);
                    }
                }
                uninit_com();
            }

            #[cfg(not(windows))]
            println!("(dry run — not on Windows)");
        }
        Some("--uninstall") => {
            println!("Unregistering SmartKey IME...");

            #[cfg(windows)]
            {
                init_com();
                match smartkey_win::registration::unregister() {
                    Ok(()) => println!("SmartKey IME unregistered successfully."),
                    Err(e) => {
                        eprintln!("Unregistration failed: {e}");
                        uninit_com();
                        std::process::exit(1);
                    }
                }
                uninit_com();
            }

            #[cfg(not(windows))]
            println!("(dry run — not on Windows)");
        }
        _ => {
            eprintln!("Usage: smartkey-register [--install | --uninstall]");
            std::process::exit(1);
        }
    }
}

/// Find the DLL in the same directory as this executable.
#[cfg(windows)]
fn find_dll_path() -> String {
    let exe = std::env::current_exe().expect("cannot determine exe path");
    let dir = exe.parent().expect("exe has no parent directory");
    dir.join("smartkey_win.dll")
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn init_com() {
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
        .expect("COM initialization failed");
    }
}

#[cfg(windows)]
fn uninit_com() {
    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }
}
```

- [ ] **Step 2: Verify compilation of both lib and binary**

```bash
cargo check -p smartkey-win
cargo check -p smartkey-win --bin smartkey-register
```

- [ ] **Step 3: Commit**

```bash
git add crates/smartkey-win/src/register.rs
git commit -m "feat(win): wire register.exe to real TSF registration logic"
```

---

### Task 7: Add DllRegisterServer / DllUnregisterServer

**Files:**
- Modify: `crates/smartkey-win/src/dll.rs`

- [ ] **Step 1: Add self-registration exports to `dll.rs`**

```rust
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

/// Self-registration entry point (called by `regsvr32 smartkey_win.dll`).
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> windows::core::HRESULT {
    let Some(path) = dll_path() else {
        return E_FAIL;
    };

    unsafe {
        if let Err(e) = CoInitializeEx(None, COINIT_APARTMENTTHREADED) {
            return e.code();
        }
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
    unsafe {
        if let Err(e) = CoInitializeEx(None, COINIT_APARTMENTTHREADED) {
            return e.code();
        }
    }

    let result = crate::registration::unregister();

    unsafe { CoUninitialize() };

    match result {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}
```

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p smartkey-win
```

- [ ] **Step 3: Commit**

```bash
git add crates/smartkey-win/src/dll.rs
git commit -m "feat(win): add DllRegisterServer/DllUnregisterServer for regsvr32 support"
```

---

## Chunk 4: Verification & Cleanup

### Task 8: Full verification

- [ ] **Step 1: Full compilation check**

```bash
cargo check -p smartkey-win
```

- [ ] **Step 2: Clippy**

```bash
cargo clippy -p smartkey-win -- -D warnings
```

- [ ] **Step 3: Format check**

```bash
cargo fmt -p smartkey-win --check
```

- [ ] **Step 4: Core tests (no regression)**

```bash
cargo test -p smartkey-core
```

- [ ] **Step 5: Build DLL in release mode**

```bash
cargo build -p smartkey-win --lib --release
ls -la target/release/smartkey_win.dll
```

- [ ] **Step 6: Verify DLL exports**

Using `nm` (MinGW) or `dumpbin` (MSVC):
```bash
nm -D target/release/smartkey_win.dll | grep -i Dll
```

Expected exports:
- `DllMain`
- `DllGetClassObject`
- `DllCanUnloadNow`
- `DllRegisterServer`
- `DllUnregisterServer`

- [ ] **Step 7: Build register binary**

```bash
cargo build -p smartkey-win --bin smartkey-register --release
```

- [ ] **Step 8: Commit + push**

```bash
git add -A
git commit -m "feat(win): complete COM DLL phase — factory, exports, registration"
git push
```

---

## Key Implementation Details

### ManuallyDrop Pattern for COM Handoff
When returning COM pointers to Windows (DllGetClassObject, CreateInstance), use:
```rust
let iface: IClassFactory = MyFactory.into(); // ref count = 1
*ppv = ManuallyDrop::new(iface).as_raw();    // transfer ownership
```
`ManuallyDrop` prevents Rust's `Drop` (which calls `Release`). Windows now owns the reference.

### Global State Thread Safety
DLL globals use `OnceLock` and `AtomicUsize` (thread-safe) because `DllMain`, `DllCanUnloadNow`, and `DllGetClassObject` can be called from any thread. The `SmartKeyTextService` COM objects use `RefCell` (STA-safe) since TSF always calls them from the STA thread.

### COM Initialization in Registration
`registration.rs` assumes COM is already initialized. Both callers (`dll.rs::DllRegisterServer` and `register.rs`) initialize COM before calling registration functions. `CoInitializeEx` is idempotent for same-apartment calls (returns S_FALSE).

### Registry Paths
- CLSID: `HKLM\SOFTWARE\Classes\CLSID\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}\InProcServer32`
- Default value: absolute DLL path
- `ThreadingModel` = `Apartment`

### Language Configuration
Currently hardcoded to Bulgarian (`LANGID 0x0402`). Multi-language support can be added later by calling `RegisterProfile` multiple times with different LANGIDs.

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| windows-rs API signatures differ from plan | HIGH | Verify each API during implementation; check docs at microsoft.github.io/windows-docs-rs/ |
| CLSID constants not in windows-rs | MEDIUM | Define manually from known GUIDs |
| cdylib + rlib + binary conflicts | LOW | Rust handles this; DLL exports are only meaningful in cdylib |
| Registration requires admin (HKLM) | LOW | Document requirement; future: add per-user HKCU option |
| GNU linker export table issues | LOW | Verify with nm; add .def file if needed |

---

## Out of Scope (Next Phase)

- ITfDisplayAttributeProvider (ghost text grey styling)
- Per-user registration (HKCU instead of HKLM)
- Multi-language profile registration
- 32-bit build support
- Installer / MSIX packaging
- Icon resource embedding (build.rs + .rc file)
