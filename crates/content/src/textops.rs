//! Text objects, text state and text showing (§9.3–9.4): glyph runs with positions.

use std::sync::Arc;

use pagebara_syntax::{Name, Object};

use crate::display::{Cmd, FontRef, Glyph, Id, NONE, TextMode, TextRun};
use crate::font::Font;
use crate::geom::Matrix;
use crate::interp::{Builder, Scope};
use crate::lex::Operand;
use crate::ops::Op;
use crate::parse::Instr;

impl Builder<'_, '_> {
    pub(crate) fn text_op(&mut self, i: &Instr<'_, '_>, sc: &mut Scope<'_>) {
        let ts = &mut self.gs.text;
        match i.op {
            Op::BeginText => {
                self.tm = Matrix::IDENTITY;
                self.tlm = Matrix::IDENTITY;
                self.text_runs_start = Some(self.list.runs.len() as u32);
            }
            Op::EndText => self.end_text(),
            Op::CharSpacing => ts.char_spacing = i.number().unwrap_or(ts.char_spacing),
            Op::WordSpacing => ts.word_spacing = i.number().unwrap_or(ts.word_spacing),
            Op::HorizontalScale => {
                ts.hscale = i.number().map_or(ts.hscale, |v| v / 100.0);
            }
            Op::Leading => ts.leading = i.number().unwrap_or(ts.leading),
            Op::TextRender => {
                if let Some(v) = i.number() {
                    ts.mode = TextMode::from_int(v as i64);
                }
            }
            Op::TextRise => ts.rise = i.number().unwrap_or(ts.rise),
            Op::Font => {
                if let [.., name, size] = i.args
                    && let (Some(n), Some(size)) = (name.name(), size.number())
                {
                    self.gs.text.size = size;
                    self.gs.text.font = self.font(&n, sc);
                }
            }
            Op::MoveText => {
                if let Some([x, y]) = i.numbers() {
                    self.next_line(x, y);
                }
            }
            Op::MoveTextLeading => {
                if let Some([x, y]) = i.numbers() {
                    ts.leading = -y;
                    self.next_line(x, y);
                }
            }
            Op::TextMatrix => {
                if let Some(m) = i.numbers::<6>() {
                    let m = Matrix::from_array(m);
                    if m.is_finite() {
                        self.tm = m;
                        self.tlm = m;
                    }
                }
            }
            Op::NextLine => {
                let l = ts.leading;
                self.next_line(0.0, -l);
            }
            Op::ShowText => {
                if let Some(s) = i.args.last() {
                    self.show(std::slice::from_ref(s));
                }
            }
            Op::ShowTextArray => {
                if let Some(a) = i.args.last() {
                    self.show(i.array(a));
                }
            }
            Op::NextLineShow => {
                let l = ts.leading;
                self.next_line(0.0, -l);
                if let Some(s) = i.args.last() {
                    self.show(std::slice::from_ref(s));
                }
            }
            Op::NextLineSpacingShow => {
                if let [.., aw, ac, s] = i.args {
                    ts.word_spacing = aw.number().unwrap_or(ts.word_spacing);
                    ts.char_spacing = ac.number().unwrap_or(ts.char_spacing);
                    let l = ts.leading;
                    self.next_line(0.0, -l);
                    self.show(std::slice::from_ref(s));
                }
            }
            _ => {}
        }
    }

    fn next_line(&mut self, x: f32, y: f32) {
        let m = Matrix::translate(x, y).then(&self.tlm);
        if m.is_finite() {
            self.tlm = m;
            self.tm = m;
        }
    }

    fn end_text(&mut self) {
        let Some(start) = self.text_runs_start.take() else {
            return;
        };
        let end = self.list.runs.len() as u32;
        if self.list.runs[start as usize..]
            .iter()
            .any(|r| r.mode.clips())
        {
            self.list.cmds.push(Cmd::ClipText { start, end });
            self.gs.clips += 1;
        }
    }

