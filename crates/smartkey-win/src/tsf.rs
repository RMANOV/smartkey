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
use std::sync::atomic::Ordering;

use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};

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
    core: RefCell<InputMethodCore>,
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
            core: RefCell::new(InputMethodCore::new(InputConfig::default())),
            thread_mgr: RefCell::new(None),
            client_id: std::cell::Cell::new(0),
            composition: Rc::new(RefCell::new(None)),
            corpus_loaded: std::cell::Cell::new(false),
            ghost_attr_atom: std::cell::Cell::new(0),
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
                // For A-Z keys (0x41-0x5A), produce lowercase.
                if (0x41..=0x5A).contains(&vk) {
                    Key::Char((vk as u8 + 32) as char)
                } else if (0x30..=0x39).contains(&vk) {
                    Key::Char(vk as u8 as char)
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
    fn execute_actions(&self, actions: &[Action], context: &ITfContext) -> bool {
        let mut consumed = false;
        let cid = self.client_id.get();

        for action in actions {
            match action {
                Action::ShowGhost(text) => {
                    let sink: Result<ITfCompositionSink> = unsafe { self.cast() };
                    match sink {
                        Ok(comp_sink) => {
                            let op = EditOp::ShowGhost {
                                text: text.clone(),
                                composition: self.composition.clone(),
                                comp_sink,
                                ghost_attr_atom: self.ghost_attr_atom.get(),
                            };
                            if let Err(e) = edit_session::request_edit_session(context, cid, op) {
                                eprintln!("smartkey: ShowGhost failed: {e}");
                            }
                        }
                        Err(e) => eprintln!("smartkey: failed to get composition sink: {e}"),
                    }
                    consumed = true;
                }
                Action::HideGhost => {
                    let op = EditOp::HideGhost {
                        composition: self.composition.clone(),
                    };
                    if let Err(e) = edit_session::request_edit_session(context, cid, op) {
                        eprintln!("smartkey: HideGhost failed: {e}");
                    }
                    consumed = true;
                }
                Action::CommitText(text) => {
                    let op = EditOp::CommitText {
                        text: text.clone(),
                        composition: self.composition.clone(),
                    };
                    if let Err(e) = edit_session::request_edit_session(context, cid, op) {
                        eprintln!("smartkey: CommitText failed: {e}");
                    }
                    consumed = true;
                }
                Action::ForwardKey => {
                    // Key must reach the application — override any prior consumption.
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
        OBJECT_COUNT
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            })
            .ok();

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
                        eprintln!("smartkey: config read error: {e}");
                        InputConfig::default()
                    }
                }
            } else {
                InputConfig::default()
            };
            *self.core.borrow_mut() = InputMethodCore::new(input_config);

            // Register ghost attribute GUID → get TfGuidAtom for SetValue.
            let cat_mgr: std::result::Result<ITfCategoryMgr, _> =
                unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) };
            match cat_mgr {
                Ok(mgr) => match unsafe { mgr.RegisterGUID(&GUID_GHOST_ATTR) } {
                    Ok(atom) => self.ghost_attr_atom.set(atom),
                    Err(e) => eprintln!("smartkey: RegisterGUID failed: {e}"),
                },
                Err(e) => eprintln!("smartkey: ITfCategoryMgr creation failed: {e}"),
            }

            let mut core = self.core.borrow_mut();
            for path in &win_config.corpus_files {
                if let Err(e) = core.load_corpus_file(path) {
                    eprintln!("smartkey: failed to load corpus {}: {e}", path.display());
                }
            }
            if let Err(e) = core.load_personal_default() {
                eprintln!("smartkey: failed to load personal profile: {e}");
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
        _pcomposition: Option<&ITfComposition>,
    ) -> Result<()> {
        // External termination (e.g. application or another TIP ended our composition).
        // Clean up our state to stay in sync.
        *self.composition.borrow_mut() = None;
        Ok(())
    }
}
