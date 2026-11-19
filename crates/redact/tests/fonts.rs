//! Embedded fonts lose the outlines of glyphs redaction removed everywhere.

mod common;

use common::*;
use pagebara_font::Font as Program;
use pagebara_font::outline::Path;
use pagebara_redact::{
    ApplyOptions, Mark, Pattern, Rect, Report, add_mark, apply_marks, find, save,
};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

const INTER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/desktop/ui/fonts/InterVariable.ttf"
);

/// A document whose pages show `lines` (page, y, text) in one embedded subset of `font`,
/// with `extra` page dictionary entries per page.
fn font_doc(font: &[u8], lines: &[(usize, f32, &str)], extra: &[&str]) -> Vec<u8> {
    let mut p = Pdf::new();
    for e in extra {
        p.page("", "", e);
    }
    let mut doc = Document::open(p.finish()).unwrap();
    let mut emb = pagebara_font::subset::Embedder::new(font.to_vec(), 0).unwrap();
    let mut contents = vec![String::new(); extra.len()];
    for &(page, y, text) in lines {
        let codes = emb.encode(text);
        contents[page].push_str(&format!("BT /F1 14 Tf 20 {y} Td <{}> Tj ET\n", hex(&codes)));
    }
    let f = emb.write(&mut doc).unwrap();
    let tree = pagebara_pages::PageTree::load(&doc);
    for (i, page) in tree.pages().iter().enumerate() {
        let mut d = doc.get(page.r).unwrap().as_dict().unwrap().clone();
        let mut fonts = Dict::new();
        fonts.set(b"F1", Object::Ref(f));
        let mut res = Dict::new();
        res.set(b"Font", Object::Dict(fonts));
        d.set(b"Resources", Object::Dict(res));
        let c = doc.add(Stream::new(Dict::new(), contents[i].clone().into_bytes()));
        d.set(b"Contents", Object::Ref(c));
        doc.set(page.r, d);
    }
    doc.save_full().unwrap()
}

struct Out {
    before: Document,
    after: Document,
    bytes: Vec<u8>,
    report: Report,
}

fn redact(file: Vec<u8>, secret: Option<&str>, marks: &[(usize, Mark)]) -> Out {
    let before = Document::open(file).unwrap();
    let mut doc = before.clone();
    let tree = pagebara_pages::PageTree::load(&doc);
    if let Some(s) = secret {
        let hits = find(&doc, &Pattern::text(s), Some(&[0]));
        assert!(!hits.is_empty());
        for h in hits {
            add_mark(&mut doc, tree.pages()[h.page].r, &h.mark()).unwrap();
        }
    }
    let report = apply_marks(&mut doc, marks, &ApplyOptions::default()).unwrap();
    let bytes = save(&doc).unwrap();
    let after = Document::open(bytes.clone()).unwrap();
    Out {
        before,
        after,
        bytes,
        report,
    }
}

/// The font of `/F1` on page 0: its dictionary and its parsed program.
fn font(doc: &Document) -> (pagebara_content::font::Font, Program) {
    let tree = pagebara_pages::PageTree::load(doc);
    let page = doc.get(tree.pages()[0].r).unwrap();
    let res = doc.get_in(page.as_dict().unwrap(), b"Resources");
    let fonts = doc.get_in(res.as_dict().unwrap(), b"Font");
    let fo = doc.get_in(fonts.as_dict().unwrap(), b"F1");
    let fd = fo.as_dict().unwrap();
    let r: ObjRef = pagebara_font::pdf::font_file_ref(doc, fd).unwrap();
    let s = doc.get(r).unwrap();
    let data = doc.decode(s.as_stream().unwrap()).unwrap().data;
    let f = pagebara_content::font::Font::load_with(
        doc,
        fd,
        Some(&*pagebara_content::font::embedded_programs()),
    );
    (f, Program::parse(data).unwrap())
}

fn outline(p: &Program, gid: u32) -> Path {
    let mut path = Path::default();
    let gid = gid as u16;
    match p {
        Program::Sfnt(s) if s.has_glyf() => {
            s.outline(gid, &mut path);
        }
        _ => {
            p.as_cff().unwrap().outline(gid, &mut path);
        }
    }
    path
}