    /// Shows strings and position adjustments, emitting one run.
    fn show(&mut self, items: &[Operand<'_>]) {
        let ts = self.gs.text;
        let Some(fr) = self.list.fonts.get(ts.font as usize) else {
            return;
        };
        let font = Arc::clone(&fr.font);
        let (size, h) = (ts.size, ts.hscale);
        let vertical = font.vertical;
        let start = self.list.glyphs.len();
        let (mut tx, mut ty) = (0.0f32, 0.0f32);
        let mut buf = std::mem::take(&mut self.buf);
        let glyphs = &mut self.list.glyphs;
        for item in items {
            if let Some(n) = item.number() {
                let adj = n / 1000.0 * size;
                if vertical {
                    ty -= adj;
                } else {
                    tx -= adj * h;
                }
                continue;
            }
            buf.clear();
            if !item.string_into(&mut buf) {
                continue;
            }
            let mut s = &buf[..];
            while !s.is_empty() {
                let (code, len) = font.next_code(s);
                s = &s[len..];
                let w0 = font.width(code);
                let spacing = ts.char_spacing
                    + if len == 1 && code == 32 {
                        ts.word_spacing
                    } else {
                        0.0
                    };
                if vertical {
                    let (w1, v) = font.vertical_metrics(code);
                    glyphs.push(Glyph {
                        code,
                        x: -v.x * size * h,
                        y: ty - v.y * size,
                        width: w0 * size * h,
                    });
                    ty += w1 * size + spacing;
                } else {
                    glyphs.push(Glyph {
                        code,
                        x: tx,
                        y: 0.0,
                        width: w0 * size * h,
                    });
                    tx += (w0 * size + spacing) * h;
                }
            }
        }
        self.buf = buf;
        let matrix = self.tm.then(&self.gs.ctm);
        let next = Matrix::translate(tx, ty).then(&self.tm);
        if next.is_finite() {
            self.tm = next;
        }
        let end = self.list.glyphs.len();
        if end == start {
            return;
        }
        if !self.visible() {
            self.list.glyphs.truncate(start);
            return;
        }
        let mode = ts.mode;
        let fill = if mode.fills() {
            self.fill_paint()
        } else {
            NONE
        };
        let (stroke, style) = if mode.strokes() {
            (self.stroke_paint(), self.style_id())
        } else {
            (NONE, NONE)
        };
        let ctm = self.ctm_id();
        self.list.runs.push(TextRun {
            font: ts.font,
            size,
            hscale: h,
            rise: ts.rise,
            matrix,
            ctm,
            mode,
            fill,
            stroke,
            style,
            glyphs: (start as u32, end as u32),
            vertical,
        });
        let run = self.list.runs.len() as Id - 1;
        self.list.cmds.push(Cmd::Text { run });
    }

    /// The font a `Tf` name selects, loaded once per document and listed once per list.
    fn font(&mut self, name: &[u8], sc: &mut Scope<'_>) -> Id {
        if let Some((_, id)) = sc.fonts.iter().find(|(n, _)| **n == *name) {
            return *id;
        }
        let id = match self.resource(sc.res, b"Font", name) {
            Some(raw) => self.load_font(name, &raw),
            None => NONE,
        };
        sc.fonts.push((name.into(), id));
        id
    }

    pub(crate) fn load_font(&mut self, name: &[u8], raw: &Object) -> Id {
        let doc = self.doc;
        let font = match raw {
            Object::Ref(r) => {
                if let Some(id) = self.font_ids.get(r) {
                    return *id;
                }
                if let Some(f) = self.it.fonts.get(r) {
                    Arc::clone(f)
                } else {
                    let o = doc.resolve(raw);
                    let Some(d) = o.as_dict() else { return NONE };
                    let f = Arc::new(Font::load_with(doc, d, self.it.programs.as_deref()));
                    self.it.fonts.insert(*r, Arc::clone(&f));
                    f
                }
            }
            Object::Dict(d) => Arc::new(Font::load_with(doc, d, self.it.programs.as_deref())),

            _ => return NONE,
        };
        self.list.fonts.push(FontRef {
            name: Name::new(name),
            object: raw.clone(),
            font,
        });
        let id = self.list.fonts.len() as Id - 1;
        if let Object::Ref(r) = raw {
            self.font_ids.insert(*r, id);
        }
        id
    }
}
