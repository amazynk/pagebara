//! The freedesktop recent-files list (`$XDG_DATA_HOME/recently-used.xbel`, shared by GTK
//! and KDE apps and the file managers' "Recent"): adds or refreshes one file's entry and
//! keeps everything else as it was. Written atomically; a list that does not parse is left
//! alone rather than replaced.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};

/// Entries beyond this are not added (GTK trims old ones itself).
const MAX_SIZE: u64 = 16 << 20;

/// Who is opening the file, as the list records it.
#[derive(Clone, Debug)]
pub struct AppInfo {
    pub name: String,
    /// Command line with `%u`, e.g. `'pagebara %u'`.
    pub exec: String,
}

impl AppInfo {
    pub fn this_app() -> AppInfo {
        let bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| pagebara_identity::NAME.to_owned());
        AppInfo {
            name: pagebara_identity::NAME.to_owned(),
            exec: format!("'{bin} %u'"),
        }
    }
}

/// `$XDG_DATA_HOME/recently-used.xbel` (default `~/.local/share`).
pub fn default_path() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|h| h.join(".local/share")))?;
    Some(data.join("recently-used.xbel"))
}

/// Records that `app` opened `file` (absolute) of type `mime` at `now`.
pub fn add(list: &Path, file: &Path, mime: &str, app: &AppInfo, now: SystemTime) -> io::Result<()> {
    let old = match std::fs::metadata(list) {
        Ok(m) if m.len() > MAX_SIZE => return Err(io::Error::other("the recent list is too big")),
        Ok(_) => std::fs::read(list)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let href = crate::uri::from_path(file);
    let stamp = timestamp(now);
    let new = if old.iter().all(u8::is_ascii_whitespace) {
        fresh(&href, mime, app, &stamp)
    } else {
        update(&old, &href, mime, app, &stamp)?
    };
    if let Some(dir) = list.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(list, &new)
}

fn fresh(href: &str, mime: &str, app: &AppInfo, stamp: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xbel version=\"1.0\"\n      \
         xmlns:bookmark=\"http://www.freedesktop.org/standards/desktop-bookmarks\"\n      \
         xmlns:mime=\"http://www.freedesktop.org/standards/shared-mime-info\"\n>\n{}</xbel>\n",
        bookmark(href, mime, app, stamp)
    )
    .into_bytes()
}

fn bookmark(href: &str, mime: &str, app: &AppInfo, stamp: &str) -> String {
    let e = |s: &str| quick_xml::escape::escape(s).into_owned();
    format!(
        "  <bookmark href=\"{}\" added=\"{stamp}\" modified=\"{stamp}\" visited=\"{stamp}\">\n    \
         <info>\n      <metadata owner=\"http://freedesktop.org\">\n        \
         <mime:mime-type type=\"{}\"/>\n        <bookmark:applications>\n          {}\n        \
         </bookmark:applications>\n      </metadata>\n    </info>\n  </bookmark>\n",
        e(href),
        e(mime),
        application(app, stamp, 1)
    )
}

fn application(app: &AppInfo, stamp: &str, count: u64) -> String {
    let e = |s: &str| quick_xml::escape::escape(s).into_owned();
    format!(
        "<bookmark:application name=\"{}\" exec=\"{}\" modified=\"{stamp}\" count=\"{count}\"/>",
        e(&app.name),
        e(&app.exec)
    )
}

