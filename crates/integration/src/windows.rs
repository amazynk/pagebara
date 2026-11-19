//! Windows: per-user registrations under `HKCU\Software\Classes` (ProgIDs, "Open with",
//! Default apps capabilities, context-menu verbs, the Explorer extensions of
//! `pagebara-shell-win`), the machine-wide Windows Search filter, and Send To shortcuts.

use std::path::Path;

use pagebara_identity::{APP_ID, NAME, SUMMARY, guid_string, windows_clsid};

use crate::catalog::menu_actions_of;
use crate::env::Env;
use crate::plan::{Op, RegValue};
use crate::registry::Root;
use crate::{NoPlan, Os};

const CLASSES: &str = r"Software\Classes";
/// Shell extension slots under `.pdf\ShellEx`.
const THUMBNAIL_SLOT: &str = "{e357fccd-a995-4576-b01f-234630154e96}";
const PREVIEW_SLOT: &str = "{8895b1c6-b41f-4c1c-a562-0d564250836f}";
/// The 64-bit preview host process (prevhost.exe).
const PREVIEW_HOST: &str = "{6d2b5079-2f0b-48dd-ab7f-97cec514d30b}";
/// `IFilter`'s interface ID, under a persistent handler.
const IFILTER: &str = "{89BCB740-6119-101A-BCB7-00DD010655AF}";

struct Plan {
    ops: Vec<Op>,
    root: Root,
}

impl Plan {
    fn new(root: Root) -> Plan {
        Plan {
            ops: Vec::new(),
            root,
        }
    }

    fn own(&mut self, key: &str) -> &mut Plan {
        self.ops.push(Op::OwnKey {
            root: self.root,
            key: key.to_owned(),
        });
        self
    }

    fn s(&mut self, key: &str, name: &str, value: &str) -> &mut Plan {
        self.ops.push(Op::Reg {
            root: self.root,
            key: key.to_owned(),
            name: name.to_owned(),
            value: RegValue::String(value.to_owned()),
        });
        self
    }
}

/// `"C:\…\pagebara.exe" args "%1"`.
fn command(exe: &str, args: &[&str]) -> String {
    let mut c = format!("\"{exe}\"");
    for a in args {
        c.push(' ');
        c.push_str(a);
    }
    c.push_str(" \"%1\"");
    c
}

fn exe_name(exe: &Path) -> String {
    exe.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{NAME}.exe"))
}

/// A ProgID opening files with the app.
fn prog_id(p: &mut Plan, id: &str, description: &str, exe: &str) {
    let k = format!(r"{CLASSES}\{id}");
    p.own(&k)
        .s(&k, "", description)
        .s(&k, "FriendlyTypeName", description)
        .s(&format!(r"{k}\DefaultIcon"), "", &format!("{exe},0"))
        .s(&format!(r"{k}\shell\open\command"), "", &command(exe, &[]));
}

