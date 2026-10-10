// SmartKey Windows IME — Text Services Framework integration.
//
// Architecture:
//   InputMethodCore (smartkey-core) ← handles all prediction + state machine
//   This crate ← thin TSF adapter: COM interfaces → handle_key → execute Actions
//
// On non-Windows platforms this crate compiles as a no-op for workspace checks.

#[cfg(windows)]
mod class_factory;
pub mod config;
#[cfg(any(windows, test))]
mod counter;
#[cfg(windows)]
mod display;
#[cfg(windows)]
pub mod dll;
#[cfg(windows)]
mod edit_session;
#[cfg(windows)]
pub mod registration;
#[cfg(any(windows, test))]
mod text_contract;
#[cfg(windows)]
mod tsf;

pub use config::SmartKeyConfig;

#[cfg(windows)]
pub use tsf::SmartKeyTextService;
