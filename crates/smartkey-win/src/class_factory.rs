//! COM Class Factory for SmartKey TSF Text Input Processor.
//!
//! Implements IClassFactory to create SmartKeyTextService instances
//! when Windows calls CoCreateInstance with CLSID_SMARTKEY.

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use windows::core::*;
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::Com::IClassFactory_Impl;

use crate::dll::{LOCK_COUNT, OBJECT_COUNT};
use crate::tsf::SmartKeyTextService;

const CLASS_E_NOAGGREGATION: HRESULT = HRESULT(0x80040110_u32 as i32);
const E_POINTER: HRESULT = HRESULT(0x80004003_u32 as i32);

/// Class factory that creates SmartKeyTextService instances.
#[implement(IClassFactory)]
pub(crate) struct SmartKeyClassFactory;

impl IClassFactory_Impl for SmartKeyClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        // COM aggregation not supported.
        if punkouter.is_some() {
            return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
        }

        unsafe {
            if ppvobject.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            *ppvobject = std::ptr::null_mut();
        }

        // Create the TIP and QueryInterface for the requested riid.
        let tip = SmartKeyTextService::new();
        let unknown: IUnknown = tip.into();
        unsafe {
            let hr = (Interface::vtable(&unknown).QueryInterface)(
                Interface::as_raw(&unknown),
                riid,
                ppvobject,
            );
            if hr.is_err() {
                return Err(Error::from_hresult(hr));
            }
        }
        // `unknown` drops here → Release balances the initial ref.
        // Caller holds their own AddRef'd reference from QueryInterface.

        OBJECT_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            LOCK_COUNT.fetch_add(1, Ordering::SeqCst);
        } else {
            LOCK_COUNT
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    Some(n.saturating_sub(1))
                })
                .ok();
        }
        Ok(())
    }
}