/// The code `/F1` shows for `c` on page 0 of `doc` (the text is one glyph per character).
fn code_of(doc: &Document, c: char) -> u32 {
    let (f, _) = font(doc);
    (0..0xFFFF)
        .find(|&code| {
            let mut s = String::new();
            f.unicode(doc, code, &mut s) && s == c.to_string()
        })
        .unwrap()
}

fn ints(v: &[i64]) -> Object {
    Object::Array(v.iter().map(|&i| Object::Int(i)).collect())
}

/// What `/ToUnicode` of `/F1` says of `code`.
fn unicode(doc: &Document, code: u32) -> String {
    let (f, _) = font(doc);
    let tu = doc.get_in(&f.dict, b"ToUnicode");
    let data = doc.decode(tu.as_stream().unwrap()).unwrap().data;
    let mut s = String::new();
    pagebara_content::font::cmap::CMap::parse(&data).unicode(code, &mut s);
    s
}

/// `Jane` goes from "Public Jane here"; of its letters only `e` is shown elsewhere.
fn check_program(data: &[u8]) {
    let file = font_doc(
        data,
        &[
            (0, 150.0, "Public Jane here"),
            (0, 100.0, "Other kept text"),
        ],
        &[""],
    );
    let o = redact(file, Some("Jane"), &[]);
    let (bf, bp) = font(&o.before);
    let (af, ap) = font(&o.after);
    let gone: Vec<char> = "Jan".chars().collect();
    for c in gone {
        let code = code_of(&o.before, c);
        let g = bf.glyph_id(code).unwrap();
        assert!(!outline(&bp, g).is_empty(), "{c} had an outline");
        assert_eq!(af.glyph_id(code), Some(g));
        assert!(outline(&ap, g).is_empty(), "{c} still has an outline");
        assert_eq!(unicode(&o.after, code), "", "{c} is still in /ToUnicode");
    }
    for c in "ePublichrOtkx".chars() {
        let code = code_of(&o.before, c);
        let g = bf.glyph_id(code).unwrap();
        assert_eq!(outline(&ap, g), outline(&bp, g), "{c} changed");
        assert_eq!(unicode(&o.after, code), c.to_string());
    }
    assert_eq!(o.report.glyphs_blanked, 3);
    assert_eq!(o.report.fonts_pruned, 1);
    let v = pagebara_redact::verify(&o.bytes, None, &o.report.regions(), &["Jane"]);
    assert!(v.is_clean(), "{v:#?}");
    // Everything else draws as before.
    let a = render(&o.before, 0);
    let b = render(&o.after, 0);
    let boxes: Vec<_> = o.report.pages[0]
        .regions
        .iter()
        .map(|r| device_box(&r.bbox(), 0, 2))
        .collect();
    assert_eq!(diff_outside(&a, &b, &boxes, 0), 0);
    assert!(page_text(&o.after, 0).text.contains("Other kept text"));
    external_checks(&o.bytes, &["Jane"]);
}

#[test]
fn truetype_subset_loses_removed_glyphs() {
    check_program(&std::fs::read(INTER).unwrap());
}

#[test]
fn cff_subset_loses_removed_glyphs() {
    let db = pagebara_font::fallback::FontDb::system();
    let otf = db
        .faces()
        .iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "otf") && f.index == 0)
        .find_map(|f| {
            let data = std::fs::read(&f.path).ok()?;
            let p = Program::parse(data.clone())?;
            let s = p.as_sfnt()?;
            let ok = !s.has_glyf()
                && s.cff().is_some_and(|c| !c.is_cff2())
                && "PublicJanehrOtkx"
                    .chars()
                    .all(|c| s.glyph_for_char(c).is_some());
            ok.then_some(data)
        });
    match otf {
        Some(d) => check_program(&d),
        None => eprintln!("skipped: no OpenType-CFF font with Latin letters is installed"),
    }
}

