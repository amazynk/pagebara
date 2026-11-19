//! Subsetting and Type 0 embedding: subsets keep their outlines, and the written PDFs pass
//! `qpdf --check`, render visibly with `mutool draw` and extract back to the same text
//! (both tools are optional; the checks skip without them).

mod common;

use std::path::{Path as FsPath, PathBuf};
use std::process::Command;

use common::system;
use pagebara_font::subset::{Embedder, cff, truetype};
use pagebara_font::{Font, Path};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

const LIBERATION: &str = "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf";
const DEJAVU_SERIF: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf";
const NIMBUS_OTF: &str = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";
const NOTO_CJK: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";

fn outline(f: &Font, gid: u16) -> Path {
    let mut p = Path::new();
    f.outline(gid, &mut p);
    p
}

#[test]
fn truetype_subset_keeps_outlines() {
    let Some(d) = system(DEJAVU_SERIF) else {
        return;
    };
    let f = Font::parse(d).unwrap();
    let s = f.as_sfnt().unwrap();
    // Composite glyphs pull in their components.
    let gids: Vec<u16> = "AÁé€ fi"
        .chars()
        .filter_map(|c| s.glyph_for_char(c))
        .collect();
    let cmap: Vec<(char, u16)> = "AÁé€ fi"
        .chars()
        .filter_map(|c| Some((c, s.glyph_for_char(c)?)))
        .collect();
    let sub = truetype::subset(s, gids.clone(), &cmap).unwrap();
    assert!(sub.glyphs.len() > gids.len());
    assert!(sub.data.len() < s.data().len() / 10);
    let g = Font::parse(sub.data.clone()).unwrap();
    assert_eq!(usize::from(g.num_glyphs()), sub.glyphs.len());
    for &old in &gids {
        let new = sub.new_id(old).unwrap();
        assert_eq!(outline(&f, old), outline(&g, new));
        assert_eq!(f.advance(old), g.advance(new));
    }
    assert_eq!(
        g.glyph_for_char('é'),
        sub.new_id(s.glyph_for_char('é').unwrap())
    );
    // The checksum adjustment makes the whole file sum to the magic number.
    let sum = sub.data.chunks(4).fold(0u32, |a, c| {
        let mut b = [0; 4];
        b[..c.len()].copy_from_slice(c);
        a.wrapping_add(u32::from_be_bytes(b))
    });
    assert_eq!(sum, 0xB1B0_AFBA);
}

#[test]
fn cff_subset_keeps_outlines() {
    for (path, text) in [(NIMBUS_OTF, "Hello Ågç"), (NOTO_CJK, "中文日本語한국어")] {
        let Some(d) = system(path) else { continue };
        let f = Font::parse(d).unwrap();
        let c = f.as_cff().unwrap();
        let gids: Vec<u16> = text.chars().filter_map(|ch| f.glyph_for_char(ch)).collect();
        let data = cff::subset(c, gids.clone()).unwrap();
        assert!(
            data.len() < c.num_glyphs() as usize * 20,
            "{path}: {}",
            data.len()
        );
        let g = Font::parse(data).unwrap();
        let gc = g.as_cff().unwrap();
        assert!(gc.is_cid());
        for &old in &gids {
            let cid = if c.is_cid() {
                c.charset_id(old).unwrap()
            } else {
                old
            };
            let new = gc.glyph_by_cid(cid).unwrap();
            assert_eq!(outline(&f, old), outline(&g, new), "{path} gid {old}");
            assert_eq!(f.as_cff().unwrap().advance(old), gc.advance(new));
        }
    }
}

