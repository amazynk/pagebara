//! `IPreviewHandler`: the Explorer preview pane. A child window shows one page fitted to
//! the pane; the mouse wheel, Page Up/Down, the arrow keys, Home and End turn pages.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use pagebara_render::Renderer;
use windows::Win32::Foundation::{
    E_FAIL, E_NOTIMPL, E_POINTER, HWND, LPARAM, LRESULT, RECT, S_FALSE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_APPWORKSPACE, DIB_RGB_COLORS, EndPaint, FillRect, GetSysColorBrush,
    InvalidateRect, PAINTSTRUCT, SetDIBitsToDevice,
};
use windows::Win32::System::Com::IStream;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SPACE,
    VK_UP,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandler_Impl, IPreviewHandlerFrame};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, IDC_ARROW, LoadCursorW, MSG,
    MoveWindow, RegisterClassW, SetParent, WINDOW_EX_STYLE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS,
    WS_VISIBLE,
};
use windows_core::{BOOL, GUID, IUnknown, Interface, PCWSTR, Ref, Result, implement};

use super::{bitmap_info, guard, read_stream};
use crate::image::centre;
use crate::pdf::{self, Bitmap};

/// Space around the page, in pixels.
const MARGIN: u32 = 8;

#[implement(IPreviewHandler, IOleWindow, IObjectWithSite, IInitializeWithStream)]
#[derive(Default)]
pub(super) struct Preview {
    inner: RefCell<Inner>,
}

#[derive(Default)]
struct Inner {
    data: Option<Vec<u8>>,
    parent: Option<HWND>,
    rect: RECT,
    window: Option<HWND>,
    site: Option<IUnknown>,
}

/// What each preview window shows, by window handle (the window procedure has no other
/// way to reach it; all of it lives on the preview thread).
struct View {
    renderer: Renderer,
    page: usize,
    bitmap: Option<Bitmap>,
    /// The client size the bitmap was drawn for.
    drawn_for: (u32, u32),
}

thread_local! {
    static VIEWS: RefCell<HashMap<isize, View>> = RefCell::new(HashMap::new());
}

fn key(h: HWND) -> isize {
    h.0 as isize
}

impl IInitializeWithStream_Impl for Preview_Impl {
    fn Initialize(&self, stream: Ref<IStream>, _mode: u32) -> Result<()> {
        guard(|| {
            let bytes = read_stream(stream.ok()?)?;
            self.inner.borrow_mut().data = Some(bytes);
            Ok(())
        })
    }
}

impl IPreviewHandler_Impl for Preview_Impl {
    fn SetWindow(&self, parent: HWND, rect: *const RECT) -> Result<()> {
        guard(|| {
            let mut inner = self.inner.borrow_mut();
            inner.parent = Some(parent);
            if !rect.is_null() {
                // SAFETY: non-null pointer to the caller's RECT.
                inner.rect = unsafe { *rect };
            }
            if let Some(w) = inner.window {
                // SAFETY: `w` is our live child window; `parent` is the host's window.
                unsafe { SetParent(w, Some(parent)) }?;
                place(w, inner.rect)?;
            }
            Ok(())
        })
    }

    fn SetRect(&self, rect: *const RECT) -> Result<()> {
        guard(|| {
            if rect.is_null() {
                return Err(E_POINTER.into());
            }
            let mut inner = self.inner.borrow_mut();
            // SAFETY: non-null pointer to the caller's RECT.
            inner.rect = unsafe { *rect };
            if let Some(w) = inner.window {
                place(w, inner.rect)?;
            }
            Ok(())
        })
    }

