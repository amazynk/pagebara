//! Each registration against a sandbox folder (never the user's own): turned on, checked,
//! turned off, and nothing of ours left behind — while what was there before stays.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pagebara_identity::{APP_ID, NAME, guid_string, windows_clsid};
use pagebara_integration::registry::{FileRegistry, Registry, Root, Value};
use pagebara_integration::{Env, Integration, Os, Status};

fn sandbox(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pagebara-int-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Every file under `dir`.
fn files(dir: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p);
            }
        }
    }
    out
}

/// A folder of stand-in programs (only their presence counts).
fn fake_path(dir: &Path, programs: &[&str]) -> Vec<PathBuf> {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for p in programs {
        std::fs::write(bin.join(p), "").unwrap();
    }
    vec![bin]
}

fn linux(dir: &Path, programs: &[&str]) -> Integration {
    let mut env = Env::sandbox(dir, Os::Linux, PathBuf::from("/opt/My Apps/pagebara"));
    // The real xdg-mime (by full path), but no file managers other than the stand-ins.
    let xdg = env.find("xdg-mime").unwrap_or_else(|| "xdg-mime".into());
    env.path = fake_path(dir, programs);
    env.defaults = Box::new(pagebara_integration::XdgDefaults {
        xdg: pagebara_os::default_app::XdgMime {
            program: xdg,
            envs: env.child_env.clone(),
        },
        mimeapps: env.config_home.join("mimeapps.list"),
        path: env.path.clone(),
    });
    Integration::new(env)
}

