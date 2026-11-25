//! Shell glue: recent documents, the taskbar jump list and progress, the AppUserModelID
//! and association-change notices. Also the console of the terminal the app was started
//! from, and the message box that stands in for it.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{E_FAIL, HWND, PROPERTYKEY};
use windows::Win32::Storage::EnhancedStorage::PKEY_Title;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
    ITaskbarList3, SHARD_PATHW, SHAddToRecentDocs, SHCNE_ASSOCCHANGED, SHCNF_IDLIST,
    SHChangeNotify, SetCurrentProcessExplicitAppUserModelID, ShellLink, TBPF_NOPROGRESS,
    TBPF_NORMAL, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW};
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};

static APP_ID: OnceLock<HSTRING> = OnceLock::new();

/// COM for this thread for as long as it lives (uninitialised again only when this call
/// was the one that started it).
struct Com(bool);

impl Com {
    fn init() -> Com {
        // SAFETY: plain COM start-up for this thread; balanced in `drop` when it succeeded
        // (an already-initialised thread answers S_FALSE, which also needs the balance).
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `CoInitializeEx` of `Com::init` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn create<T: Interface>(clsid: &windows::core::GUID) -> windows::core::Result<T> {
    // SAFETY: creates an in-process COM object; COM is initialised on this thread.
    unsafe { CoCreateInstance(clsid, None, CLSCTX_INPROC_SERVER) }
}

/// Whether the standard handle `which` leads somewhere (a console, file or pipe).
fn has_handle(which: STD_HANDLE) -> bool {
    // SAFETY: GetStdHandle only reads this process's standard handle table.
    let h = unsafe { GetStdHandle(which) };
    h.is_ok_and(|h| !h.is_invalid() && !h.0.is_null())
}

/// Whether [`attach_parent_console`] attached, and whether error output was redirected
/// (to a file or pipe) before it did.
static ATTACHED: AtomicBool = AtomicBool::new(false);
static ERRORS_REDIRECTED: AtomicBool = AtomicBool::new(false);
/// Whether [`detach_console`] let go of the console again.
static DETACHED: AtomicBool = AtomicBool::new(false);

pub fn attach_parent_console() -> bool {
    let errors = has_handle(STD_ERROR_HANDLE);
    if has_handle(STD_OUTPUT_HANDLE) && errors {
        return false;
    }
    // SAFETY: AttachConsole takes a process id (here the documented "parent process"
    // value) and touches no memory of ours; it fails when the parent has no console. It
    // fills in the standard handles that led nowhere and keeps a redirected one; std reads
    // them on every write, so later output goes to that console.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    ATTACHED.store(attached, Ordering::Relaxed);
    ERRORS_REDIRECTED.store(errors, Ordering::Relaxed);
    attached
}

pub fn detach_console() {
    if !ATTACHED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: FreeConsole takes no arguments and only detaches this process from its
    // console; the standard handles left behind are invalid, which std treats as a sink.
    let _ = unsafe { FreeConsole() };
    DETACHED.store(true, Ordering::Relaxed);
}

pub fn has_terminal() -> bool {
    // After the console is let go its handles are still set but lead nowhere.
    if DETACHED.load(Ordering::Relaxed) {
        return ERRORS_REDIRECTED.load(Ordering::Relaxed);
    }
    has_handle(STD_ERROR_HANDLE)
}

pub fn error_box(title: &[u16], text: &[u16]) {
    if title.last() != Some(&0) || text.last() != Some(&0) {
        return;
    }
    // SAFETY: both pointers come from slices that end in NUL (checked above) and outlive
    // the call; MessageBoxW only reads them. No owner window: the box stands alone.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        )
    };
}

pub fn note_recent(path: &Path) {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: SHARD_PATHW takes a NUL-terminated wide path, valid for the call.
    unsafe { SHAddToRecentDocs(SHARD_PATHW.0 as u32, Some(wide.as_ptr().cast())) };
}

pub fn set_app_user_model_id(id: &str) {
    let id = APP_ID.get_or_init(|| HSTRING::from(id));
    // SAFETY: the ID is a NUL-terminated wide string, copied by the call.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(id) } {
        log::warn!("AppUserModelID: {e}");
    }
}

