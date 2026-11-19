use pagebara_syntax::{Document, Error, Object, Permissions, SaveOptions};

const FILES: [(&str, bool); 7] = [
    ("rc4-40", false),
    ("rc4-128", false),
    ("aes-128-objstm", false),
    ("aes-256-r5", false),
    ("aes-256-r6", false),
    ("aes-256-empty-user", true),
    ("rc4-empty-user", true),
];

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/crypt/{name}.pdf",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn open(bytes: Vec<u8>, pw: &str) -> Document {
    match Document::open(bytes) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => l.unlock(pw).expect("password accepted"),
        Err(e) => panic!("{e}"),
    }
}

/// The page text and document title, which are both encrypted in the files.
fn check(doc: &Document, name: &str) {
    let cat = doc.catalog().unwrap();
    let pages = doc.get_in(cat.as_dict().unwrap(), b"Pages");
    let kids = doc.get_in(pages.as_dict().unwrap(), b"Kids");
    let page = doc.resolve(&kids.as_array().unwrap()[0]);
    let contents = doc.get_in(page.as_dict().unwrap(), b"Contents");
    let data = doc.decode(contents.as_stream().unwrap()).unwrap().data;
    assert!(
        data.starts_with(b"BT (Secret text) Tj ET"),
        "{name}: {:?}",
        data.escape_ascii().to_string()
    );
    let info = doc.get_in(doc.trailer(), b"Info");
    let title = info
        .as_dict()
        .unwrap()
        .get(b"Title")
        .and_then(Object::as_string);
    assert_eq!(title, Some(&b"Top secret title"[..]), "{name}");
}

#[test]
fn passwords() {
    for (name, empty_user) in FILES {
        match Document::open(read(name)) {
            Ok(d) => {
                assert!(empty_user, "{name} opened without password");
                assert!(!d.is_owner());
                check(&d, name);
            }
            Err(Error::PasswordRequired(l)) => {
                assert!(!empty_user, "{name}");
                let l = l.unlock("wrong").expect_err("wrong password rejected");
                let d = l.unlock("user").unwrap();
                assert!(!d.is_owner());
                check(&d, name);
            }
            Err(e) => panic!("{name}: {e}"),
        }
        let owner = open(read(name), "owner");
        // Empty-user files open without the owner password; only explicit owner unlocks count.
        if !empty_user {
            assert!(owner.is_owner(), "{name}");
            assert_eq!(owner.permissions(), Permissions::ALL);
        }
    }
}

#[test]
fn permissions_reported() {
    let d = open(read("aes-256-r6"), "user");
    let p = d.permissions();
    assert!(!p.print && !p.modify && p.copy, "{p:?}");
}

#[test]
fn saving_keeps_or_removes_protection() {
    for (name, _) in FILES {
        let mut doc = open(read(name), "user");
        let note = doc.add(Object::string(b"added after opening"));

        let inc = doc.save_incremental().unwrap();
        let re = open(inc, "user");
        check(&re, name);
        assert_eq!(
            re.get(note).unwrap().as_string(),
            Some(&b"added after opening"[..]),
            "{name}"
        );

        let full = doc.save_full().unwrap();
        let re = open(full.clone(), "user");
        check(&re, name);
        assert_eq!(
            re.get(note).unwrap().as_string(),
            Some(&b"added after opening"[..])
        );
        // The new string is stored encrypted.
        assert!(
            memchr::memmem::find(&full, b"added after opening").is_none(),
            "{name}"
        );

        assert!(matches!(
            doc.remove_security(),
            Err(Error::OwnerPasswordRequired)
        ));
        let mut doc = open(read(name), "owner");
        assert!(!doc.unlock_owner("wrong") || doc.is_owner());
        assert!(doc.unlock_owner("owner"));
        doc.remove_security().unwrap();
        assert!(matches!(doc.save_incremental(), Err(Error::NeedsFullSave)));
        let plain = Document::open(doc.save_full().unwrap()).unwrap();
        assert!(!plain.is_encrypted());
        check(&plain, name);
    }
}

#[test]
fn non_ascii_passwords_from_other_writers() {
    // qpdf stores this one in PDFDocEncoding (RC4), the other as UTF-8 (AES-256).
    for (name, pw) in [
        ("rc4-128-latin", "café€"),
        ("aes-256-unicode", "pässwörd 密码 🔐"),
    ] {
        let Err(Error::PasswordRequired(l)) = Document::open(read(name)) else {
            panic!("{name}")
        };
        let l = l.unlock("cafe").expect_err("near miss rejected");
        check(&l.unlock(pw).unwrap(), name);
    }
}

fn protect(doc: &mut Document, user: &str, owner: &str, perms: Permissions) -> Vec<u8> {
    doc.set_password(user, owner, perms).unwrap();
    assert!(matches!(doc.save_incremental(), Err(Error::NeedsFullSave)));
    doc.save_full().unwrap()
}

