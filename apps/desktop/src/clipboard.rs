//! The system clipboard. One handle is kept for the app's lifetime: on X11 the copied text
//! is served by the process holding it.

use std::cell::RefCell;

thread_local! {
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

pub fn set_text(text: &str) -> Result<(), String> {
    CLIPBOARD.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        c.as_mut()
            .map_or(Ok(()), |c| c.set_text(text).map_err(|e| e.to_string()))
    })
}