/// A glyph still shown on another page, or by an annotation's appearance, keeps its shape.
#[test]
fn glyphs_shown_elsewhere_stay() {
    let data = std::fs::read(INTER).unwrap();
    let file = font_doc(
        &data,
        &[(0, 150.0, "Public Jane"), (1, 150.0, "Jane again")],
        &["", ""],
    );
    let doc = Document::open(file.clone()).unwrap();
    let r = glyphs(&doc, 0)
        .iter()
        .filter(|g| "Jane".contains(&g.0))
        .map(|g| g.1)
        .fold([i64::MAX, i64::MAX, i64::MIN, i64::MIN], |a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        });
    let mark = Mark::rect(Rect::new(
        r[0] as f64 / 100.0 + 1.0,
        r[1] as f64 / 100.0,
        r[2] as f64 / 100.0 - 1.0,
        r[3] as f64 / 100.0,
    ));
    let o = redact(file, None, &[(0, mark)]);
    assert_eq!(o.report.pages[0].stats.glyphs_removed, 4);
    assert_eq!(o.report.glyphs_blanked, 0);
    let (bf, bp) = font(&o.before);
    let (_, ap) = font(&o.after);
    for c in "Jane".chars() {
        let g = bf.glyph_id(code_of(&o.before, c)).unwrap();
        assert_eq!(outline(&ap, g), outline(&bp, g));
    }
    assert!(page_text(&o.after, 1).text.contains("Jane again"));
}

/// An annotation appearance showing the glyph counts as a use.
#[test]
fn glyphs_in_appearances_stay() {
    let data = std::fs::read(INTER).unwrap();
    let file = font_doc(&data, &[(0, 150.0, "Public Jane")], &[""]);
    let mut doc = Document::open(file).unwrap();
    // A free text annotation away from the area, drawn with the same font.
    let tree = pagebara_pages::PageTree::load(&doc);
    let page = tree.pages()[0].r;
    let pd = doc.get(page).unwrap().as_dict().unwrap().clone();
    let res = doc.get_in(&pd, b"Resources").as_dict().unwrap().clone();
    let content = pagebara_content::page_contents(&doc, &pd);
    let s = String::from_utf8_lossy(&content);
    let j = s.find('<').unwrap();
    let codes = &s[j + 1..j + 1 + 4 * 11];
    let jane = &codes[4 * 7..];
    let mut ap = Dict::new();
    ap.set(b"Type", Object::name(b"XObject"));
    ap.set(b"Subtype", Object::name(b"Form"));
    ap.set(b"BBox", ints(&[0, 0, 100, 20]));
    ap.set(b"Resources", Object::Dict(res));
    let ap = doc.add(Stream::new(
        ap,
        format!("BT /F1 10 Tf 2 5 Td <{jane}> Tj ET").into_bytes(),
    ));
    let mut n = Dict::new();
    n.set(b"N", Object::Ref(ap));
    let mut a = Dict::new();
    a.set(b"Type", Object::name(b"Annot"));
    a.set(b"Subtype", Object::name(b"FreeText"));
    a.set(b"Rect", ints(&[150, 20, 250, 40]));
    a.set(b"AP", Object::Dict(n));
    let a = doc.add(a);
    let mut pd = pd;
    pd.set(b"Annots", Object::Array(vec![Object::Ref(a)]));
    doc.set(page, pd);
    let o = redact(doc.save_full().unwrap(), Some("Jane"), &[]);
    assert_eq!(o.report.glyphs_blanked, 0, "{:?}", o.report.warnings);
}

