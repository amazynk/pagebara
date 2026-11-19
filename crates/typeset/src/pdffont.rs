//! An existing PDF font resource being edited: which characters it shows already, which
//! glyphs of its program can get a code, and writing the additions back.

use std::collections::{BTreeMap, HashMap};

use pagebara_content::font::cmap::CMap;
use pagebara_content::font::{Font as ContentFont, FontKind, embedded_programs, names, std14};
use pagebara_font::outline::{OutlineSink, Path};
use pagebara_font::pdf::{self, FontSpec, PdfFont, PdfFontType};
use pagebara_font::{BaseEncoding, Font, unicode_to_name};
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::cmap;
use crate::kern::Kerner;
use crate::style::{FontStyle, font_style};
use crate::{Error, flate_stream, num};

/// What kind of font, for the encoding rules.
#[derive(Debug)]
enum Kind {
    Simple {
        /// The name of the standard 14 font whose metrics apply (non-embedded fonts only).
        std14: Option<Vec<u8>>,
        /// `/FirstChar` and widths (empty when the font has no `/Widths`).
        first: u32,
        widths: Vec<f32>,
        /// Symbolic TrueType: codes select glyphs through the program's cmap, so no
        /// `/Differences` can be added.
        symbolic_tt: bool,
        /// Codes free for new glyphs, best first.
        free: Vec<u8>,
    },
    Composite {
        /// `Identity-H`: codes are CIDs and can be added.
        identity: bool,
        /// `/CIDToGIDMap` as a stream (TrueType descendants).
        cid_map: Option<Vec<u16>>,
        dw: f32,
    },
    Type3 {
        procs: Vec<Vec<u8>>,
    },
}

/// A code added to the font.
#[derive(Clone, Debug)]
struct Addition {
    text: String,
    /// Width in glyph space thousandths (the `/Widths` or `/W` value).
    width: f32,
    /// A `/Differences` entry to add (simple fonts).
    name: Option<String>,
    /// A `CIDToGIDMap` entry (cid = code).
    map_gid: Option<u16>,
    gid: u16,
}

/// How the font can show one character.
#[derive(Clone, Debug)]
pub(crate) struct Choice {
    pub code: u32,
    pub code_len: u8,
    pub advance: f32,
    pub gid: Option<u16>,
    pub added: bool,
}

#[derive(Debug)]
pub(crate) struct PdfFontState {
    pub r: Option<ObjRef>,
    pub dict: Dict,
    pub style: FontStyle,
    kind: Kind,
    content: ContentFont,
    program: Option<PdfFont>,
    spec: Option<FontSpec>,
    kerner: Option<Kerner>,
    /// Text of each code as extraction gives it now.
    code_text: HashMap<u32, String>,
    /// Codes by text, with their lengths.
    by_text: HashMap<String, Vec<(u32, u8)>>,
    /// Codes `/ToUnicode` maps.
    to_unicode: Option<(CMap, Vec<u8>)>,
    usable: HashMap<u32, bool>,
    added: BTreeMap<u32, Addition>,
    added_by_text: HashMap<String, u32>,
    /// Codes added since the font was last written.
    pub pending: bool,
}

