//! Every integration item: its ID (for the settings page and `--integration`), title,
//! explanation, and which part of the app owns it.

use crate::Os;
use pagebara_i18n::{tr, tr_noop};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Something registered with the system; recorded so turning it off removes exactly
    /// that (see `plan.rs`).
    Registration,
    /// An app setting (single instance, the recent list, …), kept in the app's settings.
    Setting,
    /// The system decides; the app offers a button that opens the system's page.
    Button,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Finder,
    Opening,
    Previews,
    FileManager,
    Window,
}

impl Group {
    pub fn title(self) -> String {
        match self {
            Group::Finder => tr!("Finder Quick Actions"),
            Group::Opening => tr!("Opening files"),
            Group::Previews => tr!("Previews and search"),
            Group::FileManager => tr!("File manager menus"),
            Group::Window => tr!("Windows, taskbar and notifications"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: &'static str,
    pub title: String,
    pub explain: String,
    pub group: Group,
    pub kind: Kind,
    /// The state before the user chooses (settings only; registrations start off).
    pub default_on: bool,
}

/// A file-manager menu entry: the item `action-<name>` runs `exe <args> FILES…`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuAction {
    pub name: &'static str,
    pub title: &'static str,
    /// Command-line arguments before the files.
    pub args: &'static [&'static str],
    /// Takes all selected files at once (on Windows, one process per file collects them).
    pub multi: bool,
    /// Whether the selection may hold several files.
    pub many: bool,
}

/// The menu actions, in menu order. Names match `pagebara-actions` where they run headless.
pub const MENU_ACTIONS: [MenuAction; 10] = [
    MenuAction {
        name: "new-window",
        title: tr_noop!("Open in new window"),
        args: &["--new-window"],
        multi: true,
        many: true,
    },
    MenuAction {
        name: "merge",
        title: tr_noop!("Merge into one PDF"),
        args: &["--action", "merge"],
        multi: true,
        many: true,
    },
    MenuAction {
        name: "split",
        title: tr_noop!("Split into single pages"),
        args: &["--action", "split"],
        multi: false,
        many: true,
    },
    MenuAction {
        name: "rotate-right",
        title: tr_noop!("Rotate right"),
        args: &["--action", "rotate-right"],
        multi: false,
        many: true,
    },
    MenuAction {
        name: "rotate-left",
        title: tr_noop!("Rotate left"),
        args: &["--action", "rotate-left"],
        multi: false,
        many: true,
    },
    MenuAction {
        name: "compress",
        title: tr_noop!("Compress"),
        args: &["--action", "compress"],
        multi: false,
        many: true,
    },
    MenuAction {
        name: "images",
        title: tr_noop!("Convert pages to images"),
        args: &["--action", "images"],
        multi: false,
        many: true,
    },
    MenuAction {
        name: "ocr",
        title: tr_noop!("Make searchable (OCR)"),
        args: &["--action", "ocr"],
        multi: false,
        many: true,
    },
    MenuAction {
        name: "sign",
        title: tr_noop!("Sign"),
        args: &["--new-window", "--sign"],
        multi: false,
        many: false,
    },
    MenuAction {
        name: "print",
        title: tr_noop!("Print"),
        args: &["--new-window", "--print"],
        multi: false,
        many: false,
    },
];

/// The context-menu toggles: rotate left and right go together.
pub const MENU_ITEMS: [(&str, &[&str]); 9] = [
    ("action-new-window", &["new-window"]),
    ("action-merge", &["merge"]),
    ("action-split", &["split"]),
    ("action-rotate", &["rotate-right", "rotate-left"]),
    ("action-compress", &["compress"]),
    ("action-images", &["images"]),
    ("action-ocr", &["ocr"]),
    ("action-sign", &["sign"]),
    ("action-print", &["print"]),
];

pub fn menu_action(name: &str) -> Option<&'static MenuAction> {
    MENU_ACTIONS.iter().find(|a| a.name == name)
}

/// The menu actions of the toggle `id`.
pub fn menu_actions_of(id: &str) -> Vec<&'static MenuAction> {
    MENU_ITEMS
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, names)| names.iter().filter_map(|n| menu_action(n)).collect())
        .unwrap_or_default()
}

