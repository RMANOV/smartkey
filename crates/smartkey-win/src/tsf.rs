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

use std::cell::RefCell;
use std::rc::Rc;

use smartkey_core::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
use smartkey_core::MasterLoop;

use crate::dll::OBJECT_COUNT;

use crate::config::SmartKeyConfig;
use crate::display::{GhostAttributeInfo, SingleItemEnum, GUID_GHOST_ATTR};
use crate::edit_session::{self, EditOp};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::TextServices::*;

/// The SmartKey TSF Text Input Processor.
///
/// Wraps `InputMethodCore` and implements COM interfaces for Windows TSF.
#[windows::core::implement(
    ITfTextInputProcessor,
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider
)]
pub struct SmartKeyTextService {
    core: RefCell<MasterLoop>,
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    client_id: std::cell::Cell<u32>,
    /// Active ghost text composition. Shared with edit sessions via Rc.
    /// Safe because TSF is STA COM (single-threaded apartment).
    composition: Rc<RefCell<Option<ITfComposition>>>,
    /// Guard against duplicate corpus loading on reactivation.
    corpus_loaded: std::cell::Cell<bool>,
    /// TfGuidAtom for GUID_GHOST_ATTR, obtained from ITfCategoryMgr::RegisterGUID.
    ghost_attr_atom: std::cell::Cell<u32>,
}

impl Default for SmartKeyTextService {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartKeyTextService {
    pub fn new() -> Self {
        Self {
            core: RefCell::new(MasterLoop::new(InputConfig::default())),
            thread_mgr: RefCell::new(None),
            client_id: std::cell::Cell::new(0),
            composition: Rc::new(RefCell::new(None)),
            corpus_loaded: std::cell::Cell::new(false),
            ghost_attr_atom: std::cell::Cell::new(0),
        }
    }

