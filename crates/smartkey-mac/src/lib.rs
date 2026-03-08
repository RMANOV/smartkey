// SmartKey macOS IME — C FFI exports for Swift IMKInputController.
//
// Architecture:
//   Swift (SmartKeyInputController) ← calls C FFI ← InputMethodCore (Rust)
//
// The Swift layer is ~100 lines: receives NSEvent, calls smartkey_handle_key(),
// interprets the returned ActionList, and calls IMK APIs (setMarkedText,
// insertText, etc.).

use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};
use std::ffi::{c_char, c_uint, CStr, CString};
use std::ptr;

/// Opaque handle to an InputMethodCore instance.
pub type SmartKeyHandle = *mut InputMethodCore;

/// A single action returned by the core.
#[repr(C)]
pub struct CAction {
    /// Action type: 0=ShowGhost, 1=HideGhost, 2=CommitText, 3=ForwardKey
    pub action_type: c_uint,
    /// Payload string (owned, caller must free with smartkey_free_string).
    /// NULL for HideGhost and ForwardKey.
    pub payload: *mut c_char,
}

/// A list of actions returned by handle_key / focus_lost / reset.
#[repr(C)]
pub struct CActionList {
    pub actions: *mut CAction,
    pub count: c_uint,
}

// ======================================================================
// Lifecycle
// ======================================================================

/// Create a new InputMethodCore. Pass NULL for default config,
/// or a JSON string for custom config.
///
/// # Safety
/// `config_json` must be NULL or a valid null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn smartkey_new(config_json: *const c_char) -> SmartKeyHandle {
    let config = if config_json.is_null() {
        InputConfig::default()
    } else {
        let c_str = unsafe { CStr::from_ptr(config_json) };
        let json_str = c_str.to_str().unwrap_or("{}");
        InputConfig::from_json(json_str)
    };
    Box::into_raw(Box::new(InputMethodCore::new(config)))
}

/// Free an InputMethodCore instance.
///
/// # Safety
/// `handle` must be a valid pointer from `smartkey_new`, or NULL.
#[no_mangle]
pub unsafe extern "C" fn smartkey_free(handle: SmartKeyHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Free a string returned by the C API.
///
/// # Safety
/// `s` must be a valid pointer from a CAction payload, or NULL.
#[no_mangle]
pub unsafe extern "C" fn smartkey_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Free an action list returned by the C API.
///
/// # Safety
/// `list` must be a valid pointer from `smartkey_handle_key` etc., or NULL.
#[no_mangle]
pub unsafe extern "C" fn smartkey_free_actions(list: *mut CActionList) {
    if list.is_null() {
        return;
    }
    let list = unsafe { Box::from_raw(list) };
    let actions =
        unsafe { Vec::from_raw_parts(list.actions, list.count as usize, list.count as usize) };
    for action in actions {
        if !action.payload.is_null() {
            drop(unsafe { CString::from_raw(action.payload) });
        }
    }
}

// ======================================================================
// Key handling
// ======================================================================

/// Process a key event. Returns an owned CActionList (caller frees).
///
/// `keycode`: macOS virtual key code.
/// `modifiers`: raw NSEvent modifier flags.
///
/// # Safety
/// `handle` must be a valid pointer from `smartkey_new`.
#[no_mangle]
pub unsafe extern "C" fn smartkey_handle_key(
    handle: SmartKeyHandle,
    keycode: c_uint,
    modifiers: c_uint,
) -> *mut CActionList {
    let core = unsafe { &mut *handle };
    let event = KeyEvent {
        key: mac_keycode_to_key(keycode),
        modifiers: mac_mods_to_modifiers(modifiers),
    };
    let actions = core.handle_key(event);
    actions_to_c(actions)
}

/// Called when input focus is lost.
///
/// # Safety
/// `handle` must be a valid pointer from `smartkey_new`.
#[no_mangle]
pub unsafe extern "C" fn smartkey_focus_lost(handle: SmartKeyHandle) -> *mut CActionList {
    let core = unsafe { &mut *handle };
    actions_to_c(core.focus_lost())
}

/// Called when input focus is gained.
///
/// # Safety
/// `handle` must be a valid pointer from `smartkey_new`.
#[no_mangle]
pub unsafe extern "C" fn smartkey_focus_gained(handle: SmartKeyHandle) {
    let core = unsafe { &mut *handle };
    core.focus_gained();
}

/// Reset the core state.
///
/// # Safety
/// `handle` must be a valid pointer from `smartkey_new`.
#[no_mangle]
pub unsafe extern "C" fn smartkey_reset(handle: SmartKeyHandle) -> *mut CActionList {
    let core = unsafe { &mut *handle };
    actions_to_c(core.reset())
}

// ======================================================================
// Corpus loading
// ======================================================================

/// Load a unigram.
///
/// # Safety
/// `handle` must be valid. `word` must be null-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn smartkey_load_word(
    handle: SmartKeyHandle,
    word: *const c_char,
    freq: c_uint,
) {
    let core = unsafe { &mut *handle };
    if let Ok(w) = unsafe { CStr::from_ptr(word) }.to_str() {
        core.load_word(w, freq);
    }
}

