//! What differs per platform: where data lives, picking files, sharing and writing back to
//! documents opened from other apps. Answers that take a while (pickers) come back as
//! [`Event`]s.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod host;
#[cfg(target_os = "ios")]
pub mod ios;

use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A PDF to open: a local file, or a local copy of a document from another app.
    Open(PathBuf),
    /// A picture for a new signature.
    Image(PathBuf),
    /// Form data (FDF or XFDF) to fill the form with.
    Data(PathBuf),
    Message(String),
}

pub type Sink = Rc<dyn Fn(Event)>;

/// What [`Platform::write_back`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteBack {
    /// The file is the document itself; nothing to do.
    Local,
    /// Copied back to where it came from.
    Done,
    /// It came from somewhere that cannot be written to.
    Failed,
}

pub trait Platform {
    /// Where settings, recent files and signatures are kept.
    fn data_dir(&self) -> Option<PathBuf>;
    /// Where copies for sharing and documents received from other apps are kept.
    fn cache_dir(&self) -> PathBuf {
        std::env::temp_dir().join(crate::APP_NAME)
    }
    /// From now on, results go to `sink` (including what is pending, such as the
    /// document the app was started with).
    fn connect(&self, sink: Sink);
    fn pick_pdf(&self);
    fn pick_image(&self);
    /// Lets the user pick an FDF or XFDF file.
    fn pick_form_data(&self);
    fn can_share(&self) -> bool;
    /// Offers the file to other apps.
    fn share(&self, path: &Path, name: &str);
    /// After `path` was saved: copies it to the document it is a copy of, if any.
    fn write_back(&self, path: &Path) -> WriteBack;
    /// Lets the user pick where a copy of `path` goes.
    fn save_copy(&self, path: &Path, name: &str);
    /// Whether the system is in dark mode, when known.
    fn dark(&self) -> Option<bool> {
        None
    }
    /// PDFs the app can show on its start screen besides the recent ones.
    fn documents(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    /// A line for the start screen about where documents come from.
    fn home_hint(&self) -> String {
        String::new()
    }
    /// Back on the start screen: puts the app in the background where that is the custom;
    /// whether it did.
    fn leave(&self) -> bool {
        false
    }
}