/// Type 3 glyphs nothing shows any more draw nothing; ones still shown stay.
#[test]
fn type3_glyph_procedures_are_emptied() {
    let mut p = Pdf::new();
    let a = p.stream("", b"500 0 0 0 400 700 d1 0 0 400 700 re f");
    let b = p.stream("", b"500 0 0 0 400 500 d1 0 0 400 500 re f");
    let c = p.stream("", b"500 0 0 0 400 300 d1 0 0 400 300 re f");
    let t3 = p.add(format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 400 700] /FontMatrix [0.001 0 0 0.001 0 0] \
         /CharProcs << /a {a} 0 R /b {b} 0 R /c {c} 0 R >> /Encoding << /Type /Encoding /Differences [97 /a /b /c] >> \
         /FirstChar 97 /LastChar 99 /Widths [500 500 500] >>"
    ));
    let font = format!("/Font << /T3 {t3} 0 R >>");
    p.page(
        &font,
        "BT /T3 20 Tf 20 100 Td (abbc) Tj ET BT /T3 20 Tf 20 50 Td (ac) Tj ET",
        "",
    );
    let file = p.finish();
    let doc = Document::open(file.clone()).unwrap();
    let g = glyphs(&doc, 0);
    let r = Rect::new(
        g[1].1[0] as f64 / 100.0 + 1.0,
        95.0,
        g[2].1[2] as f64 / 100.0 - 1.0,
        120.0,
    );
    let o = redact(file, None, &[(0, Mark::rect(r))]);
    assert_eq!(o.report.pages[0].stats.glyphs_removed, 2);
    assert_eq!(o.report.glyphs_blanked, 1);
    let tree = pagebara_pages::PageTree::load(&o.after);
    let page = o.after.get(tree.pages()[0].r).unwrap();
    let res = o.after.get_in(page.as_dict().unwrap(), b"Resources");
    let fonts = o.after.get_in(res.as_dict().unwrap(), b"Font");
    let t3 = o.after.get_in(fonts.as_dict().unwrap(), b"T3");
    let procs = o.after.get_in(t3.as_dict().unwrap(), b"CharProcs");
    let procs = procs.as_dict().unwrap();
    let body = |n: &[u8]| {
        let s = o.after.get_in(procs, n);
        String::from_utf8(
            o.after
                .decode(s.as_stream().unwrap())
                .unwrap()
                .data
                .to_vec(),
        )
        .unwrap()
    };
    // `b` is gone with its name; `a` and `c` stay.
    assert!(!procs.contains(b"b"));
    assert_eq!(procs.len(), 2);
    assert!(body(b"a").contains("re") && body(b"c").contains("re"));
    let enc = o.after.get_in(t3.as_dict().unwrap(), b"Encoding");
    let diffs = o.after.get_in(enc.as_dict().unwrap(), b"Differences");
    let names: Vec<&[u8]> = diffs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n.as_name())
        .collect();
    assert_eq!(names, [&b"a"[..], b".notdef", b"c"]);
    let boxes = [device_box(&r, 0, 2)];
    assert_eq!(
        diff_outside(&render(&o.before, 0), &render(&o.after, 0), &boxes, 0),
        0
    );
}

/// Two Type 3 fonts sharing one `/CharProcs`: a glyph removed from one stays in the other.
#[test]
fn shared_type3_glyph_procedures_stay_for_the_other_font() {
    let mut p = Pdf::new();
    let a = p.stream("", b"500 0 0 0 400 700 d1 0 0 400 700 re f");
    let b = p.stream("", b"500 0 0 0 400 500 d1 0 0 400 500 re f");
    let procs = p.add(format!("<< /a {a} 0 R /b {b} 0 R >>"));
    let t3 = |p: &mut Pdf| {
        p.add(format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 400 700] /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs {procs} 0 R /Encoding << /Type /Encoding /Differences [97 /a /b] >> \
             /FirstChar 97 /LastChar 98 /Widths [500 500] >>"
        ))
    };
    let (f1, f2) = (t3(&mut p), t3(&mut p));
    let font = format!("/Font << /T3 {f1} 0 R /U3 {f2} 0 R >>");
    p.page(
        &font,
        "BT /T3 20 Tf 20 100 Td (ab) Tj ET BT /U3 20 Tf 20 50 Td (b) Tj ET",
        "",
    );
    let file = p.finish();
    let r = Rect::new(29.0, 95.0, 45.0, 120.0);
    let o = redact(file, None, &[(0, Mark::rect(r))]);
    assert_eq!(o.report.pages[0].stats.glyphs_removed, 1);
    assert_eq!(o.report.glyphs_blanked, 1);
    let tree = pagebara_pages::PageTree::load(&o.after);
    let page = o.after.get(tree.pages()[0].r).unwrap();
    let res = o.after.get_in(page.as_dict().unwrap(), b"Resources");
    let fonts = o.after.get_in(res.as_dict().unwrap(), b"Font");
    let names = |key: &[u8]| -> Vec<Vec<u8>> {
        let f = o.after.get_in(fonts.as_dict().unwrap(), key);
        let procs = o.after.get_in(f.as_dict().unwrap(), b"CharProcs");
        procs
            .as_dict()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_bytes().to_vec())
            .collect()
    };
    assert_eq!(names(b"T3"), [b"a".to_vec()]);
    assert_eq!(names(b"U3"), [b"a".to_vec(), b"b".to_vec()]);
    let boxes = [device_box(&r, 0, 2)];
    assert_eq!(
        diff_outside(&render(&o.before, 0), &render(&o.after, 0), &boxes, 0),
        0
    );
}