/// All items for `os`, in page order. Items that cannot work there are listed anyway
/// (with the reason as their status) when users would look for them.
pub fn items(os: Os) -> Vec<Item> {
    let mut v = Vec::new();
    let mut add = |id, title: String, explain: String, group, kind, default_on| {
        v.push(Item {
            id,
            title,
            explain,
            group,
            kind,
            default_on,
        })
    };
    use Group::*;
    use Kind::*;
    add(
        "open-with",
        tr!("Offer in “Open with”"),
        match os {
            Os::Linux => tr!(
                "Adds the app to the list of apps for PDF documents (a launcher entry in your home folder, when no installed package provides one)."
            ),
            Os::Windows => tr!(
                "Registers the app for PDF documents for your user account, so it appears under “Open with” and in Default apps."
            ),
            Os::MacOs => tr!(
                "The app bundle declares the PDF documents it opens; macOS lists it under “Open With”."
            ),
        },
        Opening,
        Registration,
        false,
    );
    add(
        "default-pdf",
        tr!("Default app for PDF documents"),
        match os {
            Os::Linux => tr!(
                "Makes the app open PDF documents when you double-click them. Turning it off gives them back to the app that had them before."
            ),
            Os::Windows => tr!(
                "Windows lets only you choose default apps: this opens Settings at the app's entry."
            ),
            Os::MacOs => tr!(
                "Makes the app open PDF documents; macOS asks you to confirm. Turning it off gives them back to the previous app."
            ),
        },
        Opening,
        if os == Os::Windows {
            Button
        } else {
            Registration
        },
        false,
    );
    add(
        "type-fdf",
        tr!("Form data files (FDF, XFDF)"),
        tr!(
            "Opens form data exported by PDF apps: the app finds the PDF form it belongs to and fills it in."
        ),
        Opening,
        Registration,
        false,
    );
    add(
        "default-fdf",
        tr!("Default app for form data files"),
        match os {
            Os::Windows => {
                tr!(
                    "Windows lets only you choose default apps: this opens Settings at the app's entry."
                )
            }
            _ => tr!("Makes the app open FDF and XFDF files when you double-click them."),
        },
        Opening,
        if os == Os::Windows {
            Button
        } else {
            Registration
        },
        false,
    );
    add(
        "type-images",
        tr!("Convert images to PDF"),
        tr!(
            "Offers “Convert to PDF” for PNG and JPEG images; the PDF is written next to the first image."
        ),
        if os == Os::MacOs { Finder } else { Opening },
        Registration,
        false,
    );
    add(
        "thumbnails",
        tr!("Page thumbnails"),
        tr!(
            "Shows the first page of PDF documents as their icon in the file manager, drawn by this app."
        ),
        Previews,
        Registration,
        false,
    );
    add(
        "preview",
        tr!("Preview pane"),
        tr!("Shows PDF documents in the file manager's preview pane, drawn by this app."),
        Previews,
        Registration,
        false,
    );
    add(
        "search",
        tr!("Search inside PDF documents"),
        tr!("Lets the system's search find PDF documents by the words in them, read by this app."),
        Previews,
        Registration,
        false,
    );
    for (id, names) in MENU_ITEMS {
        let actions: Vec<&MenuAction> = names.iter().filter_map(|n| menu_action(n)).collect();
        let title = match id {
            "action-rotate" => tr!("Rotate right / left"),
            _ => pagebara_i18n::translate(actions[0].title, &[]),
        };
        let what = match id {
            "action-new-window" => tr!("Opens the selected documents in a window of their own."),
            "action-merge" => {
                tr!(
                    "Puts the selected documents into one PDF, in the order selected, next to the first."
                )
            }
            "action-split" => {
                tr!("Writes each page of the selected documents as a PDF of its own.")
            }
            "action-rotate" => tr!("Turns all pages of the selected documents, in place."),
            "action-compress" => {
                tr!("Writes a smaller copy of each selected document, when one can be made.")
            }
            "action-images" => tr!("Writes the pages of the selected documents as PNG images."),
            "action-ocr" => {
                tr!(
                    "Recognises the text of scanned pages and writes a searchable copy (needs the text recognition models)."
                )
            }
            "action-sign" => tr!("Opens the document ready to sign."),
            _ => tr!("Opens the document's print dialog."),
        };
        add(
            id,
            title,
            match os {
                Os::Windows => tr!(
                    "{} Adds the entry to Explorer's menu for PDF documents.",
                    what
                ),
                Os::MacOs => what,
                Os::Linux => tr!(
                    "{} Adds the entry to the file manager's menu for PDF documents.",
                    what
                ),
            },
            if os == Os::MacOs { Finder } else { FileManager },
            Registration,
            false,
        );
    }
    add(
        "send-to",
        tr!("“Send to” menu"),
        match os {
            Os::Windows => {
                tr!("Adds the app and “Merge into one PDF” to Explorer's “Send to” menu.")
            }
            _ => tr!("Adds the app and “Merge into one PDF” to Thunar's “Send To” menu."),
        },
        FileManager,
        Registration,
        false,
    );
    add(
        "recent",
        tr!("Recent files"),
        match os {
            Os::Linux => tr!(
                "Opened files appear in the system's recent files (file managers, file pickers). Turning it off removes the entries this app added."
            ),
            Os::Windows => tr!(
                "Opened files appear in the recent files of Start and the taskbar (Windows keeps them for all apps together)."
            ),
            Os::MacOs => tr!(
                "Opened files appear in the app's Open Recent list and in the Dock menu. Turning it off clears the app's list."
            ),
        },
        Window,
        Setting,
        true,
    );
    add(
        "jump-list",
        match os {
            Os::MacOs => tr!("Dock menu"),
            _ => tr!("Taskbar jump list"),
        },
        match os {
            Os::MacOs => tr!("The app's Dock menu shows the recent files and “New Window”."),
            Os::Windows => {
                tr!("Right-clicking the taskbar button shows the recent files and “New window”.")
            }
            Os::Linux => {
                tr!("Recent files and “New window” in the menu of the taskbar or dock button.")
            }
        },
        Window,
        Setting,
        true,
    );
    add(
        "single-instance",
        tr!("One running app"),
        tr!(
            "Files opened from the file manager go to the open window instead of starting the app again."
        ),
        Window,
        Setting,
        true,
    );
    add(
        "new-window",
        tr!("Open files in a new window"),
        tr!(
            "Files opened from outside the app open in a window of their own instead of a new tab."
        ),
        Window,
        Setting,
        false,
    );
    add(
        "restore-session",
        tr!("Reopen last documents"),
        tr!(
            "Starting the app reopens the documents that were open when it was closed, at the same page."
        ),
        Window,
        Setting,
        false,
    );
    add(
        "progress",
        tr!("Progress on the taskbar"),
        match os {
            Os::Linux => tr!(
                "Long jobs (text recognition, batch actions) show their progress on the app's launcher icon, where the desktop supports it."
            ),
            _ => tr!(
                "Long jobs (text recognition, batch actions) show their progress on the app's taskbar button."
            ),
        },
        Window,
        Setting,
        true,
    );
    add(
        "notifications",
        tr!("Notifications"),
        tr!(
            "A system notification says when a long job (text recognition, signing, batch actions) finishes while the window is in the background."
        ),
        Window,
        Setting,
        false,
    );
    if os == Os::MacOs {
        v.sort_by_key(|item| item.group);
    }
    v
}

