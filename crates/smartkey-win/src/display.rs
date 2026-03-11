// Ghost text rendering via TSF Display Attributes.
//
// TSF uses ITfDisplayAttributeProvider + ITfDisplayAttributeInfo to style
// inline text. Ghost text uses a semi-transparent grey foreground.
//
// 3-layer indirection: TIP registers GUID → ITfCategoryMgr resolves atom →
// ITfDisplayAttributeProvider returns TF_DISPLAYATTRIBUTE for that GUID.

use windows::core::*;
use windows::Win32::Foundation::{BOOL, COLORREF, E_NOTIMPL, E_POINTER};
use windows::Win32::UI::TextServices::*;

/// GUID identifying the ghost text display attribute.
/// Used by ITfCategoryMgr::RegisterGUID() and ITfDisplayAttributeProvider.
pub const GUID_GHOST_ATTR: GUID = GUID::from_u128(0xA1B2C3D4_E5F6_4789_80A1_B2C3D4E5F6A7);

/// Display attribute for ghost (completion) text — grey foreground, no underline.
pub const GHOST_ATTR: TF_DISPLAYATTRIBUTE = TF_DISPLAYATTRIBUTE {
    crText: TF_DA_COLOR {
        r#type: TF_CT_COLORREF,
        Anonymous: TF_DA_COLOR_0 {
            cr: COLORREF(0x00AAAAAA), // grey RGB
        },
    },
    crBk: TF_DA_COLOR {
        r#type: TF_CT_NONE,
        Anonymous: TF_DA_COLOR_0 { cr: COLORREF(0) },
    },
    lsStyle: TF_LS_NONE,
    fBoldLine: BOOL(0),
    crLine: TF_DA_COLOR {
        r#type: TF_CT_NONE,
        Anonymous: TF_DA_COLOR_0 { cr: COLORREF(0) },
    },
    bAttr: TF_ATTR_OTHER,
};

/// ITfDisplayAttributeInfo implementation — returns GHOST_ATTR styling.
#[implement(ITfDisplayAttributeInfo)]
pub struct GhostAttributeInfo;

impl ITfDisplayAttributeInfo_Impl for GhostAttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_GHOST_ATTR)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("SmartKey ghost text"))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        unsafe {
            *pda = GHOST_ATTR;
        }
        Ok(())
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn Reset(&self) -> Result<()> {
        Ok(())
    }
}

/// One-shot COM enumerator yielding a single GhostAttributeInfo.
///
/// TSF calls EnumDisplayAttributeInfo() → iterates with Next().
/// We only have one attribute, so this is a trivial one-item enumerator.
#[implement(IEnumTfDisplayAttributeInfo)]
pub struct SingleItemEnum {
    yielded: std::cell::Cell<bool>,
}

impl SingleItemEnum {
    pub fn new() -> Self {
        Self {
            yielded: std::cell::Cell::new(false),
        }
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for SingleItemEnum_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        let clone = SingleItemEnum {
            yielded: std::cell::Cell::new(self.yielded.get()),
        };
        Ok(clone.into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if rginfo.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        if !self.yielded.get() && ulcount >= 1 {
            let info: ITfDisplayAttributeInfo = GhostAttributeInfo.into();
            unsafe {
                *rginfo = Some(info);
            }
            if !pcfetched.is_null() {
                unsafe {
                    *pcfetched = 1;
                }
            }
            self.yielded.set(true);
        } else if !pcfetched.is_null() {
            unsafe {
                *pcfetched = 0;
            }
        }
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.yielded.set(false);
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        if ulcount >= 1 {
            self.yielded.set(true);
        }
        Ok(())
    }
}
