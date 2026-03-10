// TSF Text Input Processor implementation.
//
// Implements the COM interfaces required for a Windows IME:
//   - ITfTextInputProcessorEx: lifecycle (Activate/Deactivate)
//   - ITfKeyEventSink: key event handling
//   - ITfCompositionSink: composition lifecycle
//   - ITfDisplayAttributeProvider: ghost text styling
//
// Key flow:
//   OnKeyDown → KeyEvent → InputMethodCore::handle_key() → Vec<Action>
//   Action::ShowGhost → create/update TSF composition (grey display attribute)
//   Action::HideGhost → end composition
//   Action::CommitText → end composition + insert finalized text
//   Action::ForwardKey → return S_FALSE (don't consume)

use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::TextServices::*;

/// The SmartKey TSF Text Input Processor.
///
/// Wraps `InputMethodCore` and implements COM interfaces for Windows TSF.
#[windows::core::implement(ITfTextInputProcessor, ITfTextInputProcessorEx, ITfKeyEventSink)]
pub struct SmartKeyTextService {
    core: std::cell::RefCell<InputMethodCore>,
    thread_mgr: std::cell::RefCell<Option<ITfThreadMgr>>,
    client_id: std::cell::Cell<u32>,
}

impl Default for SmartKeyTextService {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartKeyTextService {
    pub fn new() -> Self {
        Self {
            core: std::cell::RefCell::new(InputMethodCore::new(InputConfig::default())),
            thread_mgr: std::cell::RefCell::new(None),
            client_id: std::cell::Cell::new(0),
        }
    }

    /// Translate a Windows virtual key code to our platform-neutral Key.
    fn vk_to_key(vk: u32) -> Key {
        match VIRTUAL_KEY(vk as u16) {
            VK_TAB => Key::Tab,
            VK_ESCAPE => Key::Escape,
            VK_RIGHT => Key::Right,
            VK_BACK => Key::Backspace,
            VK_SPACE => Key::Space,
            VK_RETURN => Key::Return,
            _ => {
                // Try to map to a character via the virtual key.
                // For A-Z keys (0x41-0x5A), produce lowercase.
                if (0x41..=0x5A).contains(&vk) {
                    Key::Char((vk as u8 + 32) as char) // lowercase
                } else if (0x30..=0x39).contains(&vk) {
                    Key::Char(vk as u8 as char) // digits
                } else {
                    Key::Other(vk)
                }
            }
        }
    }

    /// Build Modifiers from the current keyboard state.
    fn get_modifiers() -> Modifiers {
        let mut mods = Modifiers::empty();
        unsafe {
            if GetKeyState(VK_CONTROL.0 as i32) < 0 {
                mods |= Modifiers::CTRL;
            }
            if GetKeyState(VK_MENU.0 as i32) < 0 {
                mods |= Modifiers::ALT;
            }
            if GetKeyState(VK_LWIN.0 as i32) < 0 || GetKeyState(VK_RWIN.0 as i32) < 0 {
                mods |= Modifiers::SUPER;
            }
            if GetKeyState(VK_SHIFT.0 as i32) < 0 {
                mods |= Modifiers::SHIFT;
            }
        }
        mods
    }

    /// Execute actions returned by InputMethodCore.
    fn execute_actions(&self, actions: &[Action], _context: &ITfContext) -> bool {
        let mut consumed = true;
        for action in actions {
            match action {
                Action::ShowGhost(_text) => {
                    // TODO: Create TSF composition for ghost text
                    consumed = false;
                }
                Action::HideGhost => {
                    // TODO: End any active TSF composition.
                }
                Action::CommitText(_text) => {
                    // TODO: Insert text via ITfInsertAtSelection
                    consumed = false;
                }
                Action::ForwardKey => {
                    consumed = false;
                }
            }
        }
        consumed
    }
}

// -- ITfTextInputProcessor implementation (base trait: Activate + Deactivate) --

impl ITfTextInputProcessor_Impl for SmartKeyTextService_Impl {
    /// Base-interface fallback (Win 7/8). Keep in sync with ActivateEx.
    fn Activate(&self, ptim: Option<&ITfThreadMgr>, tid: u32) -> Result<()> {
        self.client_id.set(tid);
        *self.thread_mgr.borrow_mut() = ptim.cloned();
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        // TODO: Unadvise key event sink
        *self.thread_mgr.borrow_mut() = None;
        Ok(())
    }
}

// -- ITfTextInputProcessorEx implementation (extended: ActivateEx) --

impl ITfTextInputProcessorEx_Impl for SmartKeyTextService_Impl {
    fn ActivateEx(&self, ptim: Option<&ITfThreadMgr>, tid: u32, _flags: u32) -> Result<()> {
        self.client_id.set(tid);
        *self.thread_mgr.borrow_mut() = ptim.cloned();

        // TODO: Install key event sink via ITfKeystrokeMgr::AdviseKeyEventSink
        // TODO: Load corpus files from SmartKeyConfig::load().corpus_files

        Ok(())
    }
}

// -- ITfKeyEventSink implementation --

impl ITfKeyEventSink_Impl for SmartKeyTextService_Impl {
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        Ok(())
    }

    fn OnTestKeyDown(
        &self,
        _pic: Option<&ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        let key = SmartKeyTextService::vk_to_key(wparam.0 as u32);
        let dominated = !matches!(key, Key::Other(_));
        Ok(BOOL::from(dominated))
    }

    fn OnTestKeyUp(
        &self,
        _pic: Option<&ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        Ok(BOOL::from(false))
    }

    fn OnKeyDown(&self, pic: Option<&ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = wparam.0 as u32;
        let key = SmartKeyTextService::vk_to_key(vk);
        let mods = SmartKeyTextService::get_modifiers();
        let event = KeyEvent {
            key,
            modifiers: mods,
        };

        let actions = self.core.borrow_mut().handle_key(event);

        let consumed = if let Some(ctx) = pic {
            self.execute_actions(&actions, ctx)
        } else {
            false
        };

        Ok(BOOL::from(consumed))
    }

    fn OnKeyUp(&self, _pic: Option<&ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL::from(false))
    }

    fn OnPreservedKey(&self, _pic: Option<&ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(BOOL::from(false))
    }
}
