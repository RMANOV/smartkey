// Text operations run only inside the edit cookie granted by TSF.
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::TextServices::*;

pub enum EditOp {
    ShowGhost {
        text: String,
        /// Zero for a suffix-only ghost; typed-prefix UTF-16 length for preedit.
        typed_units: usize,
        composition: Rc<RefCell<Option<ITfComposition>>>,
        comp_sink: ITfCompositionSink,
        ghost_attr_atom: u32,
    },
    HideGhost {
        composition: Rc<RefCell<Option<ITfComposition>>>,
    },
    CommitText {
        text: String,
        composition: Rc<RefCell<Option<ITfComposition>>>,
    },
    ReplaceWord {
        replace_len: usize,
        text: String,
        composition: Rc<RefCell<Option<ITfComposition>>>,
    },
}

#[implement(ITfEditSession)]
pub struct SmartKeyEditSession {
    context: ITfContext,
    op: EditOp,
}

impl ITfEditSession_Impl for SmartKeyEditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        match &self.op {
            EditOp::ShowGhost {
                text,
                typed_units,
                composition,
                comp_sink,
                ghost_attr_atom,
            } => self.show(
                ec,
                text,
                *typed_units,
                composition,
                comp_sink,
                *ghost_attr_atom,
            ),
            EditOp::HideGhost { composition } => self.hide(ec, composition),
            EditOp::CommitText { text, composition } => {
                // CommitText is an exact insertion, including partial Right acceptance.
                self.hide(ec, composition)?;
                self.insert(ec, text)
            }
            EditOp::ReplaceWord {
                replace_len,
                text,
                composition,
            } => {
                self.hide(ec, composition)?;
                self.replace(ec, *replace_len, text)
            }
        }
    }
}

impl SmartKeyEditSession_Impl {
    fn own_range(&self, active: &ITfComposition) -> Result<ITfRange> {
        unsafe {
            let range = active.GetRange()?;
            if range.GetContext()? != self.context {
                return Err(Error::from_hresult(E_INVALIDARG));
            }
            Ok(range)
        }
    }

    fn show(
        &self,
        ec: u32,
        text: &str,
        typed_units: usize,
        composition: &Rc<RefCell<Option<ITfComposition>>>,
        comp_sink: &ITfCompositionSink,
        atom: u32,
    ) -> Result<()> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let offset = i32::try_from(typed_units).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        if typed_units > utf16.len() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        // Never retain a RefCell borrow across COM: callbacks can reenter the sink.
        let active = composition.borrow().clone();
        unsafe {
            let range = if let Some(active) = active {
                let range = self.own_range(&active)?;
                range.SetText(ec, 0, &utf16)?;
                range
            } else {
                let insert: ITfInsertAtSelection = self.context.cast()?;
                let range = insert.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &utf16)?;
                let ctx_comp: ITfContextComposition = self.context.cast()?;
                let new_comp = ctx_comp.StartComposition(ec, &range, Some(comp_sink))?;
                *composition.borrow_mut() = Some(new_comp);
                range
            };
            let prop = self.context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
            prop.Clear(ec, &range)?;
            if atom != 0 && typed_units < utf16.len() {
                let ghost = range.Clone()?;
                let mut shifted = 0;
                ghost.ShiftStart(ec, offset, &mut shifted, std::ptr::null())?;
                if shifted != offset {
                    return Err(Error::from_hresult(E_FAIL));
                }
                prop.SetValue(ec, &ghost, &VARIANT::from(atom as i32))?;
            }
            let caret = range.Clone()?;
            caret.Collapse(ec, TF_ANCHOR_START)?;
            let mut shifted = 0;
            caret.ShiftEnd(ec, offset, &mut shifted, std::ptr::null())?;
            if shifted != offset {
                return Err(Error::from_hresult(E_FAIL));
            }
            caret.Collapse(ec, TF_ANCHOR_END)?;
            self.select(ec, caret)
        }
    }

    fn hide(&self, ec: u32, composition: &Rc<RefCell<Option<ITfComposition>>>) -> Result<()> {
        let active = composition.borrow().clone();
        if let Some(active) = active {
            let range = self.own_range(&active)?;
            // Clear the slot before EndComposition so our own callback cannot reset
            // the core state that has already generated the remaining action batch.
            unsafe {
                // A refused deletion must keep ownership of the surviving preedit.
                range.SetText(ec, 0, &[])?;
                composition.borrow_mut().take();
                if let Err(error) = active.EndComposition(ec) {
                    *composition.borrow_mut() = Some(active);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn insert(&self, ec: u32, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        unsafe {
            let insert: ITfInsertAtSelection = self.context.cast()?;
            let range = insert.InsertTextAtSelection(
                ec,
                TF_IAS_NOQUERY,
                &text.encode_utf16().collect::<Vec<_>>(),
            )?;
            let caret = range.Clone()?;
            caret.Collapse(ec, TF_ANCHOR_END)?;
            self.select(ec, caret)
        }
    }

    fn replace(&self, ec: u32, count: usize, text: &str) -> Result<()> {
        let lookback = count
            .checked_mul(2)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        unsafe {
            let mut selection = [TF_SELECTION::default()];
            let mut fetched = 0;
            let result =
                self.context
                    .GetSelection(ec, TF_DEFAULT_SELECTION, &mut selection, &mut fetched);
            let caret = ManuallyDrop::take(&mut selection[0].range);
            result?;
            let caret = caret
                .filter(|_| fetched == 1)
                .ok_or_else(|| Error::from_hresult(E_FAIL))?;
            if !caret.IsEmpty(ec)?.as_bool() {
                return Err(Error::from_hresult(E_INVALIDARG));
            }
            let previous = caret.Clone()?;
            let mut shifted = 0;
            previous.ShiftStart(ec, -lookback, &mut shifted, std::ptr::null())?;
            let mut buffer = vec![0u16; lookback as usize];
            let mut read = 0;
            previous.GetText(ec, 0, &mut buffer, &mut read)?;
            buffer.truncate(read as usize);
            let units = crate::text_contract::suffix_units(&buffer, count)
                .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
            let replacement = caret.Clone()?;
            let requested = -(units as i32);
            replacement.ShiftStart(ec, requested, &mut shifted, std::ptr::null())?;
            if shifted != requested {
                return Err(Error::from_hresult(E_FAIL));
            }
            replacement.SetText(ec, 0, &text.encode_utf16().collect::<Vec<_>>())?;
            replacement.Collapse(ec, TF_ANCHOR_END)?;
            self.select(ec, replacement)
        }
    }

    fn select(&self, ec: u32, caret: ITfRange) -> Result<()> {
        let mut selection = [TF_SELECTION {
            range: ManuallyDrop::new(Some(caret)),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_END,
                fInterimChar: BOOL(0),
            },
        }];
        unsafe {
            let result = self.context.SetSelection(ec, &selection);
            ManuallyDrop::drop(&mut selection[0].range);
            result
        }
    }
}

/// Synchronous write sessions are requested only from TSF key callbacks.
pub fn request_edit_session(context: &ITfContext, client_id: u32, op: EditOp) -> Result<()> {
    let session: ITfEditSession = SmartKeyEditSession {
        context: context.clone(),
        op,
    }
    .into();
    unsafe {
        context
            .RequestEditSession(client_id, &session, TF_ES_READWRITE | TF_ES_SYNC)?
            .ok()
    }
}