/// A one-page document drawing `text` with the embedded font.
fn document(e: &mut Embedder, text: &str) -> Vec<u8> {
    let mut src = b"%PDF-1.7\n".to_vec();
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 80] >>",
    ];
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(src.len());
        src.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = src.len();
    src.extend(b"xref\n0 4\n0000000000 65535 f \n");
    for o in offs {
        src.extend(format!("{o:010} 00000 n \n").bytes());
    }
    src.extend(format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes());
    let mut doc = Document::open(src).unwrap();

    let codes = e.encode(text);
    let font = e.write(&mut doc).unwrap();
    let hex: String = codes.iter().map(|b| format!("{b:02X}")).collect();
    let content = format!("BT /F1 36 Tf 10 25 Td <{hex}> Tj ET");
    let contents = doc.add(Object::Stream(Stream::new(
        Dict::new(),
        content.into_bytes(),
    )));
    let page_ref = ObjRef::new(3, 0);
    let mut page = doc.get(page_ref).unwrap().as_dict().unwrap().clone();
    let mut fonts = Dict::new();
    fonts.set(b"F1", Object::Ref(font));
    let mut res = Dict::new();
    res.set(b"Font", Object::Dict(fonts));
    page.set(b"Resources", Object::Dict(res));
    page.set(b"Contents", Object::Ref(contents));
    doc.set(page_ref, Object::Dict(page));
    doc.save_full().unwrap()
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pagebara-font-embed");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Dark pixels of a rendering (PGM from mutool).
fn dark_pixels(pdf: &FsPath) -> Option<usize> {
    let pgm = pdf.with_extension("pgm");
    let ok = Command::new("mutool")
        .args(["draw", "-q", "-r", "72", "-c", "gray", "-o"])
        .arg(&pgm)
        .arg(pdf)
        .status()
        .ok()?
        .success();
    assert!(ok, "mutool draw failed");
    let img = std::fs::read(&pgm).ok()?;
    // P5 header: magic, width, height, maxval, then one byte per pixel.
    let mut fields = 0;
    let mut i = 0;
    while fields < 4 && i < img.len() {
        while img[i].is_ascii_whitespace() {
            i += 1;
        }
        while i < img.len() && !img[i].is_ascii_whitespace() {
            i += 1;
        }
        fields += 1;
    }
    Some(img[i + 1..].iter().filter(|&&p| p < 128).count())
}

fn extracted_text(pdf: &FsPath) -> String {
    let out = Command::new("mutool")
        .args(["draw", "-q", "-F", "txt", "-o", "-"])
        .arg(pdf)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn embed_and_check(path: &str, index: u32, text: &str, file: &str) {
    let Some(d) = system(path) else { return };
    let mut e = Embedder::new(d, index).unwrap();
    assert!(e.text_width(text) > 0.0);
    let pdf = document(&mut e, text);
    let reparsed = Document::open(pdf.clone()).unwrap();
    assert!(!reparsed.was_repaired());
    let out = out_dir().join(file);
    std::fs::write(&out, &pdf).unwrap();
    eprintln!("{}: {} bytes", out.display(), pdf.len());
    if tool("qpdf") {
        let r = Command::new("qpdf")
            .arg("--check")
            .arg(&out)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&r.stdout);
        assert!(r.status.success(), "qpdf: {log}");
        assert!(log.contains("No syntax or stream encoding errors"), "{log}");
    }
    if tool("mutool") {
        let dark = dark_pixels(&out).unwrap();
        assert!(dark > 300, "{file}: only {dark} dark pixels");
        assert_eq!(extracted_text(&out), text, "{file}");
    }
}

#[test]
fn embed_truetype() {
    embed_and_check(LIBERATION, 0, "Hello, World! äöü €", "truetype.pdf");
}

#[test]
fn embed_opentype_cff() {
    embed_and_check(NIMBUS_OTF, 0, "Quick brown fox ÅÉ", "cff.pdf");
}

#[test]
fn embed_cid_keyed_cff_from_collection() {
    embed_and_check(NOTO_CJK, 0, "中文日本語テスト", "cjk.pdf");
}

/// Glyphs chosen by the caller (here an Arabic presentation form) map back to the text they
/// stand for, and CIDs to glyphs, also for CID-keyed CFF.
#[test]
fn chosen_glyphs_and_cids() {
    for (path, base, form) in [
        (
            "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
            'م',
            '\u{FEE3}',
        ),
        (NOTO_CJK, '密', '密'),
    ] {
        let Some(d) = system(path) else { continue };
        let mut e = Embedder::new(d, 0).unwrap();
        let gid = e.glyph(form);
        assert_ne!(gid, 0, "{path}");
        let cid = e.use_glyph(gid, &base.to_string());
        assert_eq!(e.glyph_of_cid(cid), gid);
        assert_eq!(e.cid(gid), cid);
        let cmap = e.to_unicode();
        let want = format!("<{cid:04X}> <{:04X}>", u32::from(base));
        assert!(cmap.contains(&want), "{path}: {cmap}");
    }
}
