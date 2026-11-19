//! `IThumbnailProvider`: page 1 as Explorer's thumbnail.
#![allow(unsafe_code)]

use std::cell::RefCell;

use pagebara_render::Renderer;
use windows::Win32::Foundation::{E_FAIL, E_POINTER, E_UNEXPECTED};
use windows::Win32::Graphics::Gdi::{CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, WTS_ALPHATYPE, WTSAT_ARGB,
};
use windows_core::{Ref, Result, implement};

use super::{bitmap_info, guard, read_stream};
use crate::pdf::{self, Bitmap};

/// Largest thumbnail drawn (Explorer asks for up to 2560 at its biggest view).
const MAX_THUMBNAIL: u32 = 2560;

#[implement(IThumbnailProvider, IInitializeWithStream)]
#[derive(Default)]
pub(super) struct Thumbnail {
    data: RefCell<Option<Vec<u8>>>,
}

impl IInitializeWithStream_Impl for Thumbnail_Impl {
    fn Initialize(&self, stream: Ref<IStream>, _mode: u32) -> Result<()> {
        guard(|| {
            let bytes = read_stream(stream.ok()?)?;
            *self.data.borrow_mut() = Some(bytes);
            Ok(())
        })
    }
}

impl IThumbnailProvider_Impl for Thumbnail_Impl {
    fn GetThumbnail(&self, cx: u32, bitmap: *mut HBITMAP, alpha: *mut WTS_ALPHATYPE) -> Result<()> {
        guard(|| {
            if bitmap.is_null() || alpha.is_null() {
                return Err(E_POINTER.into());
            }
            let bytes = self.data.borrow_mut().take().ok_or(E_UNEXPECTED)?;
            let doc = pdf::open(bytes).ok_or(E_FAIL)?;
            let size = cx.clamp(1, MAX_THUMBNAIL);
            let b = pdf::render_fit(&Renderer::new(doc), 0, size, size).ok_or(E_FAIL)?;
            let h = dib(&b)?;
            // SAFETY: both out-pointers are non-null (checked above) and owned by the caller.
            unsafe {
                *bitmap = h;
                *alpha = WTSAT_ARGB;
            }
            Ok(())
        })
    }
}

/// A DIB section holding `b`; the caller (Explorer) owns and frees it.
fn dib(b: &Bitmap) -> Result<HBITMAP> {
    let info = bitmap_info(b.width, b.height);
    let mut bits = std::ptr::null_mut();
    // SAFETY: `info` describes a 32-bit top-down bitmap; `bits` receives its pixel pointer.
    let h = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }?;
    if bits.is_null() {
        // SAFETY: `h` is the bitmap just made and not handed out.
        let _ = unsafe { DeleteObject(h.into()) };
        return Err(E_FAIL.into());
    }
    // SAFETY: the section has width × height × 4 bytes, the length of `b.bgra`.
    unsafe { std::ptr::copy_nonoverlapping(b.bgra.as_ptr(), bits.cast::<u8>(), b.bgra.len()) };
    Ok(h)
}
