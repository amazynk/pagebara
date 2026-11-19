//! Fonts for PDF/A: embedding a substitute for simple fonts that are not embedded, and
//! `/ToUnicode` maps for text whose characters can be worked out.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::OnceLock;

use pagebara_font::fallback::{self, FontDb, FontRequest};
use pagebara_font::sfnt::Sfnt;
use pagebara_syntax::{Dict, Document, Object, Stream};

use crate::content::FontUse;

fn db() -> &'static FontDb {
    static DB: OnceLock<FontDb> = OnceLock::new();
    DB.get_or_init(FontDb::system)
}

fn flate(dict: Dict, data: &[u8]) -> Stream {
    let mut d = dict;
    d.set(b"Filter", Object::name(b"FlateDecode"));
    Stream::new(d, pagebara_filters::encode_flate(data, 9))
}

fn num(v: f32) -> Object {
    if v.fract() == 0.0 {
        Object::Int(v as i64)
    } else {
        Object::Real((f64::from(v) * 1000.0).round() / 1000.0)
    }
}

/// A TrueType (`glyf`) face that has every character, trying the closest match first.
fn substitute(req: &FontRequest, chars: &[char]) -> Option<Sfnt> {
    let db = db();
    let has_all = |s: &Sfnt| {
        s.has_glyf()
            && chars
                .iter()
                .all(|&c| s.glyph_for_char(c).is_some_and(|g| g != 0))
    };
    let mut tried = Vec::new();
    if let Some(face) = fallback::find(db, req) {
        tried.push(face.path.clone());
        if let Some(pagebara_font::Font::Sfnt(s)) = db.load(face)
            && has_all(&s)
        {
            return Some(*s);
        }
    }
    let class_ok = |f: &fallback::FaceInfo| f.italic == req.italic && (f.weight >= 600) == req.bold;
    for family in ["Liberation Sans", "DejaVu Sans", "Noto Sans", "FreeSans"] {
        for face in db.faces().iter().filter(|f| {
            fallback::normalize(&f.family) == fallback::normalize(family) && class_ok(f)
        }) {
            if tried.contains(&face.path) {
                continue;
            }
            tried.push(face.path.clone());
            if let Some(pagebara_font::Font::Sfnt(s)) = db.load(face)
                && has_all(&s)
            {
                return Some(*s);
            }
        }
    }
    None
}

fn subset_tag(name: &str, codes: &BTreeMap<u32, char>) -> String {
    let mut h: u32 = 2166136261;
    for b in name
        .bytes()
        .chain(codes.keys().flat_map(|c| c.to_be_bytes()))
    {
        h = (h ^ u32::from(b)).wrapping_mul(16777619);
    }
    (0..6)
        .map(|i| char::from(b'A' + ((h >> (i * 5)) % 26) as u8))
        .collect()
}