/// Glyph names standing for `c`, most likely first.
fn names_for(c: char) -> Vec<String> {
    let u = c as u32;
    let mut v = vec![unicode_to_name(c)];
    for n in [
        format!("uni{u:04X}"),
        format!("u{u:04X}"),
        format!("u{u:05X}"),
    ] {
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v
}

fn is_blank_char(t: &str) -> bool {
    t.chars()
        .all(|c| c.is_whitespace() || c == '\u{200B}' || c == '\u{00AD}')
}

/// Whether glyph `gid` draws something.
fn has_outline(font: &Font, gid: u16) -> bool {
    let mut p = Path::new();
    font.outline(gid, &mut p) && !p.is_empty()
}

impl PdfFontState {
    pub(crate) fn open(doc: &Document, r: Option<ObjRef>, dict: Dict) -> Result<Self, Error> {
        let content = ContentFont::load_with(doc, &dict, Some(&*embedded_programs()));
        let spec = pdf::spec_from_dict(doc, &dict);
        let program = match (&spec, pdf::embedded_program(doc, &dict)) {
            (Some(s), Some(f)) => Some(PdfFont::new(f, s.clone())),
            _ => None,
        };
        let style = font_style(doc, &dict);
        let to_unicode = match &*doc.get_in(&dict, b"ToUnicode") {
            Object::Stream(s) => doc
                .decode(s)
                .ok()
                .map(|d| (CMap::parse(&d.data), d.data.to_vec())),
            _ => None,
        };
        let kind = match content.kind {
            FontKind::Type3 => {
                let procs = doc.get_in(&dict, b"CharProcs");
                let procs = procs
                    .as_dict()
                    .map(|d| d.iter().map(|(k, _)| k.as_bytes().to_vec()).collect())
                    .unwrap_or_default();
                Kind::Type3 { procs }
            }
            FontKind::Cid0 | FontKind::Cid2 => {
                let identity = matches!(
                    doc.get_in(&dict, b"Encoding").as_name(),
                    Some(b"Identity-H")
                );
                let desc = content.descendant.clone().unwrap_or_default();
                let cid_map = match &*doc.get_in(&desc, b"CIDToGIDMap") {
                    Object::Stream(s) => doc.decode(s).ok().map(|d| {
                        d.data
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|c| u16::from_be_bytes(*c))
                            .collect()
                    }),
                    _ => None,
                };
                let dw = doc.get_in(&desc, b"DW").as_f64().unwrap_or(1000.0) as f32;
                Kind::Composite {
                    identity,
                    cid_map,
                    dw,
                }
            }
            _ => {
                let widths: Vec<f32> = doc
                    .get_in(&dict, b"Widths")
                    .as_array()
                    .unwrap_or_default()
                    .iter()
                    .map(|w| doc.resolve(w).as_f64().unwrap_or(0.0) as f32)
                    .collect();
                let first = doc
                    .get_in(&dict, b"FirstChar")
                    .as_int()
                    .unwrap_or(0)
                    .clamp(0, 255) as u32;
                let std14 = if program.is_none() {
                    style_std14(&content)
                } else {
                    None
                };
                let symbolic_tt = content.kind == FontKind::TrueType && content.flags & 4 != 0;
                Kind::Simple {
                    std14,
                    first,
                    widths,
                    symbolic_tt,
                    free: Vec::new(),
                }
            }
        };
        let kerner = program
            .as_ref()
            .and_then(|p| p.font().as_sfnt().map(Kerner::new));
        let mut st = PdfFontState {
            r,
            dict,
            style,
            kind,
            content,
            program,
            spec,
            kerner,
            code_text: HashMap::new(),
            by_text: HashMap::new(),
            to_unicode,
            usable: HashMap::new(),
            added: BTreeMap::new(),
            added_by_text: HashMap::new(),
            pending: false,
        };
        st.read_codes(doc);
        st.find_free_codes();
        Ok(st)
    }

    /// Every code of the font with the text extraction gives it.
    fn read_codes(&mut self, doc: &Document) {
        let mut codes: Vec<(u32, u8)> = Vec::new();
        if self.content.is_composite() {
            for hi in 0..=255u8 {
                let (c, n) = self.content.next_code(&[hi, 0]);
                if n == 1 {
                    codes.push((c, 1));
                    continue;
                }
                for lo in 0..=255u8 {
                    let (c, n) = self.content.next_code(&[hi, lo]);
                    if n == 2 {
                        codes.push((c, 2));
                    }
                }
            }
        } else {
            codes.extend((0..=255u32).map(|c| (c, 1)));
        }
        let mut s = String::new();
        for (code, len) in codes {
            s.clear();
            if !self.content.unicode(doc, code, &mut s) || s.is_empty() {
                continue;
            }
            if !self.content.is_composite() && self.to_unicode.is_none() {
                // Without /ToUnicode, a guess that the program's own glyph name contradicts
                // is not trusted for new text.
                if let Some(g) = self
                    .program
                    .as_ref()
                    .and_then(|p| p.simple_glyph(code as u8))
                    && let Some(u) = &g.unicode
                    && **u != *s
                {
                    self.code_text.insert(code, s.clone());
                    continue;
                }
            }
            self.code_text.insert(code, s.clone());
            self.by_text.entry(s.clone()).or_default().push((code, len));
        }
    }

    fn tounicode_has(&self, code: u32) -> bool {
        let mut s = String::new();
        self.to_unicode
            .as_ref()
            .is_some_and(|(m, _)| m.unicode(code, &mut s))
    }

    fn find_free_codes(&mut self) {
        let mut ranked: Vec<(u8, u8, u8, u8)> = Vec::new();
        let Kind::Simple {
            std14,
            first,
            widths,
            symbolic_tt,
            ..
        } = &self.kind
        else {
            return;
        };
        if *symbolic_tt {
            return;
        }
        for code in 1..=255u8 {
            if code == 32 || self.tounicode_has(u32::from(code)) {
                continue;
            }
            let c = u32::from(code);
            let name = self.content.glyph_name(c);
            let in_range = c >= *first && ((c - first) as usize) < widths.len();
            if widths.is_empty() {
                // Standard 14: every named code is in use.
                if name.is_some_and(|n| std14.as_ref().is_some_and(|s| std14_width(s, n).is_some()))
                {
                    continue;
                }
            } else if in_range && widths[(c - first) as usize] != 0.0 {
                continue;
            }
            let glyph = self.program.as_ref().is_some_and(|p| p.glyph(c) != 0);
            let pref = match code {
                128..=255 => 0,
                1..=31 | 127 => 1,
                _ => 2,
            };
            ranked.push((u8::from(name.is_some()), u8::from(glyph), pref, code));
        }
        ranked.sort_unstable();
        if let Kind::Simple { free, .. } = &mut self.kind {
            *free = ranked.into_iter().map(|r| r.3).collect();
        }
    }

    /// Whether [`PdfFontState::add`] would succeed for `c`.
    pub(crate) fn can_add(&self, c: char) -> bool {
        if self.added_by_text.contains_key(&c.to_string()) {
            return true;
        }
        match &self.kind {
            Kind::Type3 { .. } => false,
            Kind::Simple { .. } => self.plan_simple(c).is_some(),
            Kind::Composite { .. } => self.plan_composite(c).is_some(),
        }
    }

    /// Ascent and descent in text space per unit size.
    pub(crate) fn metrics(&self) -> (f32, f32) {
        (self.content.ascent, self.content.descent)
    }

    /// The program's glyph for a code, if the program draws one (or a blank glyph for blank
    /// text).
    fn code_usable(&mut self, code: u32) -> bool {
        if let Some(&u) = self.usable.get(&code) {
            return u;
        }
        let text = self.code_text.get(&code).cloned().unwrap_or_default();
        let u = match &self.kind {
            Kind::Type3 { procs } => self
                .content
                .glyph_name(code)
                .is_some_and(|n| procs.iter().any(|p| p == n)),
            Kind::Simple {
                std14,
                first,
                widths,
                ..
            } => {
                let in_range = code >= *first && ((code - first) as usize) < widths.len();
                let width_ok = if widths.is_empty() { true } else { in_range };
                width_ok
                    && match (&self.program, std14) {
                        (Some(p), _) => {
                            let g = p.glyph(code);
                            g != 0 && (is_blank_char(&text) || has_outline(p.font(), g))
                        }
                        (None, Some(s)) => self
                            .content
                            .glyph_name(code)
                            .is_some_and(|n| std14_width(s, n).is_some()),
                        (None, None) => self.content.width(code) > 0.0,
                    }
            }
            // With Identity codes, text not from /ToUnicode was guessed from the program,
            // which other readers do not do: such codes get a /ToUnicode entry (and a
            // width) as additions.
            Kind::Composite { identity: true, .. } if !self.tounicode_has(code) => false,
            Kind::Composite { .. } => match &self.program {
                Some(p) => {
                    let g = p.glyph(self.content.cid(code));
                    g != 0 && (is_blank_char(&text) || has_outline(p.font(), g))
                }
                None => self.content.width(code) > 0.0,
            },
        };
        self.usable.insert(code, u);
        u
    }

    /// Width (text space per unit size) of a code.
    fn width_of(&self, code: u32) -> f32 {
        if let Some(a) = self.added.get(&code) {
            return a.width / 1000.0;
        }
        self.content.width(code)
    }

    fn glyph_of_code(&self, code: u32) -> Option<u16> {
        if let Some(a) = self.added.get(&code) {
            return Some(a.gid).filter(|&g| g != 0);
        }
        let p = self.program.as_ref()?;
        Some(p.glyph(self.content.cid(code))).filter(|&g| g != 0)
    }

    /// The code already showing `text` with a glyph and a width.
    pub(crate) fn existing(&mut self, text: &str) -> Option<Choice> {
        if let Some(&code) = self.added_by_text.get(text) {
            return Some(self.choice(code, true));
        }
        let codes = self.by_text.get(text)?.clone();
        // Codes given to other text by this session no longer mean `text`.
        let (code, len) = codes
            .into_iter()
            .find(|&(c, _)| !self.added.contains_key(&c) && self.code_usable(c))?;
        let mut ch = self.choice(code, false);
        ch.code_len = len;
        Some(ch)
    }

    fn choice(&self, code: u32, added: bool) -> Choice {
        Choice {
            code,
            code_len: if self.content.is_composite() { 2 } else { 1 },
            advance: self.width_of(code),
            gid: self.glyph_of_code(code),
            added,
        }
    }

    /// The program's glyph for `c`.
    fn find_glyph(&self, c: char) -> Option<u16> {
        let p = self.program.as_ref()?;
        let f = p.font();
        let mut cands: Vec<u16> = Vec::new();
        if let Some(s) = f.as_sfnt()
            && let Some(g) = s.glyph_for_char(c)
        {
            cands.push(g);
        }
        for n in names_for(c) {
            if let Some(g) = f.glyph_by_name(n.as_bytes()) {
                cands.push(g);
            }
        }
        if cands.is_empty() && f.num_glyphs() < 4096 {
            let mut s = String::new();
            for g in 1..f.num_glyphs() {
                if let Some(n) = f.glyph_name(g) {
                    s.clear();
                    if pagebara_font::name_to_unicode(n.as_bytes(), &mut s)
                        && s.chars().eq(std::iter::once(c))
                    {
                        cands.push(g);
                        break;
                    }
                }
            }
        }
        let blank = c.is_whitespace();
        cands
            .into_iter()
            .find(|&g| g != 0 && (blank || has_outline(f, g)))
    }

    /// Adds a code for `c` when the program (or the standard font) has its glyph.
    pub(crate) fn add(&mut self, c: char) -> Option<Choice> {
        let text = c.to_string();
        if let Some(&code) = self.added_by_text.get(&text) {
            return Some(self.choice(code, true));
        }
        let (code, add) = match &self.kind {
            Kind::Type3 { .. } => return None,
            Kind::Simple { .. } => self.plan_simple(c)?,
            Kind::Composite { .. } => self.plan_composite(c)?,
        };
        if let Kind::Simple { free, .. } = &mut self.kind {
            free.retain(|&f| u32::from(f) != code);
        }
        self.added.insert(code, add);
        self.added_by_text.insert(text, code);
        self.pending = true;
        Some(self.choice(code, true))
    }

    fn plan_simple(&self, c: char) -> Option<(u32, Addition)> {
        let Kind::Simple {
            std14,
            first,
            widths,
            symbolic_tt,
            free,
        } = &self.kind
        else {
            return None;
        };
        let text = c.to_string();
        // A code with the glyph but no width (outside `/Widths`): only the width is missing.
        if let Some(codes) = self.by_text.get(&text) {
            for &(code, _) in codes {
                let in_range = code >= *first && ((code - first) as usize) < widths.len();
                if widths.is_empty() || in_range || self.added.contains_key(&code) {
                    continue;
                }
                let found = match (&self.program, std14) {
                    (Some(p), _) => {
                        let g = p.glyph(code);
                        (g != 0 && (c.is_whitespace() || has_outline(p.font(), g)))
                            .then(|| Some((g, self.program_width(g)?)))
                            .flatten()
                    }
                    (None, Some(s)) => self
                        .content
                        .glyph_name(code)
                        .and_then(|n| std14_width(s, n))
                        .map(|w| (0, f32::from(w))),
                    (None, None) => None,
                };
                if let Some((gid, width)) = found {
                    return Some((
                        code,
                        Addition {
                            text,
                            width,
                            name: None,
                            map_gid: None,
                            gid,
                        },
                    ));
                }
            }
        }
        if *symbolic_tt {
            return self.plan_symbolic(c);
        }
        match (&self.program, std14) {
            (Some(p), _) => {
                let gid = self.find_glyph(c)?;
                let f = p.font();
                let name = match f.format() {
                    pagebara_font::Format::TrueType => {
                        // By Unicode through the (3,1) cmap when it has the character,
                        // else by the `post` name.
                        let via_cmap = f.as_sfnt().is_some_and(|s| {
                            s.find_subtable(3, 1).is_some() || s.find_subtable(3, 10).is_some()
                        }) && f.as_sfnt().and_then(|s| s.glyph_for_char(c))
                            == Some(gid);
                        if via_cmap {
                            unicode_to_name(c)
                        } else {
                            f.glyph_name(gid)?.to_owned()
                        }
                    }
                    _ => f.glyph_name(gid)?.to_owned(),
                };
                let width = self.program_width(gid)?;
                let code = free
                    .iter()
                    .copied()
                    .find(|&code| self.selects(code, &name) == Some(gid))?;
                Some((
                    u32::from(code),
                    Addition {
                        text,
                        width,
                        name: Some(name),
                        map_gid: None,
                        gid,
                    },
                ))
            }
            (None, Some(s)) => {
                let name = names_for(c)
                    .into_iter()
                    .find(|n| std14_width(s, n.as_bytes()).is_some())?;
                let width = f32::from(std14_width(s, name.as_bytes())?);
                let code = *free.first()?;
                Some((
                    u32::from(code),
                    Addition {
                        text,
                        width,
                        name: Some(name),
                        map_gid: None,
                        gid: 0,
                    },
                ))
            }
            (None, None) => None,
        }
    }

    /// Symbolic TrueType: a code the program's built-in (cmap) encoding maps to the glyph,
    /// not yet in use.
    fn plan_symbolic(&self, c: char) -> Option<(u32, Addition)> {
        let gid = self.find_glyph(c)?;
        let p = self.program.as_ref()?;
        let code = (1..=255u32).find(|&code| {
            p.glyph(code) == gid
                && !self.code_text.contains_key(&code)
                && !self.added.contains_key(&code)
        })?;
        let width = self.program_width(gid)?;
        Some((
            code,
            Addition {
                text: c.to_string(),
                width,
                name: None,
                map_gid: None,
                gid,
            },
        ))
    }

    /// The glyph the font would select for `code` with a `/Differences` entry `name`.
    fn selects(&self, code: u8, name: &str) -> Option<u16> {
        let p = self.program.as_ref()?;
        let mut spec = self.spec.clone()?;
        if spec.kind == PdfFontType::TrueType && spec.encoding.base.is_none() {
            spec.encoding.base = Some(BaseEncoding::WinAnsi);
        }
        spec.has_encoding = true;
        spec.encoding
            .differences
            .push((code, name.as_bytes().into()));
        let f = PdfFont::new(p.program().clone(), spec);
        Some(f.glyph(u32::from(code))).filter(|&g| g != 0)
    }

    fn plan_composite(&self, c: char) -> Option<(u32, Addition)> {
        let Kind::Composite {
            identity, cid_map, ..
        } = &self.kind
        else {
            return None;
        };
        if !identity {
            return None;
        }
        let p = self.program.as_ref()?;
        let gid = self.find_glyph(c)?;
        let f = p.font();
        let text = c.to_string();
        let width = self.program_width(gid)?;
        let free_cid = |cid: u32| {
            self.code_text.get(&cid).is_none_or(|t| *t == text) && !self.added.contains_key(&cid)
        };
        let (cid, map_gid) = match (self.content.kind, cid_map) {
            (FontKind::Cid2, Some(map)) => {
                let reuse = map
                    .iter()
                    .enumerate()
                    .find(|&(cid, &g)| g == gid && free_cid(cid as u32))
                    .map(|(cid, _)| cid as u32);
                match reuse {
                    Some(cid) => (cid, None),
                    None => {
                        let next = self
                            .added
                            .keys()
                            .copied()
                            .filter(|&c| c as usize >= map.len())
                            .max()
                            .map_or(map.len() as u32, |c| c + 1)
                            .max(1);
                        if next > 0xFFFF {
                            return None;
                        }
                        (next, Some(gid))
                    }
                }
            }
            (FontKind::Cid2, None) => (u32::from(gid), None),
            _ => {
                let cff = f.as_cff().or_else(|| f.as_sfnt().and_then(|s| s.cff()));
                match cff {
                    Some(c) if c.is_cid() => (u32::from(c.charset_id(gid)?), None),
                    Some(_) => (u32::from(gid), None),
                    None => return None,
                }
            }
        };
        if !free_cid(cid) || p.glyph(cid) != gid && map_gid.is_none() {
            return None;
        }
        Some((
            cid,
            Addition {
                text,
                width,
                name: None,
                map_gid,
                gid,
            },
        ))
    }

    /// The program's advance of `gid` in glyph space thousandths.
    fn program_width(&self, gid: u16) -> Option<f32> {
        let f = self.program.as_ref()?.font();
        let w = f.advance(gid)?;
        let m = f.font_matrix();
        let (x, _) = m.apply(w, 0.0);
        let (o, _) = m.apply(0.0, 0.0);
        Some((x - o) * 1000.0)
    }

    /// Kerning between two glyphs, in text space per unit size.
    pub(crate) fn kern(&self, left: u16, right: u16) -> f32 {
        let (Some(k), Some(p)) = (&self.kerner, &self.program) else {
            return 0.0;
        };
        let Some(s) = p.font().as_sfnt() else {
            return 0.0;
        };
        k.kern(s, left, right) as f32 / f32::from(s.units_per_em().max(1))
    }

    /// The font's description for finding a substitute program.
    pub(crate) fn spec(&self) -> Option<&FontSpec> {
        self.spec.as_ref()
    }

    pub(crate) fn has_program(&self) -> bool {
        self.program.is_some()
    }

    /// Draws the glyph of a code in text space per unit size, with `substitute` standing in
    /// for a font without a program. False when there is no glyph.
    pub(crate) fn outline(
        &self,
        code: u32,
        substitute: Option<&PdfFont>,
        sink: &mut dyn OutlineSink,
    ) -> bool {
        let Some(p) = self.program.as_ref().or(substitute) else {
            return false;
        };
        let mut t = Transformed {
            m: p.font_matrix(),
            sink,
        };
        if let Some(a) = self.added.get(&code) {
            let gid = if self.program.is_some() {
                a.gid
            } else {
                a.text
                    .chars()
                    .next()
                    .and_then(|c| p.glyph_for_char(c))
                    .unwrap_or(0)
            };
            return gid != 0 && p.font().outline(gid, &mut t);
        }
        let c = if self.content.is_composite() {
            self.content.cid(code)
        } else {
            code
        };
        p.outline(c, &mut t)
    }

    pub(crate) fn is_word_space(&self, code: u32, len: u8) -> bool {
        len == 1 && code == 32 && !self.content.is_composite()
    }

    /// The font dictionary with the additions (and new objects for what it refers to).
    pub(crate) fn write(&self, doc: &mut Document) -> Dict {
        let mut d = self.dict.clone();
        let entries: Vec<cmap::Entry> = self
            .added
            .iter()
            .map(|(&c, a)| {
                (
                    c,
                    if self.content.is_composite() { 2 } else { 1 },
                    a.text.clone(),
                )
            })
            .collect();
        match &self.kind {
            Kind::Simple { first, widths, .. } => {
                self.write_encoding(doc, &mut d);
                if !widths.is_empty() {
                    let lo = self
                        .added
                        .keys()
                        .copied()
                        .chain([*first])
                        .min()
                        .unwrap_or(0);
                    let hi = self
                        .added
                        .keys()
                        .copied()
                        .chain([first + widths.len() as u32 - 1])
                        .max()
                        .unwrap_or(0);
                    let mut w = vec![0.0f32; (hi - lo + 1) as usize];
                    for (i, &v) in widths.iter().enumerate() {
                        w[(first + i as u32 - lo) as usize] = v;
                    }
                    for (&c, a) in &self.added {
                        w[(c - lo) as usize] = a.width;
                    }
                    d.set(b"FirstChar", Object::Int(i64::from(lo)));
                    d.set(b"LastChar", Object::Int(i64::from(hi)));
                    d.set(b"Widths", Object::Array(w.into_iter().map(num).collect()));
                }
            }
            Kind::Composite { cid_map, dw, .. } => {
                let df = doc.get_in(&d, b"DescendantFonts");
                let mut desc = df
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|o| doc.resolve(o).as_dict().cloned())
                    .unwrap_or_default();
                let mut w: Vec<Object> = doc
                    .get_in(&desc, b"W")
                    .as_array()
                    .map(<[Object]>::to_vec)
                    .unwrap_or_default();
                let mut run: Option<(u32, Vec<Object>)> = None;
                for (&cid, a) in &self.added {
                    if (a.width - dw).abs() < 0.5 {
                        continue;
                    }
                    match &mut run {
                        Some((s, ws)) if *s + ws.len() as u32 == cid => ws.push(num(a.width)),
                        _ => {
                            if let Some((s, ws)) = run.take() {
                                w.push(Object::Int(i64::from(s)));
                                w.push(Object::Array(ws));
                            }
                            run = Some((cid, vec![num(a.width)]));
                        }
                    }
                }
                if let Some((s, ws)) = run {
                    w.push(Object::Int(i64::from(s)));
                    w.push(Object::Array(ws));
                }
                desc.set(b"W", Object::Array(w));
                if let Some(map) = cid_map {
                    let mut map = map.clone();
                    for (&cid, a) in &self.added {
                        if let Some(g) = a.map_gid {
                            if map.len() <= cid as usize {
                                map.resize(cid as usize + 1, 0);
                            }
                            map[cid as usize] = g;
                        }
                    }
                    let data: Vec<u8> = map.iter().flat_map(|g| g.to_be_bytes()).collect();
                    let s = doc.add(flate_stream(Dict::new(), &data));
                    desc.set(b"CIDToGIDMap", Object::Ref(s));
                }
                let dr = doc.add(Object::Dict(desc));
                d.set(b"DescendantFonts", Object::Array(vec![Object::Ref(dr)]));
            }
            Kind::Type3 { .. } => {}
        }
        let len = if self.content.is_composite() { 2 } else { 1 };
        let tu = match &self.to_unicode {
            Some((_, data)) => cmap::append(data, len, &entries),
            None => {
                // The codes in use (those with a width) keep the text they extract to now.
                let in_use = |c: u32| match &self.kind {
                    Kind::Simple { first, widths, .. } if !widths.is_empty() => {
                        c >= *first && widths.get((c - first) as usize).is_some_and(|w| *w != 0.0)
                    }
                    _ => true,
                };
                let mut all: Vec<cmap::Entry> = self
                    .code_text
                    .iter()
                    .filter(|(c, _)| !self.added.contains_key(c) && in_use(**c))
                    .map(|(&c, t)| (c, len, t.clone()))
                    .collect();
                all.extend(entries);
                all.sort_by_key(|e| e.0);
                cmap::new_cmap(len, &all)
            }
        };
        let tu = doc.add(flate_stream(Dict::new(), &tu));
        d.set(b"ToUnicode", Object::Ref(tu));
        d
    }

    fn write_encoding(&self, doc: &Document, d: &mut Dict) {
        let new: Vec<(u8, &str)> = self
            .added
            .iter()
            .filter_map(|(&c, a)| Some((c as u8, a.name.as_deref()?)))
            .collect();
        if new.is_empty() {
            return;
        }
        let enc = (*doc.get_in(d, b"Encoding")).clone();
        let mut ed = Dict::new();
        let mut diffs: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        match &enc {
            Object::Name(n) => ed.set(b"BaseEncoding", Object::Name(n.clone())),
            Object::Dict(old) => {
                ed = old.clone();
                let mut code = 0i64;
                let arr = doc.get_in(old, b"Differences");
                for o in arr.as_array().unwrap_or_default() {
                    match o {
                        Object::Int(_) | Object::Real(_) => code = o.as_f64().unwrap_or(0.0) as i64,
                        Object::Name(n) => {
                            if let Ok(c) = u8::try_from(code) {
                                diffs.insert(c, n.as_bytes().to_vec());
                            }
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                if self.content.kind == FontKind::TrueType {
                    ed.set(b"BaseEncoding", Object::name(b"WinAnsiEncoding"));
                }
            }
        }
        for (c, n) in new {
            diffs.insert(c, n.as_bytes().to_vec());
        }
        let mut arr = Vec::new();
        let mut prev: Option<u8> = None;
        for (c, n) in diffs {
            if prev.is_none_or(|p| u16::from(p) + 1 != u16::from(c)) {
                arr.push(Object::Int(i64::from(c)));
            }
            arr.push(Object::name(&n));
            prev = Some(c);
        }
        ed.set(b"Type", Object::name(b"Encoding"));
        ed.set(b"Differences", Object::Array(arr));
        d.set(b"Encoding", Object::Dict(ed));
    }
}

fn style_std14(content: &ContentFont) -> Option<Vec<u8>> {
    let name = content.base_font.as_ref()?;
    std14::lookup(name.as_bytes()).map(|s| s.name.as_bytes().to_vec())
}

/// The AFM width of glyph `name` in standard font `std` (Courier: any standard Latin
/// glyph).
fn std14_width(std: &[u8], name: &[u8]) -> Option<u16> {
    let s = std14::lookup(std)?;
    let id = names::glyph_id(name)?;
    let table = if s.widths.is_empty() {
        std14::lookup(b"Helvetica")?.widths
    } else {
        s.widths
    };
    let i = table.binary_search_by_key(&id, |w| w.0).ok()?;
    Some(if s.widths.is_empty() { 600 } else { table[i].1 })
}

/// An outline sink applying a matrix first.
pub(crate) struct Transformed<'a> {
    pub m: pagebara_font::Matrix,
    pub sink: &'a mut dyn OutlineSink,
}

impl OutlineSink for Transformed<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.m.apply(x, y);
        self.sink.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.m.apply(x, y);
        self.sink.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.m.apply(x1, y1);
        let (x, y) = self.m.apply(x, y);
        self.sink.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.m.apply(x1, y1);
        let (x2, y2) = self.m.apply(x2, y2);
        let (x, y) = self.m.apply(x, y);
        self.sink.curve_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.sink.close();
    }
}