pub fn plan(env: &Env, id: &str) -> Result<Vec<Op>, NoPlan> {
    let exe = env.exe.to_string_lossy().into_owned();
    let dll = env.shell_dll.to_string_lossy().into_owned();
    let caps = format!(r"Software\{NAME}\Capabilities");
    let apps = format!(r"{CLASSES}\Applications\{}", exe_name(&env.exe));
    let need_dll = || {
        if env.shell_dll.is_file() {
            Ok(())
        } else {
            Err(NoPlan::Unavailable(pagebara_i18n::tr!(
                "The Explorer add-on ({}) is not installed next to the app.",
                pagebara_identity::SHELL_DLL
            )))
        }
    };
    let mut p = Plan::new(Root::CurrentUser);
    match id {
        "open-with" => {
            let pdf = format!("{APP_ID}.pdf");
            prog_id(&mut p, &pdf, "PDF document", &exe);
            p.s(&format!(r"{CLASSES}\.pdf\OpenWithProgids"), &pdf, "")
                .own(&apps)
                .s(&apps, "FriendlyAppName", NAME)
                .s(&format!(r"{apps}\SupportedTypes"), ".pdf", "")
                .s(&format!(r"{apps}\DefaultIcon"), "", &format!("{exe},0"))
                .s(
                    &format!(r"{apps}\shell\open\command"),
                    "",
                    &command(&exe, &[]),
                )
                .own(&format!(r"Software\{NAME}"))
                .s(&caps, "ApplicationName", NAME)
                .s(&caps, "ApplicationDescription", SUMMARY)
                .s(&caps, "ApplicationIcon", &format!("{exe},0"))
                .s(&format!(r"{caps}\FileAssociations"), ".pdf", &pdf)
                .s(
                    &format!(r"{caps}\MimeAssociations"),
                    "application/pdf",
                    &pdf,
                )
                .s(r"Software\RegisteredApplications", NAME, &caps);
        }
        "default-pdf" | "default-fdf" => {
            return Err(NoPlan::Unavailable(pagebara_i18n::tr!(
                "Windows lets only you choose default apps."
            )));
        }
        "type-fdf" => {
            let fdf = format!("{APP_ID}.fdf");
            prog_id(&mut p, &fdf, "PDF form data", &exe);
            for ext in [".fdf", ".xfdf"] {
                p.s(&format!(r"{CLASSES}\{ext}\OpenWithProgids"), &fdf, "")
                    .s(&format!(r"{apps}\SupportedTypes"), ext, "")
                    .s(&format!(r"{caps}\FileAssociations"), ext, &fdf);
            }
        }
        "type-images" => {
            for ext in ["png", "jpg", "jpeg"] {
                let k = format!(r"{CLASSES}\SystemFileAssociations\.{ext}\shell\{APP_ID}.convert");
                p.own(&k)
                    .s(&k, "MUIVerb", "Convert to PDF")
                    .s(&k, "Icon", &format!("{exe},0"))
                    .s(&k, "MultiSelectModel", "Player")
                    .s(
                        &format!(r"{k}\command"),
                        "",
                        &command(&exe, &["--action", "convert", "--collect"]),
                    );
            }
        }
        "thumbnails" => {
            need_dll()?;
            let clsid = guid_string(windows_clsid("thumbnail"));
            let k = format!(r"{CLASSES}\CLSID\{clsid}");
            p.own(&k)
                .s(&k, "", &format!("{NAME} thumbnails"))
                .s(&format!(r"{k}\InprocServer32"), "", &dll)
                .s(
                    &format!(r"{k}\InprocServer32"),
                    "ThreadingModel",
                    "Apartment",
                )
                .s(
                    &format!(r"{CLASSES}\.pdf\ShellEx\{THUMBNAIL_SLOT}"),
                    "",
                    &clsid,
                );
        }
        "preview" => {
            need_dll()?;
            let clsid = guid_string(windows_clsid("preview"));
            let k = format!(r"{CLASSES}\CLSID\{clsid}");
            let name = format!("{NAME} preview");
            p.own(&k)
                .s(&k, "", &name)
                .s(&k, "DisplayName", &name)
                .s(&k, "AppID", PREVIEW_HOST)
                .s(&format!(r"{k}\InprocServer32"), "", &dll)
                .s(
                    &format!(r"{k}\InprocServer32"),
                    "ThreadingModel",
                    "Apartment",
                )
                .s(
                    &format!(r"{CLASSES}\.pdf\ShellEx\{PREVIEW_SLOT}"),
                    "",
                    &clsid,
                )
                .s(
                    r"Software\Microsoft\Windows\CurrentVersion\PreviewHandlers",
                    &clsid,
                    &name,
                );
        }
        // The indexer runs as a service and reads machine-wide registrations only.
        "search" => {
            need_dll()?;
            let mut m = Plan::new(Root::LocalMachine);
            let filter = guid_string(windows_clsid("filter"));
            let handler = guid_string(windows_clsid("persistent-handler"));
            let f = format!(r"{CLASSES}\CLSID\{filter}");
            let h = format!(r"{CLASSES}\CLSID\{handler}");
            m.own(&f)
                .s(&f, "", &format!("{NAME} text filter"))
                .s(&format!(r"{f}\InprocServer32"), "", &dll)
                .s(&format!(r"{f}\InprocServer32"), "ThreadingModel", "Both")
                .own(&h)
                .s(&h, "", &format!("{NAME} persistent handler"))
                .s(
                    &format!(r"{h}\PersistentAddinsRegistered\{IFILTER}"),
                    "",
                    &filter,
                )
                .s(&format!(r"{CLASSES}\.pdf\PersistentHandler"), "", &handler);
            return Ok(m.ops);
        }
        "send-to" => {
            let dir = env.appdata.join(r"Microsoft\Windows\SendTo");
            return Ok(vec![
                Op::File {
                    path: dir.join(format!("{NAME}.lnk")),
                    contents: crate::lnk::shortcut(&exe, "", &format!("Open with {NAME}")),
                    executable: false,
                },
                Op::File {
                    path: dir.join(format!("Merge into one PDF ({NAME}).lnk")),
                    contents: crate::lnk::shortcut(&exe, "--action merge", "Merge into one PDF"),
                    executable: false,
                },
            ]);
        }
        _ if id.starts_with("action-") => {
            for a in menu_actions_of(id) {
                let k = format!(
                    r"{CLASSES}\SystemFileAssociations\.pdf\shell\{APP_ID}.{}",
                    a.name
                );
                // Explorer starts one process per file: those of merge gather in one.
                let mut args = a.args.to_vec();
                if a.multi && args.contains(&"--action") {
                    args.push("--collect");
                }
                p.own(&k)
                    .s(&k, "MUIVerb", a.title)
                    .s(&k, "Icon", &format!("{exe},0"))
                    .s(
                        &k,
                        "MultiSelectModel",
                        if a.many { "Player" } else { "Single" },
                    )
                    .s(&format!(r"{k}\command"), "", &command(&exe, &args));
            }
            if p.ops.is_empty() {
                return Err(NoPlan::Unavailable("unknown item".into()));
            }
        }
        _ => {
            return Err(NoPlan::Unavailable(pagebara_i18n::translate(
                crate::catalog::unavailable(Os::Windows, id)
                    .unwrap_or(pagebara_i18n::tr_noop!("Not available on Windows.")),
                &[],
            )));
        }
    }
    Ok(p.ops)
}