/// Text in a simple font with the embedded `program` (`key`: `FontFile` or `FontFile3`,
/// `file_dict`: the stream's other entries), one line per `(y, text)`.
fn simple_font_doc(key: &str, file_dict: &str, program: &[u8], lines: &[(f32, &str)]) -> Vec<u8> {
    let mut p = Pdf::new();
    let ff = p.stream(file_dict, program);
    let charset: String = lines
        .iter()
        .flat_map(|l| l.1.chars())
        .filter(char::is_ascii_alphabetic)
        .collect::<std::collections::BTreeSet<char>>()
        .into_iter()
        .map(|c| format!("/{c}"))
        .collect();
    let fd = p.add(format!(
        "<< /Type /FontDescriptor /FontName /Test /Flags 32 /FontBBox [0 0 1000 1000] \
         /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /{key} {ff} 0 R \
         /CharSet ({charset}) >>"
    ));
    let widths = "500 ".repeat(95);
    let font = p.add(format!(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Test /FirstChar 32 /LastChar 126 \
         /Widths [{widths}] /Encoding /WinAnsiEncoding /FontDescriptor {fd} 0 R >>"
    ));
    let content: String = lines
        .iter()
        .map(|(y, t)| format!("BT /F1 14 Tf 20 {y} Td ({t}) Tj ET\n"))
        .collect();
    p.page(&format!("/Font << /F1 {font} 0 R >>"), &content, "");
    p.finish()
}

/// The embedded program of `/F1` on page 0 (parsed, and its length), its stream
/// dictionary and the font descriptor.
fn program(doc: &Document) -> (Program, usize, Dict, Dict) {
    let tree = pagebara_pages::PageTree::load(doc);
    let page = doc.get(tree.pages()[0].r).unwrap();
    let res = doc.get_in(page.as_dict().unwrap(), b"Resources");
    let fonts = doc.get_in(res.as_dict().unwrap(), b"Font");
    let fo = doc.get_in(fonts.as_dict().unwrap(), b"F1");
    let fd = fo.as_dict().unwrap().clone();
    let r = pagebara_font::pdf::font_file_ref(doc, &fd).unwrap();
    let s = doc.get(r).unwrap();
    let s = s.as_stream().unwrap();
    let data = doc.decode(s).unwrap().data;
    let desc = doc
        .get_in(&fd, b"FontDescriptor")
        .as_dict()
        .unwrap()
        .clone();
    let len = data.len();
    (Program::parse(data).unwrap(), len, s.dict.clone(), desc)
}

fn glyph_names(p: &Program) -> Vec<String> {
    (0..p.num_glyphs())
        .map(|g| p.glyph_name(g).unwrap_or_default().to_string())
        .collect()
}

fn any_outline(p: &Program, g: u16) -> Path {
    let mut path = Path::default();
    match p {
        Program::Type1(t) => {
            t.outline(g, &mut path);
        }
        Program::Cff(c) => {
            c.outline(g, &mut path);
        }
        Program::Sfnt(s) => {
            s.outline(g, &mut path);
        }
    }
    path
}