    fn DoPreview(&self) -> Result<()> {
        guard(|| {
            let mut inner = self.inner.borrow_mut();
            let parent = inner.parent.ok_or(E_FAIL)?;
            if inner.window.is_some() {
                return Ok(());
            }
            let bytes = inner.data.take().ok_or(E_FAIL)?;
            let doc = pdf::open(bytes).ok_or(E_FAIL)?;
            let class = register_class()?;
            let r = inner.rect;
            // SAFETY: the class is registered, `parent` is the host's window and every
            // pointer argument is null or a NUL-terminated string that outlives the call.
            let w = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class,
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    Some(parent),
                    None,
                    Some(GetModuleHandleW(None)?.into()),
                    None,
                )
            }?;
            VIEWS.with(|v| {
                v.borrow_mut().insert(
                    key(w),
                    View {
                        renderer: Renderer::new(doc),
                        page: 0,
                        bitmap: None,
                        drawn_for: (0, 0),
                    },
                )
            });
            inner.window = Some(w);
            Ok(())
        })
    }

    fn Unload(&self) -> Result<()> {
        guard(|| {
            let mut inner = self.inner.borrow_mut();
            inner.data = None;
            if let Some(w) = inner.window.take() {
                // SAFETY: `w` is our own child window, destroyed once.
                unsafe { DestroyWindow(w) }?;
            }
            Ok(())
        })
    }

    fn SetFocus(&self) -> Result<()> {
        if let Some(w) = self.inner.borrow().window {
            // SAFETY: `w` is our live window on this thread.
            unsafe { SetFocus(Some(w)) }?;
        }
        Ok(())
    }

    fn QueryFocus(&self) -> Result<HWND> {
        // SAFETY: no arguments; reads this thread's focus window.
        let h = unsafe { GetFocus() };
        if h.is_invalid() {
            Err(E_FAIL.into())
        } else {
            Ok(h)
        }
    }

    fn TranslateAccelerator(&self, msg: *const MSG) -> Result<()> {
        let frame = self
            .inner
            .borrow()
            .site
            .as_ref()
            .and_then(|s| s.cast::<IPreviewHandlerFrame>().ok());
        match frame {
            // SAFETY: `msg` is the caller's message, passed on unchanged.
            Some(f) => unsafe { f.TranslateAccelerator(msg) },
            None => Err(S_FALSE.into()),
        }
    }
}

impl IOleWindow_Impl for Preview_Impl {
    fn GetWindow(&self) -> Result<HWND> {
        self.inner.borrow().window.ok_or_else(|| E_FAIL.into())
    }

    fn ContextSensitiveHelp(&self, _enter: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IObjectWithSite_Impl for Preview_Impl {
    fn SetSite(&self, site: Ref<IUnknown>) -> Result<()> {
        self.inner.borrow_mut().site = site.cloned();
        Ok(())
    }

    fn GetSite(&self, riid: *const GUID, out: *mut *mut core::ffi::c_void) -> Result<()> {
        if out.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: non-null out-pointer from the caller.
        unsafe { *out = std::ptr::null_mut() };
        let inner = self.inner.borrow();
        let site = inner.site.as_ref().ok_or(E_FAIL)?;
        // SAFETY: `riid` and `out` are the caller's valid pointers (checked above).
        unsafe { site.query(riid, out) }.ok()
    }
}

fn place(w: HWND, r: RECT) -> Result<()> {
    // SAFETY: `w` is our live child window.
    unsafe { MoveWindow(w, r.left, r.top, r.right - r.left, r.bottom - r.top, true) }
}

/// The window class, registered once per process.
fn register_class() -> Result<PCWSTR> {
    static CLASS: OnceLock<Option<Vec<u16>>> = OnceLock::new();
    let name = CLASS.get_or_init(|| {
        let name: Vec<u16> = format!("{}.preview", pagebara_identity::APP_ID)
            .encode_utf16()
            .chain([0])
            .collect();
        // SAFETY: plain calls with valid arguments; `name` lives in the static for good.
        let ok = unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: GetModuleHandleW(None).ok()?.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            RegisterClassW(&wc) != 0
        };
        ok.then_some(name)
    });
    name.as_ref()
        .map(|n| PCWSTR(n.as_ptr()))
        .ok_or_else(|| E_FAIL.into())
}

extern "system" fn window_proc(w: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = std::panic::catch_unwind(|| handle(w, msg, wp, lp)).unwrap_or(Some(LRESULT(0)));
    // SAFETY: the arguments are the ones Windows passed to this procedure.
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(w, msg, wp, lp) })
}

