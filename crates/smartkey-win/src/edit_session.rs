// TSF Edit Session — executes text operations within a granted edit cookie.
//
// TSF requires all text modifications to go through ITfEditSession::DoEditSession().
// The framework calls back with an `ec` (edit cookie) that grants write access.
//
// Architecture: Single SmartKeyEditSession with EditOp enum instead of 3 COM classes.
// Rationale: 3 operations don't justify 3 COM classes; one struct reduces COM lifetime bugs.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::TextServices::*;

/// The operation to perform inside the edit session.
pub enum EditOp {
    /// Display ghost text as a TSF composition.
    ShowGhost {
        text: String,
        composition: Rc<RefCell<Option<ITfComposition>>>,
        /// Composition sink to receive OnCompositionTerminated callbacks.
        comp_sink: ITfCompositionSink,
        /// TfGuidAtom for ghost display attribute (grey styling). 0 = no styling.
        ghost_attr_atom: u32,
    },
    /// Remove ghost text and end the composition.
    HideGhost {
        composition: Rc<RefCell<Option<ITfComposition>>>,
    },
    /// Commit (finalize) text into the document.
    CommitText {
        text: String,
        composition: Rc<RefCell<Option<ITfComposition>>>,
    },
}

/// A one-shot edit session that executes a single EditOp.
#[implement(ITfEditSession)]
pub struct SmartKeyEditSession {
    context: ITfContext,
    #[allow(dead_code)] // reserved for display attribute operations
    client_id: u32,
    op: EditOp,
}

impl SmartKeyEditSession {
    pub fn new(context: ITfContext, client_id: u32, op: EditOp) -> Self {
        Self {
            context,
            client_id,
            op,
        }
    }
}

impl ITfEditSession_Impl for SmartKeyEditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        match &self.op {
            EditOp::ShowGhost {
                text,
                composition,
                comp_sink,
                ghost_attr_atom,
            } => self.do_show_ghost(ec, text, composition, comp_sink, *ghost_attr_atom),
            EditOp::HideGhost { composition } => self.do_hide_ghost(ec, composition),
            EditOp::CommitText { text, composition } => self.do_commit_text(ec, text, composition),
        }
    }
}

impl SmartKeyEditSession_Impl {
    /// Show or update ghost text via a TSF composition.
    ///
    /// After inserting/updating ghost text, the selection (caret) is moved to the
    /// START of the ghost range so that forwarded keys are inserted before the ghost.
    fn do_show_ghost(
        &self,
        ec: u32,
        text: &str,
        composition: &Rc<RefCell<Option<ITfComposition>>>,
        comp_sink: &ITfCompositionSink,
        ghost_attr_atom: u32,
    ) -> Result<()> {
        let text_utf16: Vec<u16> = text.encode_utf16().collect();
        let mut comp = composition.borrow_mut();

        if let Some(ref active) = *comp {
            // Update existing composition range.
            unsafe {
                let range = active.GetRange()?;
                range.SetText(ec, 0, &text_utf16)?;
                // Re-apply attribute after SetText (SetText clears properties).
                self.apply_ghost_attr(ec, &range, ghost_attr_atom)?;
                self.set_caret_to_range_start(ec, &range)?;
            }
        } else {
            // Start a new composition.
            unsafe {
                let insert: ITfInsertAtSelection = self.context.cast()?;
                let range = insert.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &text_utf16)?;

                let ctx_comp: ITfContextComposition = self.context.cast()?;
                let new_comp = ctx_comp.StartComposition(ec, &range, Some(comp_sink))?;
                *comp = Some(new_comp);

                self.apply_ghost_attr(ec, &range, ghost_attr_atom)?;
                self.set_caret_to_range_start(ec, &range)?;
            }
        }
        Ok(())
    }

    /// Set GUID_PROP_ATTRIBUTE on a range to apply ghost text styling (grey).
    ///
    /// The atom is a TfGuidAtom obtained from ITfCategoryMgr::RegisterGUID.
    /// TSF resolves it back through our ITfDisplayAttributeProvider to get
    /// the actual TF_DISPLAYATTRIBUTE (grey foreground).
    fn apply_ghost_attr(&self, ec: u32, range: &ITfRange, atom: u32) -> Result<()> {
        if atom == 0 {
            return Ok(());
        }
        unsafe {
            let prop = self.context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
            let variant = VARIANT::from(atom as i32);
            prop.SetValue(ec, range, &variant)?;
        }
        Ok(())
    }

    /// Remove ghost text and end the active composition.
    fn do_hide_ghost(
        &self,
        ec: u32,
        composition: &Rc<RefCell<Option<ITfComposition>>>,
    ) -> Result<()> {
        let mut comp = composition.borrow_mut();
        if let Some(active) = comp.take() {
            unsafe {
                // Clear the composition text before ending.
                let range = active.GetRange()?;
                range.SetText(ec, 0, &[])?;
                active.EndComposition(ec)?;
            }
        }
        Ok(())
    }

    /// Commit text: end the ghost composition so the suffix becomes permanent.
    ///
    /// When a ghost composition is active, the typed prefix is already in the
    /// document (via ForwardKey) and the ghost suffix is in the composition range.
    /// EndComposition makes the suffix permanent — no text replacement needed.
    /// The `text` parameter is only used when there's no active composition
    /// (e.g. direct commit without prior ghost).
    fn do_commit_text(
        &self,
        ec: u32,
        text: &str,
        composition: &Rc<RefCell<Option<ITfComposition>>>,
    ) -> Result<()> {
        let mut comp = composition.borrow_mut();
        if let Some(active) = comp.take() {
            unsafe {
                // Ghost suffix is already in the document. Just finalize it.
                active.EndComposition(ec)?;
            }
        } else {
            // No active composition — insert the full text at selection.
            let text_utf16: Vec<u16> = text.encode_utf16().collect();
            unsafe {
                let insert: ITfInsertAtSelection = self.context.cast()?;
                insert.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &text_utf16)?;
            }
        }
        Ok(())
    }

    /// Move the caret (selection) to the start of the given range.
    ///
    /// This ensures forwarded keys are inserted before the ghost text, not after.
    unsafe fn set_caret_to_range_start(&self, ec: u32, range: &ITfRange) -> Result<()> {
        let caret = range.Clone()?;
        caret.Collapse(ec, TF_ANCHOR_START)?;

        let selection = TF_SELECTION {
            range: ManuallyDrop::new(Some(caret)),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_END,
                fInterimChar: BOOL(0),
            },
        };
        self.context.SetSelection(ec, &[selection])?;
        Ok(())
    }
}

// -- Public helper to request an edit session from outside ---------------

/// Request a synchronous edit session on the given context.
///
/// Creates a `SmartKeyEditSession` with the specified operation and asks TSF
/// to execute it. Returns Ok(()) if the session completed successfully.
///
/// # Safety
/// Must be called from a TSF key event handler (OnKeyDown) for TF_ES_SYNC
/// to succeed — TSF only grants synchronous sessions during key callbacks.
pub fn request_edit_session(context: &ITfContext, client_id: u32, op: EditOp) -> Result<()> {
    let session: ITfEditSession = SmartKeyEditSession::new(context.clone(), client_id, op).into();

    let hr =
        unsafe { context.RequestEditSession(client_id, &session, TF_ES_READWRITE | TF_ES_SYNC)? };

    hr.ok()
}