/// `Jane` goes from "Public Jane here" in a simple font: `J`, `a` and `n` lose shape and
/// name (in the program, `/CharSet` and nowhere else); everything else stays.
fn check_simple(key: &str, file_dict: &str, data: &[u8]) -> Out {
    let file = simple_font_doc(
        key,
        file_dict,
        data,
        &[(150.0, "Public Jane here"), (100.0, "Other kept text")],
    );
    let o = redact(file, Some("Jane"), &[]);
    assert_eq!(o.report.glyphs_blanked, 3, "{:?}", o.report.warnings);
    let (bp, _, _, _) = program(&o.before);
    let (ap, len, sd, desc) = program(&o.after);
    assert_eq!(ap.num_glyphs(), bp.num_glyphs());
    let names = glyph_names(&ap);
    for c in ["J", "a", "n"] {
        let g = bp.glyph_by_name(c.as_bytes()).unwrap();
        assert!(!any_outline(&bp, g).is_empty());
        assert!(any_outline(&ap, g).is_empty(), "{c} still has an outline");
        assert!(!names.iter().any(|n| n == c), "{c} still named");
    }
    for c in "ePublichrOtkx".chars() {
        let n = c.to_string();
        let g = bp.glyph_by_name(n.as_bytes()).unwrap();
        assert_eq!(ap.glyph_by_name(n.as_bytes()), Some(g));
        assert_eq!(any_outline(&ap, g), any_outline(&bp, g), "{c} changed");
    }
    let cs = o.after.get_in(&desc, b"CharSet");
    let cs = String::from_utf8_lossy(cs.as_string().unwrap()).to_string();
    assert!(
        !cs.contains("/J") && !cs.contains("/a") && cs.contains("/e"),
        "{cs}"
    );
    if key == "FontFile" {
        let l = |k: &[u8]| o.after.get_in(&sd, k).as_int().unwrap() as usize;
        assert_eq!(l(b"Length1") + l(b"Length2") + l(b"Length3"), len);
    }
    let v = pagebara_redact::verify(&o.bytes, None, &o.report.regions(), &["Jane"]);
    assert!(v.is_clean(), "{v:#?}");
    let boxes: Vec<_> = o.report.pages[0]
        .regions
        .iter()
        .map(|r| device_box(&r.bbox(), 0, 2))
        .collect();
    let (a, b) = (render(&o.before, 0), render(&o.after, 0));
    assert_eq!(diff_outside(&a, &b, &boxes, 0), 0);
    assert!(non_white_inside(&a, boxes[0]) > 0);
    assert!(page_text(&o.after, 0).text.contains("Other kept text"));
    external_checks(&o.bytes, &["Jane"]);
    o
}

/// A Type 1 charstring number.
fn t1_num(out: &mut Vec<u8>, v: i32) {
    match v {
        -107..=107 => out.push((v + 139) as u8),
        108..=1131 => out.extend([((v - 108) / 256 + 247) as u8, ((v - 108) % 256) as u8]),
        -1131..=-108 => out.extend([((-v - 108) / 256 + 251) as u8, ((-v - 108) % 256) as u8]),
        _ => {
            out.push(255);
            out.extend(v.to_be_bytes());
        }
    }
}

fn t1_encrypt(plain: &[u8], key: u16) -> Vec<u8> {
    let mut r = key;
    [0u8; 4]
        .iter()
        .chain(plain)
        .map(|&p| {
            let c = p ^ (r >> 8) as u8;
            r = (u16::from(c).wrapping_add(r))
                .wrapping_mul(52845)
                .wrapping_add(22719);
            c
        })
        .collect()
}