pub fn shell_assoc_changed() {
    // SAFETY: SHCNE_ASSOCCHANGED takes no items.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// A shell link to this executable with `args`, titled `title`.
fn link(exe: &Path, args: &str, title: &str, tip: &str) -> windows::core::Result<IShellLinkW> {
    let l: IShellLinkW = create(&ShellLink)?;
    let exe = HSTRING::from(exe.as_os_str());
    let mut title: Vec<u16> = title.encode_utf16().chain([0]).collect();
    let value = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                Anonymous: PROPVARIANT_0_0_0 {
                    pwszVal: PWSTR(title.as_mut_ptr()),
                },
                ..Default::default()
            }),
        },
    };
    let key: PROPERTYKEY = PKEY_Title;
    // SAFETY: COM calls on a live link; the strings and `value` (borrowing `title`, which
    // outlives the calls and is never freed by the store) stay valid while they run.
    unsafe {
        l.SetPath(&exe)?;
        l.SetArguments(&HSTRING::from(args))?;
        l.SetIconLocation(&exe, 0)?;
        l.SetDescription(&HSTRING::from(tip))?;
        let store: IPropertyStore = l.cast()?;
        store.SetValue(&key, &value)?;
        store.Commit()?;
    }
    Ok(l)
}

/// The arguments of a link the user removed from the jump list.
fn link_args(l: &IShellLinkW) -> Option<String> {
    let mut buf = [0u16; 2048];
    // SAFETY: the link writes at most `buf.len()` characters, NUL included.
    unsafe { l.GetArguments(&mut buf) }.ok()?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn quoted(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

pub fn set_jump_list(recent: &[PathBuf], new_window_label: &str) -> Result<(), String> {
    let _com = Com::init();
    build_jump_list(recent, new_window_label).map_err(|e| e.to_string())
}

fn build_jump_list(recent: &[PathBuf], new_window: &str) -> windows::core::Result<()> {
    let exe =
        std::env::current_exe().map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
    let list: ICustomDestinationList = create(&DestinationList)?;
    let tasks: IObjectCollection = create(&EnumerableObjectCollection)?;
    let files: IObjectCollection = create(&EnumerableObjectCollection)?;
    let label = if new_window.is_empty() {
        "New window"
    } else {
        new_window
    };
    let new_window = link(&exe, "--new-window", label, label)?;
    let mut slots = 0u32;
    // SAFETY: COM calls on live objects created above; out-pointers are locals.
    unsafe {
        if let Some(id) = APP_ID.get() {
            list.SetAppID(id)?;
        }
        let removed: IObjectArray = list.BeginList(&mut slots)?;
        let mut gone = Vec::new();
        for i in 0..removed.GetCount()? {
            if let Ok(l) = removed.GetAt::<IShellLinkW>(i) {
                gone.extend(link_args(&l));
            }
        }
        tasks.AddObject(&new_window)?;
        let mut added = 0;
        for p in recent {
            let args = quoted(p);
            if gone.contains(&args) || added >= slots.max(1) {
                continue;
            }
            files.AddObject(&link(&exe, &args, &crate::title(p), &p.to_string_lossy())?)?;
            added += 1;
        }
        if added > 0 {
            list.AppendCategory(&HSTRING::from("Recent"), &files.cast::<IObjectArray>()?)?;
        }
        list.AddUserTasks(&tasks.cast::<IObjectArray>()?)?;
        list.CommitList()
    }
}

pub fn clear_jump_list() {
    let _com = Com::init();
    let r = create::<ICustomDestinationList>(&DestinationList).and_then(|list| {
        // SAFETY: COM call on a live list; the ID string outlives it.
        unsafe { list.DeleteList(APP_ID.get().unwrap_or(&HSTRING::new())) }
    });
    if let Err(e) = r {
        log::warn!("clear jump list: {e}");
    }
}

thread_local! {
    static TASKBAR: RefCell<Option<(Com, ITaskbarList3)>> = const { RefCell::new(None) };
}

pub fn taskbar_progress(hwnd: isize, value: Option<f64>) {
    TASKBAR.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            let com = Com::init();
            let Ok(list) = create::<ITaskbarList3>(&TaskbarList) else {
                return;
            };
            // SAFETY: COM call on the list just created.
            if unsafe { list.HrInit() }.is_err() {
                return;
            }
            *t = Some((com, list));
        }
        let Some((_, list)) = t.as_ref() else { return };
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: COM calls on a live list; an invalid window is reported, not used.
        let r = unsafe {
            match value {
                None => list.SetProgressState(hwnd, TBPF_NOPROGRESS),
                Some(v) => list.SetProgressState(hwnd, TBPF_NORMAL).and_then(|_| {
                    list.SetProgressValue(hwnd, (v.clamp(0.0, 1.0) * 1000.0) as u64, 1000)
                }),
            }
        };
        if let Err(e) = r {
            log::debug!("taskbar progress: {e}");
        }
    });
}
