//! Setting stamp text: standard fonts or installed families, with fallback fonts embedded
//! for characters they lack. All stamps of one operation share one font resource
//! dictionary, written when the operation finishes.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use pagebara_font::fallback::{FontDb, normalize};
use pagebara_syntax::{Dict, Document, ObjRef, Object};
use pagebara_typeset::{FontId, FontSession, RunFont, WriteMode, fmt_num, standard_font_dict};

const STANDARD: [&str; 12] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
];

/// The fonts offered by name: the standard ones (always available).
pub fn standard_fonts() -> &'static [&'static str] {
    &STANDARD
}

/// One line of set text.
#[derive(Debug, Default, Clone)]
pub struct Line {
    /// Text-showing operators (`Tf` and `TJ`) to put inside `BT … ET` after `Td`.
    pub ops: String,
    /// Width in points.
    pub width: f64,
    /// Ascent and descent (negative) in points.
    pub ascent: f64,
    pub descent: f64,
}

/// Sets lines of text for new content: standard fonts or installed families, fallback
/// fonts embedded for other characters. All lines share one `/Font` resource dictionary
/// ([`Typesetter::resources`]), filled by [`Typesetter::finish`].
pub struct Typesetter {
    session: FontSession,
    fonts: HashMap<String, FontId>,
    /// Standard fonts opened, to write as objects when the session leaves them as they are.
    standard: Vec<(FontId, String)>,
    resources: ObjRef,
    pub missing: Vec<char>,
}

impl std::fmt::Debug for Typesetter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Typesetter").finish_non_exhaustive()
    }
}

fn name_of(rf: RunFont) -> String {
    match rf {
        RunFont::Font(id) => format!("PagebaraF{}", id.0),
        RunFont::Fallback(id) => format!("PagebaraFB{}", id.0),
    }
}

impl Typesetter {
    pub fn new(doc: &mut Document, db: Option<Arc<FontDb>>) -> Typesetter {
        Typesetter {
            session: FontSession::new(db),
            fonts: HashMap::new(),
            standard: Vec::new(),
            resources: doc.add(Dict::new()),
            missing: Vec::new(),
        }
    }

    /// The shared `/Font` resource dictionary (filled by [`Typesetter::finish`]).
    pub fn resources(&self) -> ObjRef {
        self.resources
    }

    fn font(&mut self, doc: &Document, spec: &str) -> FontId {
        if let Some(&id) = self.fonts.get(spec) {
            return id;
        }
        let id = self.open(doc, spec);
        self.fonts.insert(spec.to_owned(), id);
        id
    }

    fn open(&mut self, doc: &Document, spec: &str) -> FontId {
        let want = normalize(spec);
        let std = STANDARD.iter().find(|s| normalize(s) == want).or_else(|| {
            // "Times", "Arial Bold" and the like map onto the standard fonts.
            let (fam, style) = pagebara_font::fallback::split_style(spec);
            let st = normalize(style);
            pagebara_typeset::standard_name(fam, st.contains("bold"), st.contains("italic"))
                .and_then(|n| STANDARD.iter().find(|s| **s == n))
        });
        if std.is_none()
            && let Some(face) = self.installed(spec)
            && let Ok(id) = self.session.open_face(&face)
        {
            return id;
        }
        let name = std.copied().unwrap_or("Helvetica");
        let id = self
            .session
            .open_font_dict(doc, standard_font_dict(name))
            .expect("a standard font dictionary");
        self.standard.push((id, name.to_owned()));
        id
    }

    fn installed(&self, spec: &str) -> Option<pagebara_font::fallback::FaceInfo> {
        let db = pagebara_typeset::system_fonts();
        let want = normalize(spec);
        let mut best: Option<&pagebara_font::fallback::FaceInfo> = None;
        for f in db.faces().iter().filter(|f| f.embeddable()) {
            let fam = normalize(&f.family);
            let full = normalize(&f.postscript_name);
            if fam == want || full == want {
                let better = best.is_none_or(|b| {
                    (f.italic as u8, f.weight.abs_diff(400))
                        < (b.italic as u8, b.weight.abs_diff(400))
                });
                if better {
                    best = Some(f);
                }
            }
        }
        best.cloned()
    }

    /// Sets `text` in `font` at `size`.
    pub fn line(&mut self, doc: &Document, font: &str, size: f64, text: &str) -> Line {
        let id = self.font(doc, font);
        let enc = self.session.encode(id, text);
        self.missing.extend(enc.missing.iter().map(|m| m.1));
        let (a, d) = self.session.metrics(id);
        let mut ops = String::new();
        let mut width = 0.0;
        for run in &enc.runs {
            let _ = write!(
                ops,
                "/{} {} Tf {} TJ ",
                name_of(run.font),
                fmt_num(size as f32),
                run.tj_operand()
            );
            width += f64::from(run.width());
        }
        Line {
            ops,
            width: width * size,
            ascent: f64::from(a) * size,
            descent: f64::from(d) * size,
        }
    }

    /// Writes the fonts used and fills the shared resource dictionary.
    pub fn finish(mut self, doc: &mut Document) -> Vec<char> {
        let ch = self.session.finish(doc, WriteMode::Copy);
        let mut res = Dict::new();
        for &id in self.fonts.values() {
            let rf = RunFont::Font(id);
            let obj = match ch.object(rf) {
                Some(o) => Some(o),
                None => self
                    .standard
                    .iter()
                    .find(|s| s.0 == id)
                    .map(|s| doc.add(standard_font_dict(&s.1))),
            };
            if let Some(o) = obj {
                res.set(name_of(rf).as_bytes(), o);
            }
        }
        for (fid, o) in &ch.fallbacks {
            res.set(name_of(RunFont::Fallback(*fid)).as_bytes(), *o);
        }
        doc.set(self.resources, Object::Dict(res));
        self.missing.sort_unstable();
        self.missing.dedup();
        self.missing
    }
}