/// Load a bigram.
///
/// # Safety
/// `handle` must be valid. String args must be null-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn smartkey_load_bigram(
    handle: SmartKeyHandle,
    ctx: *const c_char,
    word: *const c_char,
    count: c_uint,
) {
    let core = unsafe { &mut *handle };
    let ctx_s = unsafe { CStr::from_ptr(ctx) }.to_str().unwrap_or("");
    let word_s = unsafe { CStr::from_ptr(word) }.to_str().unwrap_or("");
    core.load_bigram(ctx_s, word_s, count);
}

// ======================================================================
// Helpers
// ======================================================================

/// Convert macOS virtual key code to platform-neutral Key.
fn mac_keycode_to_key(keycode: c_uint) -> Key {
    match keycode {
        0x30 => Key::Tab,       // kVK_Tab
        0x35 => Key::Escape,    // kVK_Escape
        0x7C => Key::Right,     // kVK_RightArrow
        0x33 => Key::Backspace, // kVK_Delete
        0x31 => Key::Space,     // kVK_Space
        0x24 => Key::Return,    // kVK_Return
        // Letters: kVK_ANSI_A (0x00) through kVK_ANSI_Z
        k if k <= 0x2F => {
            // Approximate: macOS keycodes aren't sequential like ASCII.
            // In production, use UCKeyTranslate for accurate mapping.
            // For the scaffold, map the most common keys.
            let ch = match k {
                0x00 => 'a',
                0x0B => 'b',
                0x08 => 'c',
                0x02 => 'd',
                0x0E => 'e',
                0x03 => 'f',
                0x05 => 'g',
                0x04 => 'h',
                0x22 => 'i',
                0x26 => 'j',
                0x28 => 'k',
                0x25 => 'l',
                0x2E => 'm',
                0x2D => 'n',
                0x1F => 'o',
                0x23 => 'p',
                0x0C => 'q',
                0x0F => 'r',
                0x01 => 's',
                0x11 => 't',
                0x20 => 'u',
                0x09 => 'v',
                0x0D => 'w',
                0x07 => 'x',
                0x10 => 'y',
                0x06 => 'z',
                _ => return Key::Other(k),
            };
            Key::Char(ch)
        }
        other => Key::Other(other),
    }
}

/// Convert macOS NSEvent modifier flags to Modifiers.
fn mac_mods_to_modifiers(flags: c_uint) -> Modifiers {
    let mut m = Modifiers::empty();
    if flags & (1 << 18) != 0 {
        m |= Modifiers::CTRL;
    } // NSEventModifierFlagControl
    if flags & (1 << 19) != 0 {
        m |= Modifiers::ALT;
    } // NSEventModifierFlagOption
    if flags & (1 << 20) != 0 {
        m |= Modifiers::SUPER;
    } // NSEventModifierFlagCommand
    if flags & (1 << 17) != 0 {
        m |= Modifiers::SHIFT;
    } // NSEventModifierFlagShift
    m
}

/// Convert a Vec<Action> into a heap-allocated CActionList.
fn actions_to_c(actions: Vec<Action>) -> *mut CActionList {
    let c_actions: Vec<CAction> = actions
        .into_iter()
        .map(|a| match a {
            Action::ShowGhost(text) => CAction {
                action_type: 0,
                payload: CString::new(text).unwrap_or_default().into_raw(),
            },
            Action::HideGhost => CAction {
                action_type: 1,
                payload: ptr::null_mut(),
            },
            Action::CommitText(text) => CAction {
                action_type: 2,
                payload: CString::new(text).unwrap_or_default().into_raw(),
            },
            Action::ForwardKey => CAction {
                action_type: 3,
                payload: ptr::null_mut(),
            },
        })
        .collect();

    let count = c_actions.len() as c_uint;
    let mut boxed = c_actions.into_boxed_slice();
    let actions_ptr = boxed.as_mut_ptr();
    std::mem::forget(boxed);

    Box::into_raw(Box::new(CActionList {
        actions: actions_ptr,
        count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mac_keycode_mapping() {
        assert_eq!(mac_keycode_to_key(0x30), Key::Tab);
        assert_eq!(mac_keycode_to_key(0x35), Key::Escape);
        assert_eq!(mac_keycode_to_key(0x00), Key::Char('a'));
        assert_eq!(mac_keycode_to_key(0x04), Key::Char('h'));
    }

    #[test]
    fn test_roundtrip_lifecycle() {
        unsafe {
            let handle = smartkey_new(ptr::null());
            assert!(!handle.is_null());

            let list = smartkey_handle_key(handle, 0x04, 0); // 'h'
            assert!(!list.is_null());
            smartkey_free_actions(list);

            smartkey_free(handle);
        }
    }
}