/// Rewrites a simple font that is not embedded as an embedded TrueType subset of a system
/// font with the same characters. The widths become the substitute's, as PDF/A requires
/// them to match the program. Returns the substitute's name.
pub(crate) fn embed_simple(doc: &mut Document, f: &FontUse) -> Result<(Dict, String), String> {
    let name = String::from_utf8_lossy(f.dict.name(b"BaseFont").unwrap_or(b"Font")).into_owned();
    let mut chars: BTreeMap<u32, char> = BTreeMap::new();
    let mut s = String::new();
    for &code in &f.codes {
        if code > 255 {
            return Err(format!("{name}: codes beyond one byte"));
        }
        s.clear();
        if !f.font.unicode(&*doc, code, &mut s) {
            return Err(format!(
                "{name}: character code {code} has no known character"
            ));
        }
        let Some(c) = s.chars().next() else {
            return Err(format!(
                "{name}: character code {code} has no known character"
            ));
        };
        chars.insert(code, c);
    }
    let spec = pagebara_font::pdf::spec_from_dict(&*doc, &f.dict)
        .ok_or_else(|| format!("{name}: unreadable font"))?;
    let req = FontRequest::from_spec(&spec);
    let list: Vec<char> = chars.values().copied().collect();
    let sub = substitute(&req, &list)
        .ok_or_else(|| format!("{name}: no installed font has all its characters"))?;
    let names: BTreeMap<u32, String> = chars
        .iter()
        .map(|(&code, &c)| (code, pagebara_font::unicode_to_name(c)))
        .collect();
    if let Some((_, n)) = names
        .iter()
        .find(|(_, n)| n.starts_with("uni") && n.len() == 7)
    {
        return Err(format!("{name}: character {n} has no standard glyph name"));
    }
    let pairs: Vec<(char, u16)> = list
        .iter()
        .filter_map(|&c| Some((c, sub.glyph_for_char(c)?)))
        .collect();
    let gids: Vec<u16> = pairs.iter().map(|p| p.1).collect();
    let set = pagebara_font::subset::truetype::subset(&sub, gids, &pairs)
        .ok_or_else(|| format!("{name}: the substitute cannot be subset"))?;
    let upm = f32::from(sub.units_per_em().max(1));
    let scale = 1000.0 / upm;
    let first = *chars.keys().next().unwrap_or(&32);
    let last = *chars.keys().next_back().unwrap_or(&32);
    let widths: Vec<Object> = (first..=last)
        .map(|code| {
            let w = chars
                .get(&code)
                .and_then(|&c| sub.glyph_for_char(c))
                .and_then(|g| sub.advance(g))
                .map_or(0.0, |a| (f32::from(a) * scale).round());
            num(w)
        })
        .collect();
    let ps: String = sub
        .postscript_name()
        .unwrap_or_else(|| "Font".into())
        .chars()
        .filter(|c| c.is_ascii_graphic() && !"[](){}<>/%#".contains(*c))
        .collect();
    let base = format!("{}+{ps}", subset_tag(&name, &chars));
    let mut ff = Dict::new();
    ff.set(b"Length1", set.data.len() as i64);
    let ff = doc.add(flate(ff, &set.data));
    let bb = sub.bbox();
    let (asc, desc, _) = sub.vertical_metrics();
    let os2 = sub.os2();
    let mut flags = 32i64;
    if sub.is_fixed_pitch() {
        flags |= 1;
    }
    if os2.as_ref().and_then(|o| o.is_serif()).unwrap_or(false) {
        flags |= 2;
    }
    if sub.italic_angle() != 0.0 {
        flags |= 64;
    }
    let weight = os2.as_ref().map_or(400.0, |o| f32::from(o.weight_class));
    let cap = os2.as_ref().map_or(0, |o| o.cap_height);
    let mut fd = Dict::new();
    fd.set(b"Type", Object::name(b"FontDescriptor"));
    fd.set(b"FontName", Object::name(base.as_bytes()));
    fd.set(b"Flags", flags);
    fd.set(
        b"FontBBox",
        bb.iter()
            .map(|&v| num((f32::from(v) * scale).round()))
            .collect::<Vec<_>>(),
    );
    fd.set(b"ItalicAngle", num(sub.italic_angle()));
    fd.set(b"Ascent", num((f32::from(asc) * scale).round()));
    fd.set(b"Descent", num((f32::from(desc) * scale).round()));
    fd.set(
        b"CapHeight",
        num(if cap > 0 {
            (f32::from(cap) * scale).round()
        } else {
            (f32::from(asc) * scale).round()
        }),
    );
    fd.set(
        b"StemV",
        num((10.0 + 220.0 * (weight - 50.0) / 900.0).round()),
    );
    fd.set(b"FontFile2", ff);
    let fd = doc.add(fd);
    let mut enc = Dict::new();
    enc.set(b"Type", Object::name(b"Encoding"));
    enc.set(b"BaseEncoding", Object::name(b"WinAnsiEncoding"));
    let mut diffs = Vec::new();
    let mut prev: Option<u32> = None;
    for (&code, n) in &names {
        if prev.is_none_or(|p| p + 1 != code) {
            diffs.push(Object::Int(i64::from(code)));
        }
        diffs.push(Object::name(n.as_bytes()));
        prev = Some(code);
    }
    enc.set(b"Differences", diffs);
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Font"));
    d.set(b"Subtype", Object::name(b"TrueType"));
    d.set(b"BaseFont", Object::name(base.as_bytes()));
    d.set(b"FirstChar", i64::from(first));
    d.set(b"LastChar", i64::from(last));
    d.set(b"Widths", widths);
    d.set(b"Encoding", enc);
    d.set(b"FontDescriptor", fd);
    if let Some(tu) = f.dict.get(b"ToUnicode") {
        d.set(b"ToUnicode", tu.clone());
    }
    Ok((d, ps))
}

/// A `/ToUnicode` CMap for the codes of a font that map to characters. `two_bytes` for
/// Type 0 fonts with `Identity-H` or `Identity-V`.
pub(crate) fn to_unicode(doc: &Document, f: &FontUse, two_bytes: bool) -> Option<Vec<u8>> {
    let mut map = Vec::new();
    let mut s = String::new();
    for &code in &f.codes {
        s.clear();
        if f.font.unicode(doc, code, &mut s) && crate::real_unicode(&s) {
            map.push((code, s.clone()));
        }
    }
    if map.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n");
    out.extend_from_slice(if two_bytes {
        b"<0000> <FFFF>\n"
    } else {
        b"<00> <FF>\n"
    });
    out.extend_from_slice(b"endcodespacerange\n");
    for chunk in map.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (code, text) in chunk {
            let hex: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
            if two_bytes {
                let _ = writeln!(out, "<{code:04X}> <{hex}>");
            } else {
                let _ = writeln!(out, "<{code:02X}> <{hex}>");
            }
        }
        out.extend_from_slice(b"endbfchar\n");
    }
    out.extend_from_slice(b"endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    Some(out)
}

/// `/CIDSet` for a CID font whose CIDs are its glyph ids (`/CIDToGIDMap /Identity`).
pub(crate) fn cid_set(glyphs: u16) -> Vec<u8> {
    let mut v = vec![0u8; usize::from(glyphs).div_ceil(8)];
    for g in 0..usize::from(glyphs) {
        v[g / 8] |= 0x80 >> (g % 8);
    }
    v
}

pub(crate) fn flate_stream(dict: Dict, data: &[u8]) -> Stream {
    flate(dict, data)
}