#[test]
fn linux_launcher_thumbnailer_and_types() {
    let dir = sandbox("linux");
    let mut i = linux(&dir, &[]);
    let before = files(&dir);
    for id in ["open-with", "thumbnails", "type-fdf", "type-images"] {
        assert_eq!(i.status(id), Status::Off, "{id}");
        i.enable(id).unwrap();
        assert_eq!(i.status(id), Status::On, "{id}");
    }
    let apps = dir.join("xdg/data/applications");
    let desktop = std::fs::read_to_string(apps.join(format!("{APP_ID}.desktop"))).unwrap();
    assert!(
        desktop.contains("Exec=\"/opt/My Apps/pagebara\" %U"),
        "{desktop}"
    );
    let thumb =
        std::fs::read_to_string(dir.join(format!("xdg/data/thumbnailers/{APP_ID}.thumbnailer")))
            .unwrap();
    assert!(thumb.contains("--thumbnail %i %o %s"));
    let convert = std::fs::read_to_string(apps.join(format!("{APP_ID}-convert.desktop"))).unwrap();
    assert!(convert.contains("--action convert %F") && convert.contains("image/png"));
    for f in [&desktop, &convert] {
        validate_desktop(&dir, f);
    }
    // A file removed by hand shows as partly removed; turning on again repairs it.
    std::fs::remove_file(dir.join(format!("xdg/data/icons/hicolor/scalable/apps/{APP_ID}.svg")))
        .unwrap();
    assert!(matches!(i.status("open-with"), Status::Partial(_)));
    i.enable("open-with").unwrap();
    assert_eq!(i.status("open-with"), Status::On);
    // The app moved: the registration is out of date.
    let mut moved = linux(&dir, &[]);
    moved.env.exe = PathBuf::from("/usr/local/bin/pagebara");
    assert_eq!(moved.status("thumbnails"), Status::Outdated);
    i.remove_all().unwrap();
    assert_eq!(files(&dir), before);
    assert!(!dir.join("integration.json").exists());
    assert!(!dir.join("xdg/data/thumbnailers").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

fn validate_desktop(dir: &Path, text: &str) {
    let p = dir.join("check.desktop");
    std::fs::write(&p, text).unwrap();
    let out = std::process::Command::new("desktop-file-validate")
        .arg(&p)
        .output();
    // Removed before anything else: the test compares the folder's files afterwards, also
    // where the validator is not installed.
    std::fs::remove_file(&p).unwrap();
    let Ok(out) = out else {
        return;
    };
    let msg = String::from_utf8_lossy(&out.stdout).to_string();
    // The file name is not the app ID here; everything else must pass.
    let problems: Vec<&str> = msg
        .lines()
        .filter(|l| !l.contains("check.desktop: hint") && !l.trim().is_empty())
        .collect();
    assert!(problems.is_empty(), "{msg}\n{text}");
}

#[test]
fn linux_default_app_comes_back() {
    let dir = sandbox("default");
    let mut i = linux(&dir, &[]);
    if i.env.defaults.available().is_err() {
        eprintln!("skipped: no xdg-mime");
        return;
    }
    let config = dir.join("xdg/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("mimeapps.list"),
        "[Default Applications]\napplication/pdf=other.desktop\n",
    )
    .unwrap();
    let apps = dir.join("xdg/data/applications");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::write(
        apps.join("other.desktop"),
        "[Desktop Entry]\nType=Application\nName=o\nExec=/bin/sh %U\nMimeType=application/pdf;\n",
    )
    .unwrap();
    assert_eq!(i.status("default-pdf"), Status::Off);
    // Needs the launcher entry: turned on with it.
    let out = i.enable("default-pdf").unwrap();
    assert_eq!(out.changed, ["open-with", "default-pdf"]);
    assert_eq!(i.status("default-pdf"), Status::On);
    let list = std::fs::read_to_string(config.join("mimeapps.list")).unwrap();
    assert!(
        list.contains(&format!("application/pdf={APP_ID}.desktop")),
        "{list}"
    );
    // Turning the launcher entry off turns the default off too: PDFs go back.
    i.disable("open-with").unwrap();
    assert_eq!(i.status("default-pdf"), Status::Off);
    let list = std::fs::read_to_string(config.join("mimeapps.list")).unwrap();
    assert!(list.contains("application/pdf=other.desktop"), "{list}");
    assert!(!list.contains(APP_ID), "{list}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn linux_file_manager_menus() {
    let dir = sandbox("menus");
    let i = linux(&dir, &[]);
    assert!(matches!(i.status("action-merge"), Status::Unavailable(_)));
    let mut i2 = linux(&dir, &["nautilus", "dolphin", "nemo", "caja", "thunar"]);
    // Thunar's own file has the user's actions: they stay.
    let uca = dir.join("xdg/config/Thunar/uca.xml");
    std::fs::create_dir_all(uca.parent().unwrap()).unwrap();
    let user = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n<action>\n\t<name>Open Terminal Here</name>\n\t<unique-id>1-1</unique-id>\n\t<command>exo-open %f</command>\n</action>\n</actions>\n";
    std::fs::write(&uca, user).unwrap();
    let before = files(&dir);
    for id in ["action-merge", "action-rotate", "action-sign", "send-to"] {
        i2.enable(id).unwrap();
        assert_eq!(i2.status(id), Status::On, "{id}");
    }
    let script = dir.join(format!(
        "xdg/data/nautilus/scripts/{NAME}/Merge into one PDF"
    ));
    let s = std::fs::read_to_string(&script).unwrap();
    assert!(
        s.contains("exec '/opt/My Apps/pagebara' '--action' 'merge' \"$@\""),
        "{s}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&script).unwrap().permissions().mode() & 0o111,
            0o111
        );
    }
    let kio = dir.join(format!(
        "xdg/data/kio/servicemenus/{APP_ID}-rotate-left.desktop"
    ));
    let k = std::fs::read_to_string(kio).unwrap();
    assert!(
        k.contains("Exec=\"/opt/My Apps/pagebara\" --action rotate-left %F"),
        "{k}"
    );
    let sign = std::fs::read_to_string(
        dir.join(format!("xdg/data/nemo/actions/{APP_ID}-sign.nemo_action")),
    )
    .unwrap();
    assert!(sign.contains("Selection=s\n"));
    assert!(
        dir.join(format!(
            "xdg/data/file-manager/actions/{APP_ID}-merge.desktop"
        ))
        .is_file()
    );
    assert!(
        dir.join(format!("xdg/data/Thunar/sendto/{APP_ID}-merge.desktop"))
            .is_file()
    );
    let merged = std::fs::read_to_string(&uca).unwrap();
    assert!(merged.contains("Open Terminal Here"));
    assert!(merged.contains(&format!("<unique-id>{APP_ID}-merge</unique-id>")));
    assert!(
        merged.contains("&apos;")
            || merged.contains("'/opt/My Apps/pagebara' '--action' 'merge' %F")
    );
    // One item off removes only its entries.
    i2.disable("action-rotate").unwrap();
    assert_eq!(i2.status("action-rotate"), Status::Off);
    assert_eq!(i2.status("action-merge"), Status::On);
    assert!(
        !std::fs::read_to_string(&uca)
            .unwrap()
            .contains("rotate-left")
    );
    i2.remove_all().unwrap();
    assert_eq!(files(&dir), before);
    assert!(!dir.join("xdg/data/nautilus").exists() && !dir.join("xdg/data/kio").exists());
    assert_eq!(std::fs::read_to_string(&uca).unwrap(), user);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn linux_packaged_items_are_provided() {
    let dir = sandbox("packaged");
    let mut env = Env::sandbox(&dir, Os::Linux, PathBuf::from("/usr/bin/pagebara"));
    let share = dir.join("usr/share");
    std::fs::create_dir_all(share.join("applications")).unwrap();
    std::fs::write(share.join(format!("applications/{APP_ID}.desktop")), "x").unwrap();
    env.data_dirs = vec![share];
    let mut i = Integration::new(env);
    assert!(matches!(i.status("open-with"), Status::Provided(_)));
    assert!(i.enable("open-with").unwrap().changed.is_empty());
    let mut flat = Env::sandbox(&dir, Os::Linux, PathBuf::from("/app/bin/pagebara"));
    flat.flatpak = true;
    assert!(matches!(
        Integration::new(flat).status("thumbnails"),
        Status::Unavailable(_)
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

fn windows(dir: &Path) -> Integration {
    let exe = dir
        .join("Program Files")
        .join(NAME)
        .join(format!("{NAME}.exe"));
    let env = Env::sandbox(dir, Os::Windows, exe);
    Integration::new(env)
}

fn reg(dir: &Path) -> FileRegistry {
    FileRegistry::open(dir.join("registry.json"))
}

#[test]
fn windows_registry_as_data() {
    let dir = sandbox("windows");
    let mut i = windows(&dir);
    // Another app's thumbnail handler and search filter were there first.
    let classes = r"Software\Classes";
    let slot = format!(r"{classes}\.pdf\ShellEx\{{e357fccd-a995-4576-b01f-234630154e96}}");
    let theirs = Value::String("{00000000-1111-2222-3333-444444444444}".into());
    i.env
        .registry
        .set(Root::CurrentUser, &slot, "", &theirs)
        .unwrap();
    i.env
        .registry
        .set(
            Root::CurrentUser,
            &format!(r"{classes}\.pdf"),
            "",
            &Value::String("AcroExch.Document".into()),
        )
        .unwrap();
    let before = reg(&dir).dump();
    // No DLL next to the exe: the extensions cannot be registered.
    assert!(matches!(i.status("thumbnails"), Status::Unavailable(_)));
    std::fs::create_dir_all(i.env.shell_dll.parent().unwrap()).unwrap();
    std::fs::write(&i.env.shell_dll, "").unwrap();
    assert_eq!(i.status("default-pdf"), Status::Button);
    for id in [
        "type-fdf",
        "thumbnails",
        "preview",
        "action-merge",
        "action-print",
        "type-images",
        "search",
    ] {
        i.enable(id).unwrap();
        assert_eq!(i.status(id), Status::On, "{id}");
    }
    // type-fdf needs the app's registration.
    assert_eq!(i.status("open-with"), Status::On);
    let r = reg(&dir);
    let exe = i.env.exe.to_string_lossy().into_owned();
    let thumb = guid_string(windows_clsid("thumbnail"));
    assert_eq!(
        r.get(Root::CurrentUser, &slot, ""),
        Some(Value::String(thumb.clone()))
    );
    assert_eq!(
        r.get(
            Root::CurrentUser,
            &format!(r"{classes}\CLSID\{thumb}\InprocServer32"),
            ""
        ),
        Some(Value::String(
            i.env.shell_dll.to_string_lossy().into_owned()
        ))
    );
    let merge = format!(r"{classes}\SystemFileAssociations\.pdf\shell\{APP_ID}.merge\command");
    assert_eq!(
        r.get(Root::CurrentUser, &merge, ""),
        Some(Value::String(format!(
            "\"{exe}\" --action merge --collect \"%1\""
        )))
    );
    assert_eq!(
        r.get(
            Root::CurrentUser,
            &format!(r"Software\{NAME}\Capabilities\FileAssociations"),
            ".xfdf"
        ),
        Some(Value::String(format!("{APP_ID}.fdf")))
    );
    let handler = guid_string(windows_clsid("persistent-handler"));
    assert_eq!(
        r.get(
            Root::LocalMachine,
            &format!(r"{classes}\.pdf\PersistentHandler"),
            ""
        ),
        Some(Value::String(handler))
    );
    // Turning the app's registration off takes type-fdf with it.
    let out = i.disable("open-with").unwrap();
    assert!(out.changed.contains(&"type-fdf".to_owned()));
    i.remove_all().unwrap();
    // The other app's handler is back; nothing of ours is left.
    assert_eq!(reg(&dir).dump(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn windows_send_to_shortcuts() {
    let dir = sandbox("sendto");
    let mut i = windows(&dir);
    i.enable("send-to").unwrap();
    let lnk = dir.join(format!(
        "home/AppData/Roaming/Microsoft\\Windows\\SendTo/Merge into one PDF ({NAME}).lnk"
    ));
    let bytes = std::fs::read(&lnk).unwrap();
    assert_eq!(&bytes[..4], &[0x4c, 0, 0, 0]);
    i.disable("send-to").unwrap();
    assert!(!lnk.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn macos_quick_actions() {
    let dir = sandbox("macos");
    let exe = PathBuf::from(format!("/Applications/{NAME}.app/Contents/MacOS/{NAME}"));
    let mut i = Integration::new(Env::sandbox(&dir, Os::MacOs, exe));
    assert!(matches!(i.status("open-with"), Status::Provided(_)));
    assert!(matches!(i.status("thumbnails"), Status::Unavailable(_)));
    assert!(matches!(i.status("default-pdf"), Status::Unavailable(_)));
    let before = files(&dir);
    assert_eq!(i.items()[0].group, pagebara_integration::Group::Finder);
    assert!(i.enable("action-compress").unwrap().shell_changed);
    let wf = dir.join(format!(
        "home/Library/Services/Compress ({NAME}).workflow/Contents"
    ));
    let doc = std::fs::read_to_string(wf.join("document.wflow")).unwrap();
    assert!(
        doc.contains("'--action' 'compress' &quot;$@&quot;"),
        "{doc}"
    );
    let info = std::fs::read_to_string(wf.join("Info.plist")).unwrap();
    assert!(info.contains("<string>com.adobe.pdf</string>"));
    assert!(info.contains("<string>com.apple.finder</string>"));
    assert!(i.disable("action-compress").unwrap().shell_changed);
    assert!(!i.disable("action-compress").unwrap().shell_changed);
    assert!(i.enable("type-images").unwrap().shell_changed);
    let info = std::fs::read_to_string(dir.join(format!(
        "home/Library/Services/Convert to PDF ({NAME}).workflow/Contents/Info.plist"
    )))
    .unwrap();
    assert!(info.contains("<string>public.png</string>"));
    assert!(info.contains("<string>public.jpeg</string>"));
    assert!(!info.contains("com.adobe.pdf"));
    assert!(i.remove_all().unwrap().shell_changed);
    assert_eq!(files(&dir), before);
    assert!(!dir.join("home/Library").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