/// A Type 1 font with a glyph for each ASCII letter: a block of its own height drawn by a
/// subroutine of its own. Returns the program and its `Length1`, `Length2` and `Length3`.
fn type1_font() -> (Vec<u8>, [usize; 3]) {
    let letters: Vec<char> = ('A'..='Z').chain('a'..='z').collect();
    let mut subrs: Vec<Vec<u8>> = vec![vec![11]; 4];
    let mut glyphs: Vec<(String, Vec<u8>)> = vec![(".notdef".into(), vec![139, 139, 13, 14])];
    for (i, c) in letters.iter().enumerate() {
        let mut s = Vec::new();
        for (args, op) in [
            (&[0, 0][..], 21u8),
            (&[0, 300 + 8 * i as i32][..], 5),
            (&[100 + 4 * i as i32, 0][..], 5),
            (&[], 9),
        ] {
            for &a in args {
                t1_num(&mut s, a);
            }
            s.push(op);
        }
        s.push(11);
        subrs.push(s);
        let mut g = Vec::new();
        t1_num(&mut g, 40);
        t1_num(&mut g, 500);
        g.push(13);
        t1_num(&mut g, 4 + i as i32);
        g.extend([10, 14]);
        glyphs.push((c.to_string(), g));
    }
    let mut p = b"dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n/ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n/lenIV 4 def\n".to_vec();
    p.extend(format!("/Subrs {} array\n", subrs.len()).bytes());
    for (i, s) in subrs.iter().enumerate() {
        let e = t1_encrypt(s, 4330);
        p.extend(format!("dup {i} {} RD ", e.len()).bytes());
        p.extend(e);
        p.extend(b" NP\n");
    }
    p.extend(format!("ND\n2 index /CharStrings {} dict dup begin\n", glyphs.len()).bytes());
    for (n, g) in &glyphs {
        let e = t1_encrypt(g, 4330);
        p.extend(format!("/{n} {} RD ", e.len()).bytes());
        p.extend(e);
        p.extend(b" ND\n");
    }
    p.extend(b"end\nend\nreadonly put\nnoaccess put\ndup /FontName get exch definefont pop\nmark currentfile closefile\n");
    let enc = t1_encrypt(&p, 55665);
    let mut f = b"%!PS-AdobeFont-1.0: Test\n/FontName /Test def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/FontBBox [0 0 1000 1000] readonly def\n/Encoding StandardEncoding def\ncurrentfile eexec\n".to_vec();
    let l1 = f.len();
    f.extend(&enc);
    let mut trailer = Vec::new();
    for _ in 0..8 {
        trailer.extend([b'0'; 64]);
        trailer.push(b'\n');
    }
    trailer.extend(b"cleartomark\n");
    f.extend(&trailer);
    (f, [l1, enc.len(), trailer.len()])
}

#[test]
fn type1_program_loses_removed_glyphs() {
    let (data, [l1, l2, l3]) = type1_font();
    let o = check_simple(
        "FontFile",
        &format!("/Length1 {l1} /Length2 {l2} /Length3 {l3}"),
        &data,
    );
    // The program is still a valid Type 1 font that draws the remaining text.
    let (ap, _, _, _) = program(&o.after);
    assert!(ap.as_type1().is_some());
}

#[test]
fn cff_glyph_names_are_neutral() {
    let db = pagebara_font::fallback::FontDb::system();
    let cff = db
        .faces()
        .iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "otf") && f.index == 0)
        .find_map(|f| {
            let p = Program::parse(std::fs::read(&f.path).ok()?)?;
            let s = p.as_sfnt()?;
            let c = s.cff().filter(|c| !c.is_cff2() && !c.is_cid())?;
            let ok = "PublicJanehrOtkx"
                .chars()
                .all(|ch| c.glyph_by_name(ch.to_string().as_bytes()).is_some());
            ok.then(|| s.table(b"CFF ").map(<[u8]>::to_vec))?
        });
    let Some(cff) = cff else {
        eprintln!("skipped: no name-keyed OpenType-CFF font with Latin letters is installed");
        return;
    };
    check_simple("FontFile3", "/Subtype /Type1C", &cff);
}