/// Copies the list event by event, refreshing (or adding) the entry for `href`.
fn update(old: &[u8], href: &str, mime: &str, app: &AppInfo, stamp: &str) -> io::Result<Vec<u8>> {
    let bad = |e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
    let mut reader = Reader::from_reader(old);
    let mut out = Writer::new(Vec::with_capacity(old.len() + 1024));
    let mut buf = Vec::new();
    // Inside the entry for `href`, and whether it already names this app.
    let mut in_ours = false;
    let mut app_done = false;
    let mut seen_ours = false;
    let mut closed = false;
    loop {
        let ev = reader.read_event_into(&mut buf).map_err(|e| bad(&e))?;
        match ev {
            Event::Eof => break,
            Event::Start(ref s)
                if s.name().as_ref() == b"bookmark"
                    && attr(s, b"href").as_deref() == Some(href) =>
            {
                in_ours = true;
                seen_ours = true;
                app_done = false;
                let s = with_attrs(
                    s,
                    &[
                        ("modified", stamp.to_owned()),
                        ("visited", stamp.to_owned()),
                    ],
                );
                out.write_event(Event::Start(s))?;
            }
            Event::Start(ref s) | Event::Empty(ref s)
                if in_ours
                    && s.name().as_ref() == b"bookmark:application"
                    && attr(s, b"name").as_deref() == Some(app.name.as_str()) =>
            {
                app_done = true;
                let count = attr(s, b"count")
                    .and_then(|c| c.parse::<u64>().ok())
                    .unwrap_or(0)
                    + 1;
                let s = with_attrs(
                    s,
                    &[("modified", stamp.to_owned()), ("count", count.to_string())],
                );
                out.write_event(match ev {
                    Event::Start(_) => Event::Start(s),
                    _ => Event::Empty(s),
                })?;
            }
            Event::End(ref e)
                if in_ours && e.name().as_ref() == b"bookmark:applications" && !app_done =>
            {
                app_done = true;
                raw(
                    &mut out,
                    &format!("  {}\n        ", application(app, stamp, 1)),
                )?;
                out.write_event(ev.borrow())?;
            }
            Event::End(ref e) if in_ours && e.name().as_ref() == b"bookmark" => {
                in_ours = false;
                if !app_done {
                    raw(
                        &mut out,
                        &format!(
                            "  <info>\n      <metadata owner=\"http://freedesktop.org\">\n        \
                             <bookmark:applications>\n          {}\n        \
                             </bookmark:applications>\n      </metadata>\n    </info>\n  ",
                            application(app, stamp, 1)
                        ),
                    )?;
                }
                out.write_event(ev.borrow())?;
            }
            Event::End(ref e) if e.name().as_ref() == b"xbel" => {
                if !seen_ours {
                    raw(&mut out, &bookmark(href, mime, app, stamp))?;
                }
                closed = true;
                out.write_event(ev.borrow())?;
            }
            ev => out.write_event(ev)?,
        }
        buf.clear();
    }
    if !closed {
        return Err(bad(&"no </xbel>"));
    }
    Ok(out.into_inner())
}

fn raw(out: &mut Writer<Vec<u8>>, text: &str) -> io::Result<()> {
    out.get_mut().extend_from_slice(text.as_bytes());
    Ok(())
}

/// Takes `app_name` out of every entry; entries only it had opened go. Returns how many
/// entries changed (the list is left alone when none did).
pub fn remove_app(list: &Path, app_name: &str) -> io::Result<usize> {
    let old = match std::fs::read(list) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let bad = |e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
    let is_app = |s: &BytesStart| s.name().as_ref() == b"bookmark:application";
    let mut reader = Reader::from_reader(old.as_slice());
    let mut out = Writer::new(Vec::with_capacity(old.len()));
    let mut buf = Vec::new();
    let mut entry: Option<Vec<Event<'static>>> = None;
    let mut changed = 0;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| bad(&e))?
            .into_owned();
        match (&mut entry, ev) {
            (_, Event::Eof) => break,
            (None, Event::Start(s)) if s.name().as_ref() == b"bookmark" => {
                entry = Some(vec![Event::Start(s)]);
            }
            (Some(events), Event::End(e)) if e.name().as_ref() == b"bookmark" => {
                events.push(Event::End(e));
                let events = entry.take().unwrap_or_default();
                let apps: Vec<bool> = events
                    .iter()
                    .filter_map(|ev| match ev {
                        Event::Start(s) | Event::Empty(s) if is_app(s) => {
                            Some(attr(s, b"name").as_deref() == Some(app_name))
                        }
                        _ => None,
                    })
                    .collect();
                if !apps.contains(&true) {
                    for ev in events {
                        out.write_event(ev)?;
                    }
                    continue;
                }
                changed += 1;
                if apps.iter().all(|ours| *ours) {
                    continue;
                }
                let mut skip = false;
                for ev in events {
                    match &ev {
                        Event::Empty(s)
                            if is_app(s) && attr(s, b"name").as_deref() == Some(app_name) => {}
                        Event::Start(s)
                            if is_app(s) && attr(s, b"name").as_deref() == Some(app_name) =>
                        {
                            skip = true
                        }
                        Event::End(e) if skip && e.name().as_ref() == b"bookmark:application" => {
                            skip = false
                        }
                        _ if skip => {}
                        _ => out.write_event(ev)?,
                    }
                }
            }
            (Some(events), ev) => events.push(ev),
            (None, ev) => out.write_event(ev)?,
        }
        buf.clear();
    }
    if changed > 0 {
        write_atomic(list, &out.into_inner())?;
    }
    Ok(changed)
}

