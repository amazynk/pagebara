//! The COM server: the DLL's exports, the class factory and helpers shared by the three
//! extensions. Nothing is registered here (see `pagebara-integration`).
#![allow(unsafe_code)]

mod filter;
mod preview;
mod thumbnail;

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_OUTOFMEMORY, E_POINTER, S_FALSE,
};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, IStream};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

use crate::MAX_FILE;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Thumbnail,
    Preview,
    Filter,
}

impl Kind {
    fn from_clsid(id: GUID) -> Option<Kind> {
        [Kind::Thumbnail, Kind::Preview, Kind::Filter]
            .into_iter()
            .find(|k| clsid(k.name()) == id)
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Thumbnail => "thumbnail",
            Kind::Preview => "preview",
            Kind::Filter => "filter",
        }
    }
}

fn clsid(kind: &str) -> GUID {
    GUID::from_u128(pagebara_identity::windows_clsid(kind))
}

/// Explorer asks for a class factory of one of our class IDs.
#[unsafe(no_mangle)]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: `ppv` is non-null and points to the caller's out-pointer.
    unsafe { *ppv = std::ptr::null_mut() };
    // SAFETY: `rclsid` is non-null and points to the caller's GUID.
    let Some(kind) = Kind::from_clsid(unsafe { *rclsid }) else {
        return CLASS_E_CLASSNOTAVAILABLE;
    };
    let factory: IClassFactory = Factory(kind).into();
    // SAFETY: `riid` and `ppv` are the caller's valid pointers (checked above).
    unsafe { factory.query(riid, ppv) }
}

/// Never unloaded: the renderer's worker threads run code from this DLL.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[implement(IClassFactory)]
struct Factory(Kind);

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: `ppv` is non-null and points to the caller's out-pointer.
        unsafe { *ppv = std::ptr::null_mut() };
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let unknown: IUnknown = match self.0 {
            Kind::Thumbnail => thumbnail::Thumbnail::default().into(),
            Kind::Preview => preview::Preview::default().into(),
            Kind::Filter => filter::Filter::default().into(),
        };
        // SAFETY: `riid` and `ppv` are the caller's valid pointers (checked above).
        unsafe { unknown.query(riid, ppv) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// Runs `f`, turning a panic into `E_FAIL` (a panic must not cross into Explorer).
fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(E_FAIL.into()))
}

/// [`guard`] for the methods that return a bare `HRESULT`.
fn guard_hr(f: impl FnOnce() -> HRESULT) -> HRESULT {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(E_FAIL)
}

/// The whole stream, up to [`MAX_FILE`] bytes.
fn read_stream(s: &IStream) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let mut n = 0u32;
        // SAFETY: `buf` is writable for `buf.len()` bytes and `n` is a valid out-pointer.
        unsafe { s.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut n)) }.ok()?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..(n as usize).min(buf.len())]);
        if out.len() as u64 > MAX_FILE {
            return Err(E_OUTOFMEMORY.into());
        }
    }
}

/// The header of a top-down 32-bit BGRA bitmap.
fn bitmap_info(width: u32, height: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}