/// Sanitising hidden layers removes their glyphs from fonts too: those in hidden marked
/// content (`Jane`: `J`, `a`, `n`) and in a hidden form (`WAVY`), but not `e`, which
/// visible text shows.
#[test]
fn hidden_layer_glyphs_leave_fonts() {
    let mut p = Pdf::new();
    let ocg = p.add("<< /Type /OCG /Name (Hidden) >>");
    p.catalog = format!("/OCProperties << /OCGs [{ocg} 0 R] /D << /OFF [{ocg} 0 R] >> >>");
    p.page("", "", "");
    let mut doc = Document::open(p.finish()).unwrap();
    let ocg = doc.reference(ocg as u32).unwrap();
    let data = std::fs::read(INTER).unwrap();
    let mut emb = pagebara_font::subset::Embedder::new(data, 0).unwrap();
    let mut show =
        |y: u32, t: &str| format!("BT /F1 14 Tf 20 {y} Td <{}> Tj ET\n", hex(&emb.encode(t)));
    let content = format!(
        "/OC /oc1 BDC {}EMC\n{}{}/Fm1 Do\n",
        show(150, "Jane"),
        show(100, "Public here"),
        show(50, "Other kept text"),
    );
    let form_content = show(20, "WAVY");
    let f = emb.write(&mut doc).unwrap();
    let mut fonts = Dict::new();
    fonts.set(b"F1", Object::Ref(f));
    let mut fres = Dict::new();
    fres.set(b"Font", Object::Dict(fonts.clone()));
    let mut fd = Dict::new();
    fd.set(b"Type", Object::name(b"XObject"));
    fd.set(b"Subtype", Object::name(b"Form"));
    fd.set(b"BBox", ints(&[0, 0, 300, 200]));
    fd.set(b"OC", Object::Ref(ocg));
    fd.set(b"Resources", Object::Dict(fres));
    let form = doc.add(Stream::new(fd, form_content.into_bytes()));
    let mut xo = Dict::new();
    xo.set(b"Fm1", Object::Ref(form));
    let mut props = Dict::new();
    props.set(b"oc1", Object::Ref(ocg));
    let mut res = Dict::new();
    res.set(b"Font", Object::Dict(fonts));
    res.set(b"XObject", Object::Dict(xo));
    res.set(b"Properties", Object::Dict(props));
    let tree = pagebara_pages::PageTree::load(&doc);
    let page = tree.pages()[0].r;
    let mut pd = doc.get(page).unwrap().as_dict().unwrap().clone();
    pd.set(b"Resources", Object::Dict(res));
    let c = doc.add(Stream::new(Dict::new(), content.into_bytes()));
    pd.set(b"Contents", Object::Ref(c));
    doc.set(page, pd);
    let before = Document::open(doc.save_full().unwrap()).unwrap();

    let mut d = before.clone();
    let opts = pagebara_redact::Sanitize {
        hidden_layers: true,
        ..pagebara_redact::Sanitize::none()
    };
    let rep = pagebara_redact::sanitize(&mut d, &opts);
    assert_eq!(rep.hidden_content_pages, 1);
    assert_eq!(rep.glyphs_blanked, 7, "{:?}", rep.warnings);
    let after = Document::open(save(&d).unwrap()).unwrap();
    let (bf, bp) = font(&before);
    let (af, ap) = font(&after);
    // Codes are the glyph ids of the whole font.
    let inter = Program::parse(std::fs::read(INTER).unwrap()).unwrap();
    let code = |c: char| u32::from(inter.as_sfnt().unwrap().glyph_for_char(c).unwrap());
    for c in "JanWAVY".chars() {
        let g = bf.glyph_id(code(c)).unwrap();
        assert!(!outline(&bp, g).is_empty());
        assert!(outline(&ap, g).is_empty(), "{c} still has an outline");
        assert_eq!(af.glyph_id(code(c)), Some(g));
    }
    for c in "ePublichrOtkx".chars() {
        let g = bf.glyph_id(code(c)).unwrap();
        assert_eq!(outline(&ap, g), outline(&bp, g), "{c} changed");
    }
    // The visible page draws as before.
    assert_eq!(
        diff_outside(&render(&before, 0), &render(&after, 0), &[], 0),
        0
    );
}
