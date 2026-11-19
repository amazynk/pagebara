//! Linux: launcher entries, the thumbnailer, file-manager menu entries (GNOME Files
//! scripts, Dolphin service menus, Nemo and Caja actions, Thunar custom actions) and
//! Thunar's Send To, all in the user's own folders.

use std::io;
use std::path::{Path, PathBuf};

use pagebara_identity::{APP_ID, NAME, SUMMARY};

use crate::catalog::{MenuAction, menu_actions_of};
use crate::env::Env;
use crate::plan::Op;
use crate::{NoPlan, Os};

pub const PDF_TYPES: [&str; 2] = ["application/pdf", "application/x-pdf"];
pub const FDF_TYPES: [&str; 2] = ["application/vnd.fdf", "application/vnd.adobe.xfdf"];
pub const IMAGE_TYPES: [&str; 2] = ["image/png", "image/jpeg"];

const DESKTOP_TEMPLATE: &str = include_str!("../../../packaging/linux/app.desktop.in");
const THUMBNAILER_TEMPLATE: &str = include_str!("../../../packaging/linux/app.thumbnailer.in");
const ICON: &[u8] = include_bytes!("../../../packaging/icons/app.svg");

/// A user `uca.xml` made from nothing.
pub const EMPTY_UCA: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n</actions>\n";

/// File managers with menus apps can add to, by program name.
const FILE_MANAGERS: [(&str, &str); 5] = [
    ("nautilus", "GNOME Files"),
    ("dolphin", "Dolphin"),
    ("nemo", "Nemo"),
    ("caja", "Caja"),
    ("thunar", "Thunar"),
];

/// One argument of a desktop file's `Exec` key, quoted as the Desktop Entry spec asks
/// (and with the backslashes of the string value doubled).
pub fn exec_arg(s: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    if !s.is_empty() && !s.chars().any(|c| RESERVED.contains(c)) {
        return s.to_owned();
    }
    let mut q = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    q.replace('\\', "\\\\")
}

/// A POSIX shell word.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn fill(template: &str) -> String {
    template
        .replace("@APP_NAME@", NAME)
        .replace("@APP_ID@", APP_ID)
        .replace("@APP_SUMMARY@", SUMMARY)
}

/// The launcher entry for a copy of the app outside a package: the template with the
/// executable's full path.
pub fn desktop_file(exe: &Path) -> String {
    let exe_s = exe.to_string_lossy();
    let mut out = String::new();
    for line in fill(DESKTOP_TEMPLATE).lines() {
        if let Some(rest) = line.strip_prefix("Exec=@APP_BIN@") {
            out.push_str(&format!("Exec={}{rest}\n", exec_arg(&exe_s)));
        } else {
            out.push_str(line);
            out.push('\n');
        }
        if line == "Type=Application" {
            out.push_str(&format!("TryExec={}\n", exec_arg(&exe_s)));
        }
    }
    out
}

/// A hidden launcher entry: offered under "Open with" for `types` only.
fn hidden_entry(exe: &Path, name: &str, args: &[&str], types: &[&str]) -> String {
    let mut exec = exec_arg(&exe.to_string_lossy());
    for a in args {
        exec.push(' ');
        exec.push_str(&exec_arg(a));
    }
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nIcon={APP_ID}\nExec={exec} %F\n\
         TryExec={}\nTerminal=false\nNoDisplay=true\nMimeType={};\n",
        exec_arg(&exe.to_string_lossy()),
        types.join(";")
    )
}

pub fn thumbnailer(exe: &Path) -> String {
    let exe_s = exe.to_string_lossy();
    fill(THUMBNAILER_TEMPLATE)
        .lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("TryExec=@APP_BIN@") {
                format!("TryExec={exe_s}{rest}\n")
            } else if let Some(rest) = l.strip_prefix("Exec=@APP_BIN@") {
                format!("Exec={}{rest}\n", exec_arg(&exe_s))
            } else {
                format!("{l}\n")
            }
        })
        .collect()
}

/// Inserts `block` before `</actions>`.
pub fn xml_insert(text: &str, block: &str) -> Option<String> {
    let at = text.rfind("</actions>")?;
    Some(format!("{}{block}{}", &text[..at], &text[at..]))
}