fn attr(s: &BytesStart, name: &[u8]) -> Option<String> {
    s.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Explicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// `s` with the given attributes set (replaced in place, or appended).
fn with_attrs(s: &BytesStart, set: &[(&str, String)]) -> BytesStart<'static> {
    let name = String::from_utf8_lossy(s.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);
    let mut done = vec![false; set.len()];
    for a in s.attributes().flatten() {
        match set.iter().position(|(k, _)| k.as_bytes() == a.key.as_ref()) {
            Some(i) => {
                done[i] = true;
                out.push_attribute((set[i].0, set[i].1.as_str()));
            }
            None => out.push_attribute(a),
        }
    }
    for (i, (k, v)) in set.iter().enumerate() {
        if !done[i] {
            out.push_attribute((*k, v.as_str()));
        }
    }
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let r = write_synced(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    io::Write::write_all(&mut f, bytes)?;
    f.sync_all()
}

/// `2026-11-15T10:20:30.123456Z`, as GLib writes them.
pub fn timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, day) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_micros()
    )
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn app() -> AppInfo {
        AppInfo {
            name: "pagebara".into(),
            exec: "'pagebara %u'".into(),
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pagebara-xbel-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn timestamps() {
        assert_eq!(timestamp(at(0)), "1970-01-01T00:00:00.000000Z");
        assert_eq!(timestamp(at(1_763_200_830)), "2025-11-15T10:00:30.000000Z");
        assert_eq!(timestamp(at(951_782_400)), "2000-02-29T00:00:00.000000Z");
    }

    #[test]
    fn creates_then_refreshes() {
        let d = temp("new");
        let list = d.join("sub/recently-used.xbel");
        let file = Path::new("/docs/a & b.pdf");
        add(&list, file, "application/pdf", &app(), at(100)).unwrap();
        let s = std::fs::read_to_string(&list).unwrap();
        assert!(s.contains("href=\"file:///docs/a%20%26%20b.pdf\""), "{s}");
        assert!(s.contains("count=\"1\""));
        add(&list, file, "application/pdf", &app(), at(200)).unwrap();
        let s = std::fs::read_to_string(&list).unwrap();
        assert_eq!(s.matches("<bookmark ").count(), 1, "{s}");
        assert!(s.contains("count=\"2\""), "{s}");
        assert!(s.contains("added=\"1970-01-01T00:01:40.000000Z\""));
        assert!(s.contains("visited=\"1970-01-01T00:03:20.000000Z\""));
        std::fs::remove_dir_all(d).unwrap();
    }

    const GTK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0"
      xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks"
      xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info"
>
  <bookmark href="file:///home/u/notes.txt" added="2024-01-01T00:00:00.000000Z" modified="2024-01-01T00:00:00.000000Z" visited="2024-01-01T00:00:00.000000Z">
    <info>
      <metadata owner="http://freedesktop.org">
        <mime:mime-type type="text/plain"/>
        <bookmark:applications>
          <bookmark:application name="gedit" exec="&apos;gedit %u&apos;" modified="2024-01-01T00:00:00.000000Z" count="3"/>
        </bookmark:applications>
      </metadata>
    </info>
  </bookmark>
  <bookmark href="file:///home/u/a.pdf" added="2024-01-01T00:00:00.000000Z" modified="2024-01-01T00:00:00.000000Z" visited="2024-01-01T00:00:00.000000Z">
    <info>
      <metadata owner="http://freedesktop.org">
        <mime:mime-type type="application/pdf"/>
        <bookmark:applications>
          <bookmark:application name="Document Viewer" exec="&apos;evince %u&apos;" modified="2024-01-01T00:00:00.000000Z" count="1"/>
        </bookmark:applications>
      </metadata>
    </info>
  </bookmark>
</xbel>
"#;

    #[test]
    fn keeps_other_entries_and_apps() {
        let d = temp("gtk");
        let list = d.join("recently-used.xbel");
        std::fs::write(&list, GTK).unwrap();
        add(
            &list,
            Path::new("/home/u/a.pdf"),
            "application/pdf",
            &app(),
            at(300),
        )
        .unwrap();
        add(
            &list,
            Path::new("/home/u/new.pdf"),
            "application/pdf",
            &app(),
            at(400),
        )
        .unwrap();
        let s = std::fs::read_to_string(&list).unwrap();
        assert!(
            s.contains("name=\"gedit\" exec=\"&apos;gedit %u&apos;\""),
            "{s}"
        );
        assert!(s.contains("name=\"Document Viewer\""));
        assert_eq!(s.matches("<bookmark ").count(), 3, "{s}");
        assert_eq!(s.matches("name=\"pagebara\"").count(), 2, "{s}");
        // The entry for a.pdf names both apps and keeps its first date.
        let a = &s[s.find("a.pdf").unwrap()..s.find("new.pdf").unwrap()];
        assert!(a.contains("added=\"2024-01-01T00:00:00.000000Z\""), "{a}");
        assert!(a.contains("visited=\"1970-01-01T00:05:00.000000Z\""), "{a}");
        assert!(a.contains("Document Viewer") && a.contains("pagebara"));
        // Still well-formed.
        let mut r = Reader::from_str(&s);
        while r.read_event().unwrap() != Event::Eof {}
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn removing_our_entries_keeps_the_others() {
        let d = temp("remove");
        let list = d.join("recently-used.xbel");
        let other = AppInfo {
            name: "Document Viewer".into(),
            exec: "'evince %u'".into(),
        };
        add(
            &list,
            Path::new("/a/only-ours.pdf"),
            "application/pdf",
            &app(),
            at(10),
        )
        .unwrap();
        add(
            &list,
            Path::new("/a/shared.pdf"),
            "application/pdf",
            &app(),
            at(11),
        )
        .unwrap();
        add(
            &list,
            Path::new("/a/shared.pdf"),
            "application/pdf",
            &other,
            at(12),
        )
        .unwrap();
        add(
            &list,
            Path::new("/a/theirs.pdf"),
            "application/pdf",
            &other,
            at(13),
        )
        .unwrap();
        assert_eq!(remove_app(&list, "pagebara").unwrap(), 2);
        let text = std::fs::read_to_string(&list).unwrap();
        assert!(!text.contains("only-ours.pdf"), "{text}");
        assert!(text.contains("shared.pdf") && text.contains("theirs.pdf"));
        assert!(!text.contains("name=\"pagebara\""), "{text}");
        assert!(text.contains("name=\"Document Viewer\""));
        assert_eq!(remove_app(&list, "pagebara").unwrap(), 0);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_broken_list_is_not_replaced() {
        let d = temp("broken");
        let list = d.join("recently-used.xbel");
        std::fs::write(&list, "<xbel><bookmark href=").unwrap();
        assert!(add(&list, Path::new("/a.pdf"), "application/pdf", &app(), at(1)).is_err());
        assert_eq!(
            std::fs::read_to_string(&list).unwrap(),
            "<xbel><bookmark href="
        );
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        std::fs::remove_dir_all(d).unwrap();
    }
}