    /// Translate a Windows virtual key code to our platform-neutral Key.
    ///
    /// Uses `ToUnicodeEx` for layout-aware character resolution — supports
    /// Shift, Cyrillic, and any keyboard layout installed on the system.
    fn vk_to_key(vk: u32, scan_code: u32) -> Key {
        // Special keys first (layout-independent).
        match VIRTUAL_KEY(vk as u16) {
            VK_TAB => return Key::Tab,
            VK_ESCAPE => return Key::Escape,
            VK_RIGHT => return Key::Right,
            VK_LEFT => return Key::Left,
            VK_UP => return Key::Up,
            VK_DOWN => return Key::Down,
            VK_HOME => return Key::Home,
            VK_END => return Key::End,
            VK_PRIOR => return Key::PageUp,
            VK_NEXT => return Key::PageDown,
            VK_BACK => return Key::Backspace,
            VK_SPACE => return Key::Space,
            VK_RETURN => return Key::Return,
            _ => {}
        }
        // Use ToUnicodeEx for layout-aware character resolution.
        unsafe {
            let mut keyboard_state = [0u8; 256];
            let _ = GetKeyboardState(&mut keyboard_state);
            let layout = GetKeyboardLayout(0);
            let mut buf = [0u16; 4];
            // Flag 4 = do not modify the dead-key composition buffer.
            // Without this, calling ToUnicodeEx from OnTestKeyDown would consume
            // pending dead-key state (e.g. ^ + e → ê), breaking compose sequences.
            let result = ToUnicodeEx(vk, scan_code, &keyboard_state, &mut buf, 4, layout);
            if result >= 1 {
                if let Some(ch) = char::decode_utf16(buf[..result as usize].iter().copied())
                    .next()
                    .and_then(|r| r.ok())
                {
                    if !ch.is_control() {
                        return Key::Char(ch);
                    }
                }
            }
        }
        Key::Other(vk)
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
    fn execute_actions(&self, actions: &[Action], context: &ITfContext) -> bool {
        let consumed = crate::text_contract::consumes_key(actions);
        let cid = self.client_id.get();
        for action in actions {
            let op = match action {
                Action::ForwardKey => continue,
                Action::ShowGhost(text) | Action::ShowComposing { typed: text, .. } => {
                    let sink: Result<ITfCompositionSink> = unsafe { self.cast() };
                    let comp_sink = match sink {
                        Ok(sink) => sink,
                        Err(error) => {
                            log::error!("smartkey: composition sink failed: {error}");
                            let _ = self.core.borrow_mut().reset();
                            return true;
                        }
                    };
                    let (full, typed_units) = match action {
                        Action::ShowComposing { typed, ghost } => {
                            (format!("{typed}{ghost}"), typed.encode_utf16().count())
                        }
                        _ => (text.clone(), 0),
                    };
                    EditOp::ShowGhost {
                        text: full,
                        typed_units,
                        composition: self.composition.clone(),
                        comp_sink,
                        ghost_attr_atom: self.ghost_attr_atom.get(),
                    }
                }
                Action::HideGhost => EditOp::HideGhost {
                    composition: self.composition.clone(),
                },
                Action::CommitText(text) => EditOp::CommitText {
                    text: text.clone(),
                    composition: self.composition.clone(),
                },
                Action::ReplaceWord { replace_len, text } => EditOp::ReplaceWord {
                    replace_len: *replace_len,
                    text: text.clone(),
                    composition: self.composition.clone(),
                },
            };
            if let Err(error) = edit_session::request_edit_session(context, cid, op) {
                log::error!("smartkey: text edit failed; stopping dependent actions: {error}");
                let _ = self.core.borrow_mut().reset();
                // Some edits may already have succeeded. Replaying the raw key can
                // duplicate committed text, so do not forward a partially applied batch.
                return true;
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
        // Unadvise key event sink.
        if let Some(ref mgr) = *self.thread_mgr.borrow() {
            let keystroke_mgr: Result<ITfKeystrokeMgr> = mgr.cast();
            if let Ok(km) = keystroke_mgr {
                let _ = unsafe { km.UnadviseKeyEventSink(self.client_id.get()) };
            }
        }

        // Clear composition state.
        *self.composition.borrow_mut() = None;
        *self.thread_mgr.borrow_mut() = None;

        // Track live object count for DllCanUnloadNow.
        crate::counter::saturating_decrement(&OBJECT_COUNT);

        Ok(())
    }
}

// -- ITfTextInputProcessorEx implementation (extended: ActivateEx) --

impl ITfTextInputProcessorEx_Impl for SmartKeyTextService_Impl {
    fn ActivateEx(&self, ptim: Option<&ITfThreadMgr>, tid: u32, _flags: u32) -> Result<()> {
        self.client_id.set(tid);
        *self.thread_mgr.borrow_mut() = ptim.cloned();

        // Install key event sink so we receive OnKeyDown/OnKeyUp callbacks.
        if let Some(ref mgr) = *self.thread_mgr.borrow() {
            let keystroke_mgr: ITfKeystrokeMgr = mgr.cast()?;
            // Get ITfKeyEventSink interface from ourselves.
            let sink: ITfKeyEventSink = unsafe { self.cast()? };
            unsafe {
                keystroke_mgr.AdviseKeyEventSink(tid, &sink, true)?;
            }
        }

        // Load config + corpus files once per instance (guard against reactivation).
        if !self.corpus_loaded.get() {
            let win_config = SmartKeyConfig::load();

            // Read user config JSON and build InputConfig (or fall back to defaults).
            let input_config = if win_config.config_file.is_file() {
                match std::fs::read_to_string(&win_config.config_file) {
                    Ok(json) => InputConfig::from_json(&json),
                    Err(e) => {
                        log::warn!("smartkey: config read error: {e}");
                        InputConfig::default()
                    }
                }
            } else {
                InputConfig::default()
            };
            *self.core.borrow_mut() = MasterLoop::new(input_config);

            // Register ghost attribute GUID → get TfGuidAtom for SetValue.
            let cat_mgr: std::result::Result<ITfCategoryMgr, _> =
                unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) };
            match cat_mgr {
                Ok(mgr) => match unsafe { mgr.RegisterGUID(&GUID_GHOST_ATTR) } {
                    Ok(atom) => self.ghost_attr_atom.set(atom),
                    Err(e) => log::error!("smartkey: RegisterGUID failed: {e}"),
                },
                Err(e) => log::error!("smartkey: ITfCategoryMgr creation failed: {e}"),
            }

            let mut core = self.core.borrow_mut();
            for path in &win_config.corpus_files {
                if let Err(e) = core.load_corpus_file(path) {
                    log::error!("smartkey: failed to load corpus {}: {e}", path.display());
                }
            }
            if let Err(e) = core.load_personal_default() {
                log::warn!("smartkey: failed to load personal profile: {e}");
            }
            self.corpus_loaded.set(true);
        }

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
        lparam: LPARAM,
    ) -> Result<BOOL> {
        let scan_code = ((lparam.0 >> 16) & 0xFF) as u32;
        let key = SmartKeyTextService::vk_to_key(wparam.0 as u32, scan_code);
        let core = self.core.borrow();

        let pending = core.has_ghost()
            || !core.current_word().is_empty()
            || self.composition.borrow().is_some();
        let should_claim = crate::text_contract::claims_key(
            &key,
            SmartKeyTextService::get_modifiers(),
            core.is_enabled(),
            pending,
        );
        Ok(BOOL::from(should_claim))
    }

    fn OnTestKeyUp(
        &self,
        _pic: Option<&ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        Ok(BOOL::from(false))
    }

    fn OnKeyDown(&self, pic: Option<&ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let vk = wparam.0 as u32;
        let scan_code = ((lparam.0 >> 16) & 0xFF) as u32;
        let key = SmartKeyTextService::vk_to_key(vk, scan_code);
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

// -- ITfDisplayAttributeProvider implementation --

impl ITfDisplayAttributeProvider_Impl for SmartKeyTextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(SingleItemEnum::new().into())
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        if guid.is_null() || unsafe { *guid } != GUID_GHOST_ATTR {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        Ok(GhostAttributeInfo.into())
    }
}

// -- ITfCompositionSink implementation --

impl ITfCompositionSink_Impl for SmartKeyTextService_Impl {
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        pcomposition: Option<&ITfComposition>,
    ) -> Result<()> {
        let matches_active = {
            let active = self.composition.borrow();
            active
                .as_ref()
                .is_some_and(|current| Some(current) == pcomposition)
        };
        if matches_active {
            self.composition.borrow_mut().take();
            // An external termination finalized the range; discard stale predictions.
            // Own Hide/Commit removes the slot before its termination callback.
            let _ = self.core.borrow_mut().reset();
        }
        Ok(())
    }
}