/// Removes the `<action>` holding `<unique-id>id</unique-id>` (and its line break).
pub fn xml_remove(text: &str, id: &str) -> String {
    let marker = format!("<unique-id>{}</unique-id>", xml_escape(id));
    let mut out = text.to_owned();
    while let Some(m) = out.find(&marker) {
        let Some(start) = out[..m].rfind("<action>") else {
            break;
        };
        let Some(end) = out[m..]
            .find("</action>")
            .map(|e| m + e + "</action>".len())
        else {
            break;
        };
        // The whole lines, indentation and line break included.
        let start = out[..start].rfind('\n').map_or(0, |n| n + 1);
        let end = if out[end..].starts_with('\n') {
            end + 1
        } else {
            end
        };
        out.replace_range(start..end, "");
    }
    out
}

/// Removes `ty=app` from the `[Default Applications]` (and `[Added Associations]`
/// leftovers are left alone) of a `mimeapps.list`.
pub fn remove_mime_default(list: &Path, ty: &str, app: &str) -> io::Result<()> {
    let Ok(text) = std::fs::read_to_string(list) else {
        return Ok(());
    };
    let mut section = String::new();
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.to_owned();
        }
        if section == "[Default Applications]"
            && let Some((k, v)) = t.split_once('=')
            && k.trim() == ty
        {
            let rest: Vec<&str> = v
                .split(';')
                .filter(|a| !a.is_empty() && *a != app)
                .collect();
            if !rest.is_empty() {
                out.push_str(&format!("{ty}={};\n", rest.join(";")));
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out != text {
        std::fs::write(list, out)?;
    }
    Ok(())
}

/// The installed package's file `rel` (under one of `XDG_DATA_DIRS`).
fn packaged(env: &Env, rel: &str) -> Option<PathBuf> {
    env.data_dirs
        .iter()
        .map(|d| d.join(rel))
        .find(|p| p.is_file())
}

fn desktop_id(suffix: &str) -> String {
    format!("{APP_ID}{suffix}.desktop")
}

/// The app's launcher entry exists (packaged or ours).
pub fn launcher_known(env: &Env) -> bool {
    packaged(env, &format!("applications/{}", desktop_id(""))).is_some()
}

fn file(path: PathBuf, contents: impl Into<Vec<u8>>, executable: bool) -> Op {
    Op::File {
        path,
        contents: contents.into(),
        executable,
    }
}

pub fn plan(env: &Env, id: &str) -> Result<Vec<Op>, NoPlan> {
    if env.flatpak {
        return Err(NoPlan::Unavailable(pagebara_i18n::tr!(
            "Not available in the Flatpak version: the sandbox cannot register with the desktop. \
             Choose the default app in your system settings."
        )));
    }
    let apps = env.data_home.join("applications");
    let exe = &env.exe;
    Ok(match id {
        "open-with" => {
            if let Some(p) = packaged(env, &format!("applications/{}", desktop_id(""))) {
                return Err(NoPlan::Provided(pagebara_i18n::tr!(
                    "Registered by the installed package ({}).",
                    p.display()
                )));
            }
            vec![
                file(apps.join(desktop_id("")), desktop_file(exe), false),
                file(
                    env.data_home
                        .join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")),
                    ICON,
                    false,
                ),
                Op::UpdateDesktopDatabase(apps),
            ]
        }
        "default-pdf" | "default-fdf" => {
            env.defaults.available().map_err(NoPlan::Unavailable)?;
            let (types, app) = if id == "default-pdf" {
                (&PDF_TYPES, desktop_id(""))
            } else {
                (&FDF_TYPES, desktop_id("-forms"))
            };
            vec![Op::Default {
                types: types.iter().map(|t| t.to_string()).collect(),
                app,
            }]
        }
        "type-fdf" => vec![
            file(
                apps.join(desktop_id("-forms")),
                hidden_entry(exe, NAME, &[], &FDF_TYPES),
                false,
            ),
            Op::UpdateDesktopDatabase(apps),
        ],
        "type-images" => vec![
            file(
                apps.join(desktop_id("-convert")),
                hidden_entry(
                    exe,
                    &format!("Convert to PDF ({NAME})"),
                    &["--action", "convert"],
                    &IMAGE_TYPES,
                ),
                false,
            ),
            Op::UpdateDesktopDatabase(apps),
        ],
        "thumbnails" => {
            if let Some(p) = packaged(env, &format!("thumbnailers/{APP_ID}.thumbnailer")) {
                return Err(NoPlan::Provided(pagebara_i18n::tr!(
                    "Installed by the package ({}).",
                    p.display()
                )));
            }
            vec![file(
                env.data_home
                    .join(format!("thumbnailers/{APP_ID}.thumbnailer")),
                thumbnailer(exe),
                false,
            )]
        }
        "send-to" => {
            if env.find("thunar").is_none() {
                return Err(NoPlan::Unavailable(pagebara_i18n::tr!(
                    "Only Thunar has a “Send To” menu that apps can add to; use the file manager menu entries instead."
                )));
            }
            let dir = env.data_home.join("Thunar/sendto");
            vec![
                file(
                    dir.join(desktop_id("")),
                    hidden_entry(exe, NAME, &[], &PDF_TYPES).replace("NoDisplay=true\n", ""),
                    false,
                ),
                file(
                    dir.join(desktop_id("-merge")),
                    hidden_entry(
                        exe,
                        &format!("Merge into one PDF ({NAME})"),
                        &["--action", "merge"],
                        &PDF_TYPES,
                    )
                    .replace("NoDisplay=true\n", ""),
                    false,
                ),
            ]
        }
        _ if id.starts_with("action-") => {
            let actions = menu_actions_of(id);
            if actions.is_empty() {
                return Err(NoPlan::Unavailable("unknown item".into()));
            }
            let found: Vec<&str> = FILE_MANAGERS
                .iter()
                .filter(|(p, _)| env.find(p).is_some())
                .map(|(p, _)| *p)
                .collect();
            if found.is_empty() {
                return Err(NoPlan::Unavailable(pagebara_i18n::tr!(
                    "No supported file manager found ({}).",
                    FILE_MANAGERS.map(|f| f.1).join(", ")
                )));
            }
            let mut ops = Vec::new();
            for a in actions {
                for fm in &found {
                    ops.push(menu_entry(env, fm, a));
                }
            }
            ops
        }
        _ => {
            return Err(NoPlan::Unavailable(pagebara_i18n::translate(
                crate::catalog::unavailable(Os::Linux, id)
                    .unwrap_or(pagebara_i18n::tr_noop!("Not available on Linux.")),
                &[],
            )));
        }
    })
}

/// The command line of a menu action, as POSIX shell words.
fn sh_command(exe: &Path, a: &MenuAction) -> String {
    let mut c = sh_quote(&exe.to_string_lossy());
    for arg in a.args {
        c.push(' ');
        c.push_str(&sh_quote(arg));
    }
    c
}

fn exec_command(exe: &Path, a: &MenuAction) -> String {
    let mut c = exec_arg(&exe.to_string_lossy());
    for arg in a.args {
        c.push(' ');
        c.push_str(&exec_arg(arg));
    }
    c
}

fn menu_entry(env: &Env, fm: &str, a: &MenuAction) -> Op {
    let exe = &env.exe;
    let key = format!("{APP_ID}-{}", a.name);
    let pdf = PDF_TYPES.join(";");
    match fm {
        // Scripts get the selected files as arguments; they appear under Scripts > NAME.
        "nautilus" => file(
            env.data_home
                .join("nautilus/scripts")
                .join(NAME)
                .join(a.title),
            format!(
                "#!/bin/sh\n# Added by {NAME} (Settings > System integration), which removes it again.\nexec {} \"$@\"\n",
                sh_command(exe, a)
            ),
            true,
        ),
        // Dolphin runs service menus outside the system folders only when executable.
        "dolphin" => file(
            env.data_home
                .join("kio/servicemenus")
                .join(format!("{key}.desktop")),
            format!(
                "[Desktop Entry]\nType=Service\nMimeType={pdf};\nActions={name};\nX-KDE-Submenu={NAME}\n{}\n\
                 [Desktop Action {name}]\nName={title}\nIcon={APP_ID}\nExec={} %F\n",
                if a.many {
                    ""
                } else {
                    "X-KDE-MaxNumberOfUrls=1"
                },
                exec_command(exe, a),
                name = a.name,
                title = a.title,
            )
            .replace("\n\n[", "\n["),
            true,
        ),
        "nemo" => file(
            env.data_home
                .join("nemo/actions")
                .join(format!("{key}.nemo_action")),
            format!(
                "[Nemo Action]\nName={}\nComment={NAME}\nExec={} %F\nIcon-Name={APP_ID}\n\
                 Selection={}\nExtensions=pdf;\nMimetypes={pdf};\n",
                a.title,
                exec_command(exe, a),
                if a.many { "notnone" } else { "s" },
            ),
            false,
        ),
        // caja-actions reads the FileManager-Actions format.
        "caja" => file(
            env.data_home
                .join("file-manager/actions")
                .join(format!("{key}.desktop")),
            format!(
                "[Desktop Entry]\nType=Action\nName={}\nTooltip={NAME}\nIcon={APP_ID}\nProfiles=main;\n\n\
                 [X-Action-Profile main]\nName={}\nExec={} %F\nMimeTypes={pdf};\nSelectionCount={}\n",
                a.title,
                a.title,
                exec_command(exe, a),
                if a.many { ">0" } else { "=1" },
            ),
            false,
        ),
        _ => {
            let path = env.config_home.join("Thunar/uca.xml");
            let template = env
                .config_dirs
                .iter()
                .map(|d| d.join("Thunar/uca.xml"))
                .find(|p| p.is_file());
            let block = format!(
                "<action>\n\t<icon>{APP_ID}</icon>\n\t<name>{}</name>\n\t<submenu>{}</submenu>\n\t\
                 <unique-id>{}</unique-id>\n\t<command>{} {}</command>\n\t<description>{}</description>\n\t\
                 <range>{}</range>\n\t<patterns>*.pdf;*.PDF</patterns>\n\t<other-files/>\n</action>\n",
                xml_escape(a.title),
                xml_escape(NAME),
                xml_escape(&key),
                xml_escape(&sh_command(exe, a)),
                if a.many { "%F" } else { "%f" },
                xml_escape(NAME),
                if a.many { "*" } else { "1" },
            );
            Op::XmlAction {
                path,
                template,
                id: key,
                block,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quoting() {
        assert_eq!(exec_arg("/usr/bin/pagebara"), "/usr/bin/pagebara");
        assert_eq!(exec_arg("/opt/my app/pagebara"), "\"/opt/my app/pagebara\"");
        assert_eq!(exec_arg("/a$b"), "\"/a\\\\$b\"");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn desktop_file_from_the_template() {
        let d = desktop_file(Path::new("/home/u/Apps/My App.AppImage"));
        assert!(
            d.contains("Exec=\"/home/u/Apps/My App.AppImage\" %U\n"),
            "{d}"
        );
        assert!(d.contains("Exec=\"/home/u/Apps/My App.AppImage\" --new-window\n"));
        assert!(d.contains(&format!("Icon={APP_ID}\n")));
        assert!(!d.contains('@'));
        let t = thumbnailer(Path::new("/x/pagebara"));
        assert!(t.contains("Exec=/x/pagebara --thumbnail %i %o %s"), "{t}");
    }

    #[test]
    fn uca_merge_and_removal_keep_the_rest() {
        let user = "<?xml version=\"1.0\"?>\n<actions>\n<action>\n\t<name>Terminal</name>\n\t<unique-id>1-1</unique-id>\n</action>\n</actions>\n";
        let block = "<action>\n\t<unique-id>x-merge</unique-id>\n</action>\n";
        let merged = xml_insert(user, block).unwrap();
        assert!(merged.contains("1-1") && merged.contains("x-merge"));
        assert_eq!(xml_remove(&merged, "x-merge"), user);
        assert_eq!(xml_remove(user, "x-merge"), user);
    }

    #[test]
    fn mimeapps_removal() {
        let p = std::env::temp_dir().join(format!("pagebara-mimeapps-{}", std::process::id()));
        std::fs::write(
            &p,
            "[Added Associations]\napplication/pdf=a.desktop;\n[Default Applications]\napplication/pdf=x.desktop;b.desktop;\nimage/png=x.desktop\n",
        )
        .unwrap();
        remove_mime_default(&p, "application/pdf", "x.desktop").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("application/pdf=b.desktop;\n"));
        assert!(t.contains("application/pdf=a.desktop;"));
        assert!(t.contains("image/png=x.desktop"));
        std::fs::remove_file(p).unwrap();
    }
}