#[test]
fn set_password_on_plain_file() {
    let mut no_print = Permissions::ALL;
    no_print.print = false;
    no_print.print_high_quality = false;
    let saved = protect(
        &mut open(read("plain"), ""),
        "pässwörd 密码 🔐",
        "Ωmega",
        no_print,
    );
    assert!(memchr::memmem::find(&saved, b"Secret text").is_none());
    assert!(matches!(
        Document::open(saved.clone()),
        Err(Error::PasswordRequired(_))
    ));

    let user = open(saved.clone(), "pässwörd 密码 🔐");
    check(&user, "user");
    assert!(!user.is_owner());
    assert_eq!(user.permissions(), no_print);
    let owner = open(saved, "Ωmega");
    assert!(owner.is_owner());
    check(&owner, "owner");
}

#[test]
fn passwords_are_saslprepped() {
    // Soft hyphen maps to nothing, no-break space to space, and NFKC folds Ⅸ to IX.
    let saved = protect(
        &mut open(read("plain"), ""),
        "I\u{AD}X\u{A0}y",
        "o",
        Permissions::ALL,
    );
    for typed in ["IX y", "\u{2168} y", "I\u{AD}X\u{A0}y"] {
        check(&open(saved.clone(), typed), typed);
    }
    let Err(Error::PasswordRequired(l)) = Document::open(saved) else {
        panic!()
    };
    assert!(l.unlock("IXy").is_err());

    let mut doc = open(read("plain"), "");
    assert!(matches!(
        doc.set_password("bell\u{7}", "o", Permissions::ALL),
        Err(Error::InvalidPassword)
    ));
}

#[test]
fn change_and_remove_password() {
    // Only the owner may change protection.
    let mut user = open(read("aes-256-r6"), "user");
    assert!(matches!(
        user.set_password("a", "b", Permissions::ALL),
        Err(Error::OwnerPasswordRequired)
    ));
    let mut doc = open(read("rc4-40"), "owner");
    let changed = protect(&mut doc, "new user", "new owner", Permissions::ALL);
    let Err(Error::PasswordRequired(l)) = Document::open(changed.clone()) else {
        panic!()
    };
    let l = l.unlock("user").expect_err("old password no longer works");
    check(&l.unlock("new user").unwrap(), "changed");

    let mut doc = open(changed, "new owner");
    doc.remove_security().unwrap();
    let plain = Document::open(doc.save_full().unwrap()).unwrap();
    assert!(!plain.is_encrypted());
    check(&plain, "removed");
}

#[test]
fn aes128_without_document_length() {
    // `/Length` of the document is for V 2 and 3; V 4 files may leave it out (same size
    // replacements keep the offsets).
    let mut f = read("aes-128-objstm");
    let at = memchr::memmem::find(&f, b"/Filter /Standard /Length 128").unwrap() + 18;
    f[at..at + 11].copy_from_slice(b"/Lxxxxx 128");
    check(&open(f.clone(), "user"), "no /Length");
    let at = memchr::memmem::find(&f, b"/CFM /AESV2 /Length 16 >>").unwrap() + 12;
    f[at..at + 13].copy_from_slice(b"/Length 128>>");
    check(&open(f, "user"), "crypt filter /Length in bits");
}

/// Object streams and xref streams in a saved file.
fn structure_streams(saved: &[u8]) -> (usize, usize) {
    let count = |p: &[u8]| memchr::memmem::find_iter(saved, p).count();
    (count(b"/Type /ObjStm"), count(b"/Type /XRef"))
}

#[test]
fn compact_save_keeps_protection() {
    for (name, _) in FILES {
        let mut doc = open(read(name), "user");
        let note = doc.add(Object::string(b"added after opening"));
        let saved = doc.save_full_with(&SaveOptions::compact()).unwrap();
        assert_eq!(structure_streams(&saved), (1, 1), "{name}");
        // Strings inside the object stream are encrypted with it.
        assert!(memchr::memmem::find(&saved, b"added after opening").is_none());
        let re = open(saved, "user");
        assert!(!re.was_repaired(), "{name}");
        check(&re, name);
        assert_eq!(
            re.get(note).unwrap().as_string(),
            Some(&b"added after opening"[..])
        );
    }
}

#[test]
fn compact_save_with_new_password() {
    let mut doc = open(read("plain"), "");
    doc.set_password("u", "o", Permissions::ALL).unwrap();
    let saved = doc.save_full_with(&SaveOptions::compact()).unwrap();
    assert!(memchr::memmem::find(&saved, b"Secret text").is_none());
    // The /Encrypt dictionary stays a plain object outside the object streams.
    assert!(memchr::memmem::find(&saved, b"/Filter /Standard").is_some());
    check(&open(saved.clone(), "u"), "user");
    let mut owner = open(saved, "o");
    assert!(owner.is_owner());
    owner.remove_security().unwrap();
    let plain = owner.save_full_with(&SaveOptions::compact()).unwrap();
    let plain = Document::open(plain).unwrap();
    assert!(!plain.is_encrypted());
    check(&plain, "removed");
}
