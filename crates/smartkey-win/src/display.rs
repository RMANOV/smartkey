// Ghost text rendering via TSF Display Attributes.
//
// TSF uses ITfDisplayAttributeProvider + ITfDisplayAttributeInfo to style
// inline text. Ghost text uses a semi-transparent grey foreground.

use windows::core::*;
use windows::Win32::UI::TextServices::*;

/// Display attribute for ghost (completion) text — grey, italic.
pub const GHOST_ATTR: TF_DISPLAYATTRIBUTE = TF_DISPLAYATTRIBUTE {
    crText: TF_DA_COLOR {
        r#type: TF_CT_COLORREF,
        Anonymous: windows::Win32::UI::TextServices::TF_DA_COLOR_0 {
            cr: 0x00AAAAAA, // grey RGB
        },
    },
    crBk: TF_DA_COLOR {
        r#type: TF_CT_NONE,
        Anonymous: windows::Win32::UI::TextServices::TF_DA_COLOR_0 { cr: 0 },
    },
    lsStyle: TF_LS_NONE,
    fBoldLine: false.into(),
    crLine: TF_DA_COLOR {
        r#type: TF_CT_NONE,
        Anonymous: windows::Win32::UI::TextServices::TF_DA_COLOR_0 { cr: 0 },
    },
    bAttr: TF_ATTR_OTHER,
};
