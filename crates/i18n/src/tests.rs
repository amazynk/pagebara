use super::*;

/// Fuzzer finds: a `msgstr[` key ending inside a multibyte character panicked, and a huge
/// form index would allocate that many strings.
#[test]
fn damaged_plural_keys() {
    let e = po::parse("msgid \"a\"\nmsgstr[о \"x\"\nmsgstr[99999999999] \"y\"\nmsgstr[1] \"z\"\n");
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].strs, ["", "z"]);
}

#[test]
fn formats_placeholders() {
    assert_eq!(format("Page {} of {}", &[&3, &10], None), "Page 3 of 10");
    assert_eq!(format("{1} / {0}", &[&"a", &"b"], None), "b / a");
    assert_eq!(format("{n} pages", &[], Some(4)), "4 pages");
    assert_eq!(format("{{x}} {}", &[&1], None), "{x} 1");
    assert_eq!(format("{} {}", &[&1], None), "1 {}");
}

#[test]
fn finds_languages() {
    assert_eq!(find("de_DE.UTF-8").map(|l| l.code), Some("de"));
    assert_eq!(find("pt-PT").map(|l| l.code), Some("pt_BR"));
    assert_eq!(find("zh-Hans-CN").map(|l| l.code), Some("zh_CN"));
    assert_eq!(find("iw_IL").map(|l| l.code), Some("he"));
    assert_eq!(find("C"), None);
    assert!(find("ar").is_some_and(|l| l.rtl));
}

#[test]
fn plural_forms() {
    let forms = |code, ns: &[i64]| ns.iter().map(|&n| plural_form(code, n)).collect::<Vec<_>>();
    assert_eq!(forms("en", &[0, 1, 2]), [1, 0, 1]);
    assert_eq!(forms("fr", &[0, 1, 2]), [0, 0, 1]);
    assert_eq!(
        forms("ru", &[1, 2, 5, 11, 21, 22, 25]),
        [0, 1, 2, 2, 0, 1, 2]
    );
    assert_eq!(forms("pl", &[1, 2, 5, 12, 22, 21]), [0, 1, 2, 2, 1, 2]);
    assert_eq!(forms("ar", &[0, 1, 2, 3, 11, 100]), [0, 1, 2, 3, 4, 5]);
    assert_eq!(forms("ja", &[1, 2]), [0, 0]);
}

/// Every catalog has the right number of plural forms and keeps each message's
/// placeholders, so no translation drops or invents an argument.
#[test]
fn catalogs_are_consistent() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../po");
    for l in &LANGUAGES[1..] {
        let path = dir.join(format!("{}.po", l.code));
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let entries = po::parse(&src);
        assert_eq!(
            po::plural_count(&entries),
            Some(plural_count(l.code)),
            "{}",
            l.code
        );
        for e in entries.iter().filter(|e| e.translated()) {
            let want = placeholders(&e.id, e.plural.is_some());
            for s in &e.strs {
                let mut got = placeholders(s, e.plural.is_some());
                // A form for exactly one item may leave out the count.
                let n = "{n}".to_owned();
                if e.plural.is_some() && want.contains(&n) && !got.contains(&n) {
                    got.push("{n}".into());
                    got.sort();
                }
                assert_eq!(got, want, "{}: {:?} → {:?}", l.code, e.id, s);
            }
            if e.plural.is_some() {
                assert_eq!(e.strs.len(), plural_count(l.code), "{}: {:?}", l.code, e.id);
            }
        }
    }
}

/// The placeholders of a message, numbered: `{}` `{}` and `{0}` `{1}` compare equal.
fn placeholders(s: &str, _plural: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut next = 0;
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let tail = &rest[i..];
        if let Some(t) = tail.strip_prefix("{{") {
            rest = t;
            continue;
        }
        let Some(end) = tail.find('}') else { break };
        let name = tail[1..end].trim();
        out.push(if name.is_empty() {
            next += 1;
            format!("{{{}}}", next - 1)
        } else {
            format!("{{{name}}}")
        });
        rest = &tail[end + 1..];
    }
    out.sort();
    out
}

#[test]
fn numbers_and_languages() {
    set_language(&LANGUAGES[0]);
    assert_eq!(integer(1234567), "1,234,567");
    assert_eq!(decimal(-1.25, 1), "-1.2");
    assert_eq!(bytes(1536), "1.5 KB");
    assert_eq!(trn!("{n} page", "{n} pages", 1), "1 page");
    set_language(find("de").unwrap());
    assert_eq!(integer(1234), "1.234");
    assert_eq!(decimal(1234.567, 2), "1.234,57");
    assert_eq!(date_time(2026, 11, 5, 9, 3), "05.11.2026, 09:03");
    set_language(find("es").unwrap());
    assert_eq!(integer(1234), "1234");
    assert_eq!(integer(12345), "12.345");
    set_language(&LANGUAGES[0]);
}
