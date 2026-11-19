//! Printing on Windows through the spooler with GDI: printers come from `EnumPrintersW`, a
//! job is a printer device context (`CreateDCW` with a `DEVMODEW` for copies, duplex,
//! collation and paper, merged by `DocumentPropertiesW` with the printer's own settings)
//! on which `StretchDIBits` draws each sheet of the print file between
//! `StartDocW`/`StartPage` and `EndPage`/`EndDoc`. Sheets are the bitmaps every platform
//! prints (see `super`), so no PDF handler needs to be installed.
//!
//! The only unsafe code of the crate: these are C APIs with no safe wrapper crate. Every
//! call gets valid, NUL-terminated strings and buffers that outlive it, and each handle is
//! released on every path by a guard.
#![allow(unsafe_code)]

use pagebara_i18n::{tr, tr_noop};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DEVMODEW, DIB_RGB_COLORS, DM_COLLATE,
    DM_COPIES, DM_DUPLEX, DM_IN_BUFFER, DM_ORIENTATION, DM_OUT_BUFFER, DM_PAPERSIZE,
    DMCOLLATE_FALSE, DMCOLLATE_TRUE, DMORIENT_LANDSCAPE, DMORIENT_PORTRAIT, DeleteDC,
    GetDeviceCaps, HALFTONE, HDC, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX,
    PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY, SetBrushOrgEx, SetStretchBltMode, StretchDIBits,
};
use windows_sys::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW,
    PRINTER_ATTRIBUTE_WORK_OFFLINE, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE,
    PRINTER_INFO_4W,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};

use super::spool::{DevicePage, PrintFile, duplex_code, paper_code, place_on_device, to_dib};
use super::system::{Job, Printer};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is a NUL-terminated string from the spooler, valid while its buffer is.
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

fn last_error(what: &str) -> String {
    let e = std::io::Error::last_os_error();
    format!("{}: {e}", pagebara_i18n::translate(what, &[]))
}

pub fn printers() -> Result<Vec<Printer>, String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0u32, 0u32);
    // SAFETY: a size query: no buffer, out-pointers to locals.
    unsafe { EnumPrintersW(flags, null(), 4, null_mut(), 0, &mut needed, &mut count) };
    if needed == 0 {
        return Ok(Vec::new());
    }
    // u64s keep the buffer aligned for the PRINTER_INFO_4W records it holds.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` holds `needed` bytes.
    let ok = unsafe {
        EnumPrintersW(
            flags,
            null(),
            4,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
            &mut count,
        )
    };
    if ok == 0 {
        return Err(last_error(tr_noop!("Could not list printers")));
    }
    // SAFETY: the spooler wrote `count` records at the start of `buf`.
    let infos = unsafe {
        std::slice::from_raw_parts(buf.as_ptr().cast::<PRINTER_INFO_4W>(), count as usize)
    };
    let default = default_printer();
    let mut out: Vec<Printer> = infos
        .iter()
        .map(|i| {
            let name = from_wide(i.pPrinterName);
            Printer {
                is_default: default.as_deref() == Some(name.as_str()),
                enabled: i.Attributes & PRINTER_ATTRIBUTE_WORK_OFFLINE == 0,
                name,
                // Windows printer names are already what people call them.
                description: String::new(),
            }
        })
        .filter(|p| !p.name.is_empty())
        .collect();
    out.sort_by_key(|p| !p.is_default);
    Ok(out)
}

fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: a size query.
    unsafe { GetDefaultPrinterW(null_mut(), &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` characters.
    (unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) } != 0)
        .then(|| from_wide(buf.as_ptr()))
}

struct PrinterHandle(PRINTER_HANDLE);

impl Drop for PrinterHandle {
    fn drop(&mut self) {
        // SAFETY: opened by OpenPrinterW, closed once.
        unsafe { ClosePrinter(self.0) };
    }
}

struct Dc(HDC);

impl Drop for Dc {
    fn drop(&mut self) {
        // SAFETY: created by CreateDCW, deleted once.
        unsafe { DeleteDC(self.0) };
    }
}

/// The printer's settings with the job's copies, duplex, collation, paper and orientation,
/// as a `DEVMODEW` with the driver's private data after it (u64s for alignment).
fn devmode(name: &[u16], job: &Job, landscape: bool) -> Result<Vec<u64>, String> {
    let mut h = PRINTER_HANDLE { Value: null_mut() };
    // SAFETY: `name` is NUL-terminated; `h` receives the handle.
    if unsafe { OpenPrinterW(name.as_ptr(), &mut h, null()) } == 0 {
        return Err(last_error(tr_noop!("Could not open the printer")));
    }
    let h = PrinterHandle(h);
    // SAFETY: with no buffers and no mode, returns the size the driver's DEVMODE needs.
    let size =
        unsafe { DocumentPropertiesW(null_mut(), h.0, name.as_ptr(), null_mut(), null(), 0) };
    if size < std::mem::size_of::<DEVMODEW>() as i32 {
        return Err(last_error(tr_noop!(
            "Could not read the printer's settings"
        )));
    }
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let dm = buf.as_mut_ptr().cast::<DEVMODEW>();
    // SAFETY: `buf` holds `size` bytes, aligned for DEVMODEW.
    if unsafe { DocumentPropertiesW(null_mut(), h.0, name.as_ptr(), dm, null(), DM_OUT_BUFFER) } < 0
    {
        return Err(last_error(tr_noop!(
            "Could not read the printer's settings"
        )));
    }
    // SAFETY: the driver filled in a DEVMODEW at the start of `buf`; the paper fields are
    // the printer variant of its first union.
    unsafe {
        let d = &mut *dm;
        let p = &mut d.Anonymous1.Anonymous1;
        d.dmFields |= DM_COPIES | DM_COLLATE | DM_DUPLEX | DM_ORIENTATION;
        p.dmCopies = job.copies.clamp(1, i16::MAX as u32) as i16;
        d.dmCollate = if job.collate {
            DMCOLLATE_TRUE
        } else {
            DMCOLLATE_FALSE
        };
        d.dmDuplex = duplex_code(job.duplex);
        let orientation = if landscape {
            DMORIENT_LANDSCAPE
        } else {
            DMORIENT_PORTRAIT
        };
        p.dmOrientation = orientation as i16;
        if let Some(code) = paper_code(job.paper) {
            d.dmFields |= DM_PAPERSIZE;
            p.dmPaperSize = code;
        }
    }
    // Let the driver check and complete the changes (it drops what it cannot do).
    // SAFETY: input and output are the same valid buffer, as the API allows.
    let merged = unsafe {
        DocumentPropertiesW(
            null_mut(),
            h.0,
            name.as_ptr(),
            dm,
            dm,
            DM_IN_BUFFER | DM_OUT_BUFFER,
        )
    };
    if merged < 0 {
        return Err(last_error(tr_noop!("The printer refused the settings")));
    }
    Ok(buf)
}

/// Prints the job's file; `output` sends it to a file instead (as "Print to file" does),
/// which is how the tests drive the "Microsoft Print to PDF" printer without a dialog.
pub fn submit(job: &Job, output: Option<&Path>) -> Result<String, String> {
    let file = PrintFile::open(&job.file)?;
    if file.is_empty() {
        return Err(tr!("Nothing to print."));
    }
    let first = file.sheet(0)?;
    let name = wide(&job.printer);
    let mode = devmode(&name, job, first.size_pt.0 > first.size_pt.1)?;
    let driver = wide("WINSPOOL");
    // SAFETY: NUL-terminated strings and a complete DEVMODE that outlive the call.
    let dc = unsafe { CreateDCW(driver.as_ptr(), name.as_ptr(), null(), mode.as_ptr().cast()) };
    if dc.is_null() {
        return Err(last_error(tr_noop!("Could not start printing")));
    }
    let dc = Dc(dc);
    // SAFETY: `dc` is a valid printer DC for all of these.
    let dev = unsafe {
        DevicePage {
            dpi: (
                f64::from(GetDeviceCaps(dc.0, LOGPIXELSX as i32)),
                f64::from(GetDeviceCaps(dc.0, LOGPIXELSY as i32)),
            ),
            physical: (
                GetDeviceCaps(dc.0, PHYSICALWIDTH as i32),
                GetDeviceCaps(dc.0, PHYSICALHEIGHT as i32),
            ),
            offset: (
                GetDeviceCaps(dc.0, PHYSICALOFFSETX as i32),
                GetDeviceCaps(dc.0, PHYSICALOFFSETY as i32),
            ),
        }
    };
    let title = wide(&job.title);
    let out = output.map(|p| wide(&p.to_string_lossy()));
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: title.as_ptr(),
        lpszOutput: out.as_ref().map_or(null(), |o| o.as_ptr()),
        lpszDatatype: null(),
        fwType: 0,
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    let id = unsafe { StartDocW(dc.0, &info) };
    if id <= 0 {
        return Err(last_error(tr_noop!("The printer did not take the job")));
    }
    let pages = (|| {
        for i in 0..file.len() {
            let sheet = if i == 0 {
                first.clone()
            } else {
                file.sheet(i)?
            };
            let dib = to_dib(&sheet);
            let (x, y, w, h) = place_on_device(sheet.size_pt, &dev);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: dib.width,
                    biHeight: dib.height,
                    biPlanes: 1,
                    biBitCount: 24,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: `dc` is in a document; `dib.bits` holds the rows `bmi` describes.
            unsafe {
                if StartPage(dc.0) <= 0 {
                    return Err(last_error(tr_noop!("Printing failed")));
                }
                SetStretchBltMode(dc.0, HALFTONE);
                SetBrushOrgEx(dc.0, 0, 0, null_mut());
                let drawn = StretchDIBits(
                    dc.0,
                    x,
                    y,
                    w,
                    h,
                    0,
                    0,
                    dib.width,
                    dib.height,
                    dib.bits.as_ptr().cast(),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                if drawn == 0 || EndPage(dc.0) <= 0 {
                    return Err(last_error(tr_noop!("Printing failed")));
                }
            }
        }
        Ok(())
    })();
    // SAFETY: ends (or abandons) the document started above.
    unsafe {
        if pages.is_err() {
            AbortDoc(dc.0);
        } else if EndDoc(dc.0) <= 0 {
            return Err(last_error(tr_noop!("Printing failed")));
        }
    }
    pages.map(|()| tr!("job {}", id))
}
