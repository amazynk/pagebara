use pagebara_syntax::{Dict, Document, ObjRef, Object, SaveOptions, Stream};

/// A classic file from object bodies (object `i + 1` is `objs[i]`) with exact offsets.
fn classic(objs: &[&str], root: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root {root} 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

const BASIC: [&str; 4] = [
    "<</Type/Catalog/Pages 2 0 R>>",
    "<</Type/Pages/Kids[3 0 R]/Count 1>>",
    "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R>>",
    "<</Length 9>>stream\n0 0 m S\n\nendstream",
];

fn dict(doc: &Document, num: u32) -> Dict {
    doc.get(ObjRef::new(num, 0))
        .unwrap()
        .as_dict()
        .unwrap()
        .clone()
}

#[test]
fn classic_file() {
    let doc = Document::open(classic(&BASIC, 1)).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(doc.version(), (1, 4));
    let cat = doc.catalog().unwrap();
    let pages = doc.get_in(cat.as_dict().unwrap(), b"Pages");
    assert_eq!(pages.as_dict().unwrap().int(b"Count"), Some(1));
    let page = dict(&doc, 3);
    let contents = doc.get_in(&page, b"Contents");
    let s = contents.as_stream().unwrap();
    assert_eq!(doc.decode(s).unwrap().data, b"0 0 m S\n\n");
}

#[test]
fn broken_offsets_are_repaired() {
    let mut f = classic(&BASIC, 1);
    // Shift every object by inserting junk after the header.
    f.splice(9..9, b"% junk junk junk\n".iter().copied());
    let doc = Document::open(f).unwrap();
    assert_eq!(dict(&doc, 3).name(b"Type"), Some(&b"Page"[..]));
    assert!(doc.was_repaired());
}

#[test]
fn missing_xref_and_trailer() {
    let f = classic(&BASIC, 1);
    let cut = memchr::memmem::find(&f, b"xref").unwrap();
    let doc = Document::open(f[..cut].to_vec()).unwrap();
    assert!(doc.was_repaired());
    assert_eq!(
        doc.catalog().unwrap().as_dict().unwrap().name(b"Type"),
        Some(&b"Catalog"[..])
    );
}

#[test]
fn leading_junk_before_header() {
    let mut f = b"GARBAGE\n".to_vec();
    f.extend(classic(&BASIC, 1));
    let doc = Document::open(f).unwrap();
    assert_eq!(dict(&doc, 2).int(b"Count"), Some(1));
}

/// Objects 1-3 in an object stream (object 4), an xref stream as object 5.
fn compressed() -> Vec<u8> {
    let bodies = [
        "<</Type/Catalog/Pages 2 0 R>>",
        "<</Type/Pages/Kids[3 0 R]/Count 1>>",
        "<</Type/Page/Parent 2 0 R>>",
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (i, b) in bodies.iter().enumerate() {
        header += &format!("{} {} ", i + 1, body.len());
        body += b;
        body += " ";
    }
    let data = header.clone() + &body;
    let mut out = b"%PDF-1.5\n".to_vec();
    let stm_at = out.len();
    out.extend(
        format!(
            "4 0 obj\n<</Type/ObjStm/N 3/First {}/Length {}>>stream\n{data}\nendstream\nendobj\n",
            header.len(),
            data.len()
        )
        .bytes(),
    );
    let x = out.len();
    // W [1 2 1]: type, offset or stream number, generation or index.
    let mut rows: Vec<u8> = vec![0, 0, 0, 255];
    for i in 0..3 {
        rows.extend([2, 0, 4, i]);
    }
    rows.extend([1, (stm_at >> 8) as u8, stm_at as u8, 0]);
    rows.extend([1, (x >> 8) as u8, x as u8, 0]);
    out.extend(
        format!(
            "5 0 obj\n<</Type/XRef/Size 6/W[1 2 1]/Root 1 0 R/Length {}>>stream\n",
            rows.len()
        )
        .bytes(),
    );
    out.extend(&rows);
    out.extend(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").bytes());
    out
}

#[test]
fn xref_and_object_streams() {
    let doc = Document::open(compressed()).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(dict(&doc, 3).name(b"Type"), Some(&b"Page"[..]));
    assert_eq!(doc.object_numbers(), [1, 2, 3, 4, 5]);
}

#[test]
fn object_stream_inside_itself() {
    // The xref stream claims object stream 4 is object 0 of object stream 4.
    let mut f = compressed();
    let rows = memchr::memmem::find(&f, b"/XRef").unwrap();
    let rows = rows + memchr::memmem::find(&f[rows..], b"stream\n").unwrap() + 7;
    f[rows + 16..rows + 20].copy_from_slice(&[2, 0, 4, 0]);
    let doc = Document::open(f).unwrap();
    // Found by scanning the file instead.
    assert_eq!(dict(&doc, 3).name(b"Type"), Some(&b"Page"[..]));
}

#[test]
fn incremental_save_keeps_history() {
    for f in [classic(&BASIC, 1), compressed()] {
        let doc = Document::open(f.clone()).unwrap();
        let mut ed = doc.clone();
        let mut d = dict(&ed, 2);
        d.set(b"Count", 7);
        ed.set(ObjRef::new(2, 0), d);
        let new = ed.add(Object::string(b"hello"));
        ed.delete(ObjRef::new(3, 0));
        // The snapshot taken before editing is untouched.
        assert_eq!(dict(&doc, 2).int(b"Count"), Some(1));

        let saved = ed.save_incremental().unwrap();
        assert!(saved.starts_with(&f));
        let re = Document::open(saved).unwrap();
        assert!(!re.was_repaired());
        assert_eq!(dict(&re, 2).int(b"Count"), Some(7));
        assert_eq!(re.get(new).unwrap().as_string(), Some(&b"hello"[..]));
        assert!(re.get(ObjRef::new(3, 0)).is_none());
        // Unchanged objects come through the /Prev chain.
        assert!(re.catalog().is_some());
    }
}

#[test]
fn full_save_roundtrip() {
    for f in [classic(&BASIC, 1), compressed()] {
        let mut doc = Document::open(f).unwrap();
        let s = doc.add(Stream::new(Dict::new(), b"q Q".to_vec()));
        let out = doc.save_full().unwrap();
        let re = Document::open(out).unwrap();
        assert!(!re.was_repaired());
        assert_eq!(dict(&re, 3).name(b"Type"), Some(&b"Page"[..]));
        let st = re.get(s).unwrap();
        assert_eq!(re.decode(st.as_stream().unwrap()).unwrap().data, b"q Q");
        // No object or xref streams survive a full rewrite.
        assert!(re.object_numbers().iter().all(|&n| {
            let o = re.get(ObjRef::new(n, 0)).unwrap();
            o.as_dict()
                .and_then(|d| d.name(b"Type"))
                .is_none_or(|t| t != b"ObjStm" && t != b"XRef")
        }));
    }
}

/// A damaged file's page tree root that carries `/Type /ObjStm` is still the page tree
/// root: a full save keeps it, and with it the pages.
#[test]
fn full_save_keeps_a_referred_object_typed_as_an_object_stream() {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>",
        "<</Type/ObjStm/Kids[3 0 R]/Count 1/Length 0>>stream\n\nendstream",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R>>",
        "<</Length 9>>stream\n0 0 m S\n\nendstream",
        // Refers to nothing and nothing refers to it: this one is left out.
        "<</Type/XRef/Length 0>>stream\n\nendstream",
    ];
    let doc = Document::open(classic(&objs, 1)).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(doc.old_structure().into_iter().collect::<Vec<_>>(), [5]);
    for out in [
        doc.save_full().unwrap(),
        doc.save_full_with(&SaveOptions::compact()).unwrap(),
    ] {
        let re = Document::open(out).unwrap();
        assert!(!re.was_repaired());
        let cat = re.catalog().unwrap();
        let pages = re.get_in(cat.as_dict().unwrap(), b"Pages");
        let kids = pages.as_stream().unwrap().dict.get(b"Kids").cloned();
        assert_eq!(
            kids,
            Some(Object::Array(vec![Object::Ref(ObjRef::new(3, 0))]))
        );
        assert!(re.get(ObjRef::new(5, 0)).is_none());
    }
}

#[test]
fn compact_save_roundtrip() {
    let long = "0 0 m 100 100 l S\n".repeat(50);
    for f in [classic(&BASIC, 1), compressed()] {
        let mut doc = Document::open(f).unwrap();
        let s = doc.add(Stream::new(Dict::new(), long.as_bytes().to_vec()));
        let mut parms = Dict::new();
        parms.set(b"Predictor", 12);
        let mut d = Dict::new();
        d.set(b"DecodeParms", parms);
        let odd = doc.add(Stream::new(d, long.as_bytes().to_vec()));
        let mut meta = Dict::new();
        meta.set(b"Type", Object::name(b"Metadata"));
        let xmp = doc.add(Stream::new(meta, long.as_bytes().to_vec()));
        let old_gen = ObjRef::new(doc.add(Object::Null).num, 3);
        doc.set(old_gen, Object::Int(42));
        let many: Vec<_> = (0..250).map(|i| doc.add(Object::Int(i))).collect();

        let out = doc.save_full_with(&SaveOptions::compact()).unwrap();
        assert!(out.starts_with(b"%PDF-1.5"));
        assert!(!out.windows(6).any(|w| w == b"\nxref\n"));
        // 255 small objects at most 100 per stream; the old object stream is gone.
        assert_eq!(memchr::memmem::find_iter(&out, b"/Type /ObjStm").count(), 3);
        let re = Document::open(out.clone()).unwrap();
        assert!(!re.was_repaired());
        assert_eq!(dict(&re, 3).name(b"Type"), Some(&b"Page"[..]));
        for (i, r) in many.iter().enumerate() {
            assert_eq!(re.get(*r).unwrap().as_int(), Some(i as i64));
        }
        assert_eq!(re.reference(old_gen.num), Some(old_gen));
        assert_eq!(re.get(old_gen).unwrap().as_int(), Some(42));
        for r in [s, odd, xmp] {
            let st = re.get(r).unwrap();
            let st = st.as_stream().unwrap();
            assert_eq!(re.decode(st).unwrap().data, long.as_bytes());
            let flate = st.dict.name(b"Filter") == Some(b"FlateDecode");
            assert_eq!(flate, r != xmp);
            assert!(!st.dict.contains(b"DecodeParms"));
        }
        // Saving again is stable, and incremental updates on top work.
        let mut re = re;
        re.set(many[0], Object::Int(-1));
        let inc = Document::open(re.save_incremental().unwrap()).unwrap();
        assert_eq!(inc.get(many[0]).unwrap().as_int(), Some(-1));
        assert_eq!(inc.get(many[1]).unwrap().as_int(), Some(1));
    }
}

#[test]
fn save_options_each_alone() {
    for opts in [
        SaveOptions {
            xref_stream: true,
            ..SaveOptions::default()
        },
        SaveOptions {
            compress_streams: true,
            ..SaveOptions::default()
        },
        SaveOptions {
            object_streams: true,
            ..SaveOptions::default()
        },
    ] {
        let doc = Document::open(classic(&BASIC, 1)).unwrap();
        let out = doc.save_full_with(&opts).unwrap();
        let xref_table = out.windows(6).any(|w| w == b"\nxref\n");
        assert_eq!(xref_table, !opts.xref_stream && !opts.object_streams);
        let re = Document::open(out).unwrap();
        assert!(!re.was_repaired(), "{opts:?}");
        let c = re.get(ObjRef::new(4, 0)).unwrap();
        assert_eq!(
            re.decode(c.as_stream().unwrap()).unwrap().data,
            b"0 0 m S\n\n"
        );
    }
}

#[test]
fn import_copies_reachable_objects_once() {
    let src = Document::open(classic(&BASIC, 1)).unwrap();
    let mut dst = Document::open(compressed()).unwrap();
    let mut map = Default::default();
    let a = dst.import(&src, ObjRef::new(3, 0), &mut map);
    let b = dst.import(&src, ObjRef::new(3, 0), &mut map);
    assert_eq!(a, b);
    let page = dst.get(a).unwrap();
    let page = page.as_dict().unwrap();
    assert!(!page.contains(b"Parent"));
    let c = dst.get_in(page, b"Contents");
    assert_eq!(
        dst.decode(c.as_stream().unwrap()).unwrap().data,
        b"0 0 m S\n\n"
    );
}
