// TSF Edit Session — executes text operations within a granted edit cookie.
//
// TSF requires all text modifications to go through ITfEditSession::DoEditSession().
// The framework calls back with an `ec` (edit cookie) that grants write access.
//
// Architecture: Single SmartKeyEditSession with EditOp enum instead of 3 COM classes.
// Rationale: 3 operations don't justify 3 COM classes; one struct reduces COM lifetime bugs.

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::UI::TextServices::*;

/// The operation to perform inside the edit session.
pub enum EditOp {
    /// Display ghost text as a TSF composition.
    ShowGhost {
        text: String,
        composition: Rc<RefCell<Option<ITfComposition>>>,
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
            EditOp::ShowGhost { text, composition } => self.do_show_ghost(ec, text, composition),
            EditOp::HideGhost { composition } => self.do_hide_ghost(ec, composition),
            EditOp::CommitText { text, composition } => self.do_commit_text(ec, text, composition),
        }
    }
}

impl SmartKeyEditSession_Impl {
    /// Show or update ghost text via a TSF composition.
    fn do_show_ghost(
        &self,
        ec: u32,
        text: &str,
        composition: &Rc<RefCell<Option<ITfComposition>>>,
    ) -> Result<()> {
        let text_utf16: Vec<u16> = text.encode_utf16().collect();
        let mut comp = composition.borrow_mut();

        if let Some(ref active) = *comp {
            // Update existing composition range.
            unsafe {
                let range = active.GetRange()?;
                range.SetText(ec, 0, &text_utf16)?;
            }
        } else {
            // Start a new composition.
            unsafe {
                let insert: ITfInsertAtSelection = self.context.cast()?;
                let range = insert.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &text_utf16)?;

                let ctx_comp: ITfContextComposition = self.context.cast()?;
                // StartComposition needs a composition sink. We pass None since
                // SmartKeyTextService implements ITfCompositionSink separately
                // and handles OnCompositionTerminated via its own impl.
                let new_comp = ctx_comp.StartComposition(ec, &range, None)?;
                *comp = Some(new_comp);
            }
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

    /// Commit text: end any ghost composition, then insert finalized text.
    fn do_commit_text(
        &self,
        ec: u32,
        text: &str,
        composition: &Rc<RefCell<Option<ITfComposition>>>,
    ) -> Result<()> {
        let text_utf16: Vec<u16> = text.encode_utf16().collect();

        // End ghost composition first — ghost text becomes permanent.
        let mut comp = composition.borrow_mut();
        if let Some(active) = comp.take() {
            unsafe {
                // Replace the ghost text with the committed text in-place.
                let range = active.GetRange()?;
                range.SetText(ec, 0, &text_utf16)?;
                active.EndComposition(ec)?;
            }
        } else {
            // No active composition — insert at the current selection.
            unsafe {
                let insert: ITfInsertAtSelection = self.context.cast()?;
                insert.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &text_utf16)?;
            }
        }
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