/// Our handling of a message, or `None` for the default.
fn handle(w: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_PAINT => paint(w),
        WM_ERASEBKGND => return Some(LRESULT(1)),
        WM_SIZE => invalidate(w),
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16;
            turn(w, |p, _| {
                if delta < 0 {
                    p + 1
                } else {
                    p.saturating_sub(1)
                }
            });
        }
        WM_KEYDOWN => {
            let k = wp.0 as u16;
            if k == VK_NEXT.0 || k == VK_DOWN.0 || k == VK_RIGHT.0 || k == VK_SPACE.0 {
                turn(w, |p, _| p + 1);
            } else if k == VK_PRIOR.0 || k == VK_UP.0 || k == VK_LEFT.0 {
                turn(w, |p, _| p.saturating_sub(1));
            } else if k == VK_HOME.0 {
                turn(w, |_, _| 0);
            } else if k == VK_END.0 {
                turn(w, |_, n| n.saturating_sub(1));
            } else {
                return None;
            }
        }
        WM_LBUTTONDOWN => {
            // SAFETY: `w` is the window this message is for, on this thread.
            let _ = unsafe { SetFocus(Some(w)) };
        }
        WM_DESTROY => {
            VIEWS.with(|v| v.borrow_mut().remove(&key(w)));
        }
        _ => return None,
    }
    let _ = lp;
    Some(LRESULT(0))
}

fn invalidate(w: HWND) {
    // SAFETY: `w` is our live window.
    let _ = unsafe { InvalidateRect(Some(w), None, false) };
}

/// Shows page `to(current, count)`.
fn turn(w: HWND, to: impl Fn(usize, usize) -> usize) {
    let changed = VIEWS.with(|v| {
        let mut v = v.borrow_mut();
        let view = v.get_mut(&key(w))?;
        let n = view.renderer.page_count();
        let p = to(view.page, n).min(n.saturating_sub(1));
        (p != view.page).then(|| {
            view.page = p;
            view.bitmap = None;
        })
    });
    if changed.is_some() {
        invalidate(w);
    }
}

fn client_size(w: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    // SAFETY: `w` is our live window and `r` a valid out-pointer.
    let _ = unsafe { GetClientRect(w, &mut r) };
    (
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}

fn paint(w: HWND) {
    let (cw, ch) = client_size(w);
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `w` is the window being painted; `ps` is a valid out-pointer.
    let dc = unsafe { BeginPaint(w, &mut ps) };
    let full = RECT {
        left: 0,
        top: 0,
        right: cw as i32,
        bottom: ch as i32,
    };
    // SAFETY: `dc` is the paint DC; system colour brushes need no freeing.
    unsafe { FillRect(dc, &full, GetSysColorBrush(COLOR_APPWORKSPACE)) };
    VIEWS.with(|v| {
        let mut v = v.borrow_mut();
        let Some(view) = v.get_mut(&key(w)) else {
            return;
        };
        if view.bitmap.is_none() || view.drawn_for != (cw, ch) {
            let (bw, bh) = (cw.saturating_sub(2 * MARGIN), ch.saturating_sub(2 * MARGIN));
            view.bitmap = (bw > 0 && bh > 0)
                .then(|| pdf::render_fit(&view.renderer, view.page, bw, bh))
                .flatten();
            view.drawn_for = (cw, ch);
        }
        if let Some(b) = &view.bitmap {
            let (x, y) = centre(b.width, b.height, cw, ch);
            let info = bitmap_info(b.width, b.height);
            // SAFETY: `b.bgra` holds `width × height` BGRA pixels as `info` describes.
            unsafe {
                SetDIBitsToDevice(
                    dc,
                    x,
                    y,
                    b.width,
                    b.height,
                    0,
                    0,
                    0,
                    b.height,
                    b.bgra.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                )
            };
        }
    });
    // SAFETY: ends the paint begun above.
    let _ = unsafe { EndPaint(w, &ps) };
}
