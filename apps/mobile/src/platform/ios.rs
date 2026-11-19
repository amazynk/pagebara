//! iOS: the app's Documents folder is shared with the Files app (`UIFileSharingEnabled`,
//! `LSSupportsOpeningDocumentsInPlace`), so PDFs put there show on the start screen and
//! saved documents show in Files. The system document picker and share sheet need UIKit
//! calls that are not wired up yet.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use pagebara_i18n::tr;

use super::{Event, Platform, Sink, WriteBack};

#[derive(Default)]
pub struct Ios {
    sink: RefCell<Option<Sink>>,
}

impl std::fmt::Debug for Ios {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ios")
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn documents_dir() -> PathBuf {
    home().join("Documents")
}

impl Ios {
    fn say(&self, text: &str) {
        if let Some(sink) = self.sink.borrow().as_ref() {
            sink(Event::Message(text.into()));
        }
    }
}

impl Platform for Ios {
    fn data_dir(&self) -> Option<PathBuf> {
        Some(
            home()
                .join("Library/Application Support")
                .join(crate::APP_NAME),
        )
    }

    fn cache_dir(&self) -> PathBuf {
        home().join("Library/Caches")
    }

    fn connect(&self, sink: Sink) {
        *self.sink.borrow_mut() = Some(sink);
    }

    fn pick_pdf(&self) {
        self.say(&tr!(
            "Put PDFs in Files › On My iPhone › {}; they show here.",
            crate::APP_NAME
        ));
    }

    fn pick_image(&self) {
        self.say(&tr!(
            "Put the photo in Files › On My iPhone › {} as signature.jpg or .png.",
            crate::APP_NAME
        ));
        let dir = documents_dir();
        for name in ["signature.png", "signature.jpg", "signature.jpeg"] {
            let p = dir.join(name);
            if p.exists()
                && let Some(sink) = self.sink.borrow().as_ref()
            {
                sink(Event::Image(p));
                return;
            }
        }
    }

    /// The newest FDF or XFDF file in the app's documents folder.
    fn pick_form_data(&self) {
        let newest = std::fs::read_dir(documents_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    e.eq_ignore_ascii_case("fdf") || e.eq_ignore_ascii_case("xfdf")
                })
            })
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        match (newest, self.sink.borrow().as_ref()) {
            (Some(p), Some(sink)) => sink(Event::Data(p)),
            _ => self.say(&tr!(
                "Put the .fdf or .xfdf file in Files › On My iPhone › {}.",
                crate::APP_NAME
            )),
        }
    }

    fn can_share(&self) -> bool {
        false
    }

    fn share(&self, _path: &Path, _name: &str) {}

    fn write_back(&self, _path: &Path) -> WriteBack {
        WriteBack::Local
    }

    fn save_copy(&self, path: &Path, name: &str) {
        let to = documents_dir().join(name);
        self.say(&match std::fs::copy(path, &to) {
            Ok(_) => tr!(
                "Saved “{}” in Files › On My iPhone › {}.",
                name,
                crate::APP_NAME
            ),
            Err(e) => tr!("The copy could not be saved: {}", e),
        });
    }

    fn documents(&self) -> Vec<PathBuf> {
        let Ok(dir) = std::fs::read_dir(documents_dir()) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
            .collect();
        out.sort();
        out
    }

    fn home_hint(&self) -> String {
        tr!(
            "PDFs you put in Files › On My iPhone › {} show here.",
            crate::APP_NAME
        )
    }
}
