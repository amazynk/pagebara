//! Running the phone UI on a desktop, for development and UI tests: file dialogs for
//! pickers; sharing is left to phones.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use pagebara_i18n::tr;

use super::{Event, Platform, Sink, WriteBack};

#[derive(Default)]
pub struct Host {
    profile: Option<PathBuf>,
    sink: RefCell<Option<Sink>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl Host {
    pub fn new(profile: Option<PathBuf>) -> Host {
        Host {
            profile,
            sink: RefCell::new(None),
        }
    }

    fn pick(&self, title: String, name: String, ext: &'static [&'static str]) {
        let Some(sink) = self.sink.borrow().clone() else {
            return;
        };
        let task = slint::spawn_local(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title(title)
                .add_filter(name, ext)
                .pick_file()
                .await;
            if let Some(f) = picked {
                let path = f.path().to_owned();
                sink(if ext.contains(&"pdf") {
                    Event::Open(path)
                } else if ext.contains(&"fdf") {
                    Event::Data(path)
                } else {
                    Event::Image(path)
                });
            }
        });
        if let Err(e) = task {
            log::warn!("cannot show the file dialog: {e}");
        }
    }
}

impl Platform for Host {
    fn data_dir(&self) -> Option<PathBuf> {
        self.profile.clone().or_else(|| {
            pagebara_signatures::Library::default_dir()
                .and_then(|d| d.parent().map(|p| p.join("mobile")))
        })
    }

    fn connect(&self, sink: Sink) {
        *self.sink.borrow_mut() = Some(sink);
    }

    fn pick_pdf(&self) {
        self.pick(tr!("Open a PDF"), tr!("PDF documents"), &["pdf", "PDF"]);
    }

    fn pick_image(&self) {
        self.pick(
            tr!("A photo of your signature"),
            tr!("Images"),
            &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"],
        );
    }

    fn pick_form_data(&self) {
        self.pick(
            tr!("Form data"),
            tr!("FDF and XFDF"),
            &["fdf", "xfdf", "FDF", "XFDF"],
        );
    }

    fn can_share(&self) -> bool {
        false
    }

    fn share(&self, _path: &Path, _name: &str) {}

    fn write_back(&self, _path: &Path) -> WriteBack {
        WriteBack::Local
    }

    fn save_copy(&self, path: &Path, name: &str) {
        let Some(sink) = self.sink.borrow().clone() else {
            return;
        };
        let from = path.to_owned();
        let name = name.to_owned();
        let task = slint::spawn_local(async move {
            let ext = Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("pdf")
                .to_owned();
            let Some(f) = rfd::AsyncFileDialog::new()
                .set_title(tr!("Save a copy"))
                .add_filter(ext.to_uppercase(), &[ext.as_str()])
                .set_file_name(&name)
                .save_file()
                .await
            else {
                return;
            };
            sink(Event::Message(match std::fs::copy(&from, f.path()) {
                Ok(_) => tr!("Saved a copy."),
                Err(e) => tr!("The copy could not be saved: {}", e),
            }));
        });
        if let Err(e) = task {
            log::warn!("cannot show the file dialog: {e}");
        }
    }
}
