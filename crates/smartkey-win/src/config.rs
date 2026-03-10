// Windows-specific configuration paths.
//
// Config dir: %APPDATA%\smartkey\ (e.g. C:\Users\X\AppData\Roaming\smartkey\)

use smartkey_core::paths;
use std::path::PathBuf;

/// SmartKey Windows configuration.
pub struct SmartKeyConfig {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub corpus_files: Vec<PathBuf>,
}

impl SmartKeyConfig {
    /// Load configuration using platform-aware paths from smartkey-core.
    pub fn load() -> Self {
        Self {
            config_dir: paths::config_dir(),
            config_file: paths::config_file(),
            corpus_files: paths::corpus_files(),
        }
    }
}

// COM class GUID for SmartKey TSF Text Input Processor.
// Generated once, never changes — used for registration and activation.
pub const CLSID_SMARTKEY_STR: &str = "7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C";

// Language profile GUID.
pub const GUID_PROFILE_STR: &str = "8B4C0F2E-5D3A-4F6B-9E7C-2E3F4A5B6C7D";

/// COM CLSID as typed GUID for Windows API calls.
#[cfg(windows)]
pub const CLSID_SMARTKEY: windows::core::GUID =
    windows::core::GUID::from_u128(0x7A3B9E1F_4C2D_4E5A_8F6B_1D2E3F4A5B6C);

/// Language profile as typed GUID for Windows API calls.
#[cfg(windows)]
pub const GUID_PROFILE: windows::core::GUID =
    windows::core::GUID::from_u128(0x8B4C0F2E_5D3A_4F6B_9E7C_2E3F4A5B6C7D);