pub fn item(os: Os, id: &str) -> Option<Item> {
    items(os).into_iter().find(|i| i.id == id)
}

/// Why `id` cannot work on `os` (shown as its status, through [`pagebara_i18n::translate`]),
/// or `None` when it can.
pub fn unavailable(os: Os, id: &str) -> Option<&'static str> {
    Some(match (os, id) {
        (Os::Linux, "preview") => {
            tr_noop!(
                "File managers on Linux have no add-on hook for preview panes; GNOME's previewer and KDE use the thumbnailer and their own PDF libraries."
            )
        }
        (Os::Linux, "search") => {
            tr_noop!(
                "GNOME's search (LocalSearch/Tracker) and KDE's Baloo read PDF text with their own built-in extractors and take no external ones."
            )
        }
        (Os::MacOs, "thumbnails" | "preview") => {
            tr_noop!(
                "macOS draws PDF thumbnails and previews itself; a Quick Look extension has to be built and signed with Xcode inside the app bundle."
            )
        }
        (Os::MacOs, "search") => {
            tr_noop!(
                "Spotlight already indexes the text of PDF documents; an importer would replace Apple's for no gain."
            )
        }
        (Os::MacOs, "type-fdf" | "default-fdf") => {
            tr_noop!(
                "Declared by the app bundle; choose the app for FDF files in Finder's Get Info."
            )
        }
        (Os::MacOs, "send-to") => {
            tr_noop!("macOS has no “Send to” menu; use the Quick Actions above.")
        }
        (Os::MacOs, "progress") => tr_noop!("Not shown on the Dock yet."),
        (Os::MacOs, "notifications") => {
            tr_noop!(
                "Notifications need a signed app bundle with the notification permission; not done yet."
            )
        }
        (Os::Windows, "notifications") => {
            tr_noop!(
                "Windows notifications need a Start menu shortcut with the app's ID and the WinRT toast API; not done yet."
            )
        }
        _ => return None,
    })
}

/// Not built, with the reason (listed by `--integration status`; see the crate docs).
pub const SKIPPED: [(&str, &str); 3] = [
    (
        "tray",
        "Tray / menu-bar icon: Slint and winit have no tray support; the tray-icon crate needs its own GTK event loop on Linux beside winit's.",
    ),
    (
        "protocol",
        "A pagebara:// link handler: file paths and file:// links already work everywhere, and a URL scheme would let any web page start the app with arguments.",
    ),
    (
        "type-portfolio",
        "PDF portfolios are PDF files (same type and extension), so the PDF registration covers them.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_unique_and_menu_items_resolve() {
        for os in [Os::Linux, Os::Windows, Os::MacOs] {
            let items = items(os);
            let mut ids: Vec<_> = items.iter().map(|i| i.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), items.len());
        }
        for (id, _) in MENU_ITEMS {
            assert!(!menu_actions_of(id).is_empty(), "{id}");
        }
    }
}
