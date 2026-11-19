//! Appearance streams for widgets (ISO 32000-2 §12.7.4.3 and §12.5.6.19): text, combo and
//! list boxes in the field's default appearance, check marks in ZapfDingbats, push button
//! captions, all on the `/MK` background and border.

use std::sync::Arc;

use pagebara_annot::{Color, Glyph, Helvetica, Rect, TextFont};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::Result;
use crate::content::Content;
use crate::da::DefaultAppearance;
use crate::fonts::{self, SharedFont};
use crate::form::{Change, Form, acroform, update_acroform};
use crate::model::{BorderKind, Field, FieldKind, FieldValue, Widget};

/// Line height as a multiple of the font size.
const LEADING: f64 = 1.15;
/// Acrobat's selection colour in list boxes.
const HIGHLIGHT: Color = Color::Rgb(0.6, 0.757, 0.855);
/// Gap between the border and text.
const INDENT: f64 = 2.0;
const DEFAULT_DA: &[u8] = b"/Helv 0 Tf 0 g";

type Run = Vec<(char, Glyph)>;

fn run_width(r: &Run) -> f64 {
    r.iter().map(|(_, g)| g.width).sum()
}

fn run_bytes(r: &[(char, Glyph)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (_, g) in r {
        let be = g.code.to_be_bytes();
        out.extend_from_slice(&be[4 - usize::from(g.len.clamp(1, 4))..]);
    }
    out
}

/// Shows `r`, switching faces for fonts made of several; leaves face 0 selected.
fn show_run(c: &mut Content, font: &dyn TextFont, size: f64, r: &[(char, Glyph)]) {
    let mut face = 0;
    for part in r.chunk_by(|a, b| a.1.face == b.1.face) {
        let f = part[0].1.face;
        if f != face {
            c.name(&font.face_name(f)).n(size).op("Tf");
            face = f;
        }
        c.show(&run_bytes(part));
    }
    if face != 0 {
        c.name(&font.face_name(0)).n(size).op("Tf");
    }
}

/// Lays out text, collecting what the font lacks.
struct Shaper<'a> {
    font: &'a dyn TextFont,
    missing: Vec<char>,
    used: Vec<(char, Glyph)>,
}

impl<'a> Shaper<'a> {
    fn new(font: &'a dyn TextFont) -> Self {
        Shaper {
            font,
            missing: Vec::new(),
            used: Vec::new(),
        }
    }

    fn run(&mut self, s: &str) -> Run {
        s.chars()
            .filter(|c| !c.is_control())
            .map(|c| {
                let g = self.font.glyph(c).unwrap_or_else(|| {
                    if !self.missing.contains(&c) {
                        self.missing.push(c);
                    }
                    self.font.fallback()
                });
                if !self.used.iter().any(|(u, _)| *u == c) {
                    self.used.push((c, g));
                }
                (c, g)
            })
            .collect()
    }

    /// Breaks at spaces where possible, inside words that alone are too wide, and at
    /// every newline.
    fn wrap(&mut self, text: &str, size: f64, max: f64) -> Vec<Run> {
        let mut lines = Vec::new();
        let fits = |l: &Run| run_width(l) * size <= max;
        let space = self.run(" ");
        for para in text.split('\n') {
            let mut line: Run = Vec::new();
            for word in para.split(' ') {
                let word = self.run(word);
                let mut candidate = line.clone();
                if !candidate.is_empty() {
                    candidate.extend_from_slice(&space);
                }
                candidate.extend_from_slice(&word);
                if fits(&candidate) || (line.is_empty() && word.is_empty()) {
                    line = candidate;
                    continue;
                }
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                for g in word {
                    line.push(g);
                    if !fits(&line) && line.len() > 1 {
                        line.pop();
                        lines.push(std::mem::replace(&mut line, vec![g]));
                    }
                }
            }
            lines.push(line);
        }
        lines
    }
}

/// The form XObject's size and, for `/MK /R`, the matrix turning it.
struct Frame {
    w: f64,
    h: f64,
    matrix: Option<[f64; 6]>,
}

fn frame(rect: Rect, rotation: u16) -> Frame {
    let (rw, rh) = (rect.width().max(1.0), rect.height().max(1.0));
    let (w, h) = if rotation % 180 == 90 {
        (rh, rw)
    } else {
        (rw, rh)
    };
    let matrix = match rotation {
        90 => Some([0.0, 1.0, -1.0, 0.0, h, 0.0]),
        180 => Some([-1.0, 0.0, 0.0, -1.0, w, h]),
        270 => Some([0.0, -1.0, 1.0, 0.0, 0.0, w]),
        _ => None,
    };
    Frame { w, h, matrix }
}

/// Paints the background and border; returns the inner rectangle for contents.
fn paint_box(c: &mut Content, w: &Widget, fr: &Frame, round: bool) -> Rect {
    let full = Rect::new(0.0, 0.0, fr.w, fr.h);
    if let Some(bg) = w.mk.background {
        c.fill_color(bg);
        if round {
            c.ellipse(full);
        } else {
            c.rect(full);
        }
        c.fill();
    }
    let bw = if w.mk.border.is_some() {
        w.border.width
    } else {
        0.0
    };
    if bw <= 0.0 {
        return full.inset(1.0);
    }
    let bc = w.mk.border.unwrap_or(Color::BLACK);
    let kind = w.border.kind;
    if matches!(kind, BorderKind::Beveled | BorderKind::Inset) && !round {
        let (light, dark) = if kind == BorderKind::Beveled {
            let dark = w.mk.background.map_or(Color::Gray(0.75), |b| {
                let [r, g, b] = b.to_rgb();
                Color::Rgb(r / 2.0, g / 2.0, b / 2.0)
            });
            (Color::Gray(1.0), dark)
        } else {
            (Color::Gray(0.5), Color::Gray(0.75))
        };
        let (x0, y0, x1, y1) = (bw, bw, fr.w - bw, fr.h - bw);
        let (i0, j0, i1, j1) = (2.0 * bw, 2.0 * bw, fr.w - 2.0 * bw, fr.h - 2.0 * bw);
        c.fill_color(light)
            .move_to(x0, y0)
            .line_to(x0, y1)
            .line_to(x1, y1)
            .line_to(i1, j1)
            .line_to(i0, j1)
            .line_to(i0, j0)
            .op("h")
            .fill();
        c.fill_color(dark)
            .move_to(x1, y1)
            .line_to(x1, y0)
            .line_to(x0, y0)
            .line_to(i0, j0)
            .line_to(i1, j0)
            .line_to(i1, j1)
            .op("h")
            .fill();
    }
    c.stroke_color(bc).line_width(bw);
    if kind == BorderKind::Dashed {
        c.dash(&w.border.dash);
    }
    if kind == BorderKind::Underline {
        c.move_to(0.0, bw / 2.0).line_to(fr.w, bw / 2.0).stroke();
    } else if round {
        c.ellipse(full.inset(bw / 2.0)).stroke();
    } else {
        c.rect(full.inset(bw / 2.0)).stroke();
    }
    if kind == BorderKind::Dashed {
        c.dash(&[]);
    }
    let pad = match kind {
        BorderKind::Beveled | BorderKind::Inset => 2.0 * bw,
        _ => bw,
    };
    full.inset(pad)
}

/// What a widget shows as text.
enum Body {
    Line(String),
    Lines(String),
    Comb(String, usize),
    List {
        items: Vec<String>,
        selected: Vec<usize>,
        top: usize,
    },
    Caption(String),
}

/// The vertical offset of a baseline that centres capitals in a band of height `h`.
fn centred_baseline(y0: f64, h: f64, size: f64) -> f64 {
    y0 + (h - size * 0.7) / 2.0
}

impl Form {
    /// Builds appearance streams for every widget (check boxes and radio buttons only
    /// where missing unless `force`), then clears `/NeedAppearances`.
    pub fn generate_appearances(&mut self, doc: &mut Document, force: bool) -> Result<Change> {
        self.check_editable(doc)?;
        let mut change = Change::default();
        for i in 0..self.fields.len() {
            change.merge(self.appearance_for(doc, i, force)?);
        }
        if acroform(doc).is_some_and(|af| af.contains(b"NeedAppearances")) {
            update_acroform(doc, |_, af| af.set(b"NeedAppearances", false));
        }
        self.need_appearances = false;
        Ok(change)
    }

    /// The font for `text` in widget DA font `name`: the form's own font when it can show
    /// the text, else the caller's font, else Helvetica.
    fn font_for(&self, doc: &Document, dr: &Dict, name: Option<&str>, text: &str) -> SharedFont {
        let own: Option<SharedFont> = name
            .and_then(|n| fonts::from_dr(doc, dr, n))
            .map(|f| Arc::new(f) as SharedFont);
        let covers = |f: &SharedFont| text.chars().all(|c| c.is_control() || f.glyph(c).is_some());
        if let Some(f) = &own
            && covers(f)
        {
            return f.clone();
        }
        if let Some(f) = &self.font {
            return f.clone();
        }
        if covers(&(Arc::new(Helvetica) as SharedFont)) {
            return Arc::new(Helvetica);
        }
        own.unwrap_or_else(|| Arc::new(Helvetica))
    }

    pub(crate) fn appearance_for(
        &mut self,
        doc: &mut Document,
        i: usize,
        force: bool,
    ) -> Result<Change> {
        let f = self.fields[i].clone();
        let dr = acroform(doc)
            .and_then(|af| doc.get_in(&af, b"DR").as_dict().cloned())
            .unwrap_or_default();
        let mut change = Change::default();
        let signed = doc
            .get(f.id)
            .and_then(|o| o.as_dict().map(|d| d.contains(b"V")))
            .unwrap_or(false);
        for w in &f.widgets {
            let made = match f.kind {
                FieldKind::Text | FieldKind::ComboBox | FieldKind::ListBox => {
                    self.text_widget(doc, &dr, &f, w, &mut change.missing)
                }
                FieldKind::PushButton
                    if !w.has_appearance || (force && (!w.mk.has_icon || w.mk.icon.is_some())) =>
                {
                    self.text_widget(doc, &dr, &f, w, &mut change.missing)
                }
                FieldKind::Signature if !signed && (!w.has_appearance || force) => {
                    self.box_widget(doc, w)
                }
                FieldKind::CheckBox | FieldKind::Radio if !w.has_appearance || force => {
                    self.button_widget(doc, &f, w)
                }
                _ => false,
            };
            if made {
                change.widgets.push(w.id);
            }
        }
        if !change.widgets.is_empty() {
            self.reload_field(doc, i);
        }
        Ok(change)
    }

    fn body(&self, f: &Field) -> (Body, Option<Color>) {
        let shown = self.display_text(f);
        let body = match f.kind {
            FieldKind::Text if f.is_password() => {
                Body::Line("•".repeat(shown.text.chars().count()))
            }
            FieldKind::Text if f.is_comb() => Body::Comb(shown.text, f.max_len.unwrap_or(1)),
            FieldKind::Text if f.is_multiline() => Body::Lines(shown.text),
            FieldKind::ListBox => Body::List {
                items: f.options.iter().map(|o| o.display.clone()).collect(),
                selected: f.selection(),
                top: f.top_index,
            },
            FieldKind::PushButton => Body::Caption(
                f.widgets
                    .first()
                    .and_then(|w| w.mk.caption.clone())
                    .unwrap_or_default(),
            ),
            _ => Body::Line(shown.text),
        };
        (body, shown.color)
    }

    fn text_widget(
        &mut self,
        doc: &mut Document,
        dr: &Dict,
        f: &Field,
        w: &Widget,
        missing: &mut Vec<char>,
    ) -> bool {
        let da_src = w.da.as_deref().or(f.da.as_deref()).map(str::as_bytes);
        let mut da = DefaultAppearance::parse(da_src.unwrap_or(DEFAULT_DA));
        let (mut body, color) = self.body(f);
        if let Body::Caption(_) = body {
            body = Body::Caption(w.mk.caption.clone().unwrap_or_default());
        }
        if let Some(c) = color {
            da.color = c;
        }
        let all_text = match &body {
            Body::Line(s) | Body::Lines(s) | Body::Comb(s, _) | Body::Caption(s) => s.clone(),
            Body::List { items, .. } => items.join(""),
        };
        let font = self.font_for(doc, dr, da.font.as_deref(), &all_text);
        if let Body::Line(s) = &body
            && f.is_password()
            && font.glyph('•').is_none()
        {
            body = Body::Line("*".repeat(s.chars().count()));
        }
        let fr = frame(w.rect, w.mk.rotation);
        let mut c = Content::default();
        let inner = paint_box(&mut c, w, &fr, false);
        let mut sh = Shaper::new(&*font);
        let mut xobjects = Dict::new();
        let q = w.quadding.unwrap_or(f.quadding);
        let fname = font.resource_name().to_owned();
        if f.kind != FieldKind::PushButton {
            c.op("/Tx BMC");
        }
        c.save().rect(inner).clip();
        let x_for = |tw: f64| match q {
            1 => (fr.w - tw) / 2.0,
            2 => inner.x1 - INDENT - tw,
            _ => inner.x0 + INDENT,
        };
        match &body {
            Body::Line(s) => {
                let run = sh.run(s);
                let tw = run_width(&run);
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    let by_h = (inner.height() / LEADING).max(1.0);
                    let by_w = if tw > 0.0 {
                        (inner.width() - 2.0 * INDENT) / tw
                    } else {
                        by_h
                    };
                    by_h.min(by_w).clamp(4.0_f64.min(by_h), 1000.0)
                };
                if !run.is_empty() {
                    c.fill_color(da.color).begin_text(&fname, size);
                    c.td(
                        x_for(tw * size),
                        centred_baseline(inner.y0, inner.height(), size),
                    );
                    show_run(&mut c, &*font, size, &run);
                    c.end_text();
                }
            }
            Body::Caption(s) => {
                let run = sh.run(s);
                let tw = run_width(&run);
                let icon =
                    w.mk.icon
                        .filter(|_| w.mk.text_position != 0)
                        .and_then(|i| Some((i, icon_bbox(doc, i)?)));
                let tp = if icon.is_some() {
                    w.mk.text_position
                } else {
                    0
                };
                let caption = tp != 1 && !run.is_empty();
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    let room = if matches!(tp, 2 | 3) { 0.4 } else { 1.0 };
                    let by_h = (inner.height() * room / LEADING).min(12.0);
                    let by_w = if tw > 0.0 {
                        (inner.width() - 2.0 * INDENT) / tw
                    } else {
                        by_h
                    };
                    by_h.min(by_w).max(1.0)
                };
                let band = size * LEADING;
                let cw = tw * size + 2.0 * INDENT;
                let (cap, ico) = match tp {
                    _ if !caption => (inner, inner),
                    2 => (
                        Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + band),
                        Rect::new(inner.x0, inner.y0 + band, inner.x1, inner.y1),
                    ),
                    3 => (
                        Rect::new(inner.x0, inner.y1 - band, inner.x1, inner.y1),
                        Rect::new(inner.x0, inner.y0, inner.x1, inner.y1 - band),
                    ),
                    4 => (
                        Rect::new(inner.x1 - cw, inner.y0, inner.x1, inner.y1),
                        Rect::new(inner.x0, inner.y0, inner.x1 - cw, inner.y1),
                    ),
                    5 => (
                        Rect::new(inner.x0, inner.y0, inner.x0 + cw, inner.y1),
                        Rect::new(inner.x0 + cw, inner.y0, inner.x1, inner.y1),
                    ),
                    _ => (inner, inner),
                };
                if let Some((i, bb)) = icon {
                    let sc = (ico.width() / bb.width().max(1e-3))
                        .min(ico.height() / bb.height().max(1e-3))
                        .max(0.0);
                    let tx = ico.x0 + (ico.width() - bb.width() * sc) / 2.0 - bb.x0 * sc;
                    let ty = ico.y0 + (ico.height() - bb.height() * sc) / 2.0 - bb.y0 * sc;
                    c.save()
                        .cm([sc, 0.0, 0.0, sc, tx, ty])
                        .do_xobject("I0")
                        .restore();
                    xobjects.set(b"I0", i);
                }
                if caption {
                    c.fill_color(da.color).begin_text(&fname, size);
                    c.td(
                        cap.x0 + (cap.width() - tw * size) / 2.0,
                        centred_baseline(cap.y0, cap.height(), size),
                    );
                    show_run(&mut c, &*font, size, &run);
                    c.end_text();
                }
            }
            Body::Lines(s) => {
                let max = inner.width() - 2.0 * INDENT;
                let fits = |sh: &mut Shaper, size: f64| {
                    sh.wrap(s, size, max).len() as f64 * size * LEADING <= inner.height() - 2.0
                };
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    let mut size = 12.0;
                    while size > 4.0 && !fits(&mut sh, size) {
                        size -= 0.5;
                    }
                    size
                };
                let lines = sh.wrap(s, size, max);
                let ascent = font.ascent().clamp(0.6, 1.0);
                let mut y = inner.y1 - 1.0 - ascent * size;
                let (mut px, mut py) = (0.0, 0.0);
                c.fill_color(da.color).begin_text(&fname, size);
                for line in &lines {
                    if y < inner.y0 - size {
                        break;
                    }
                    if !line.is_empty() {
                        let x = x_for(run_width(line) * size);
                        c.td(x - px, y - py);
                        show_run(&mut c, &*font, size, line);
                        (px, py) = (x, y);
                    }
                    y -= size * LEADING;
                }
                c.end_text();
            }
            Body::Comb(s, cells) => {
                let cells = (*cells).max(1);
                let cw = fr.w / cells as f64;
                let run: Run = sh.run(s).into_iter().take(cells).collect();
                let widest = run.iter().map(|(_, g)| g.width).fold(0.0, f64::max);
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    let by_h = inner.height() / LEADING;
                    let by_w = if widest > 0.0 {
                        cw * 0.9 / widest
                    } else {
                        by_h
                    };
                    by_h.min(by_w).max(1.0)
                };
                if let Some(bc) = w.mk.border
                    && w.border.width > 0.0
                {
                    c.stroke_color(bc).line_width(w.border.width);
                    for k in 1..cells {
                        let x = cw * k as f64;
                        c.move_to(x, 0.0).line_to(x, fr.h).stroke();
                    }
                }
                let y = centred_baseline(inner.y0, inner.height(), size);
                let (mut px, mut py) = (0.0, 0.0);
                c.fill_color(da.color).begin_text(&fname, size);
                for (k, g) in run.iter().enumerate() {
                    let x = cw * k as f64 + (cw - g.1.width * size) / 2.0;
                    c.td(x - px, y - py);
                    show_run(&mut c, &*font, size, std::slice::from_ref(g));
                    (px, py) = (x, y);
                }
                c.end_text();
            }
            Body::List {
                items,
                selected,
                top,
            } => {
                let size = if da.size > 0.0 { da.size } else { 12.0 };
                let lh = size * LEADING;
                let top = (*top).min(items.len().saturating_sub(1));
                let mut y1 = inner.y1;
                for (k, item) in items.iter().enumerate().skip(top) {
                    if y1 <= inner.y0 {
                        break;
                    }
                    let y0 = y1 - lh;
                    if selected.contains(&k) {
                        c.fill_color(HIGHLIGHT)
                            .rect(Rect::new(inner.x0, y0, inner.x1, y1))
                            .fill();
                    }
                    let run = sh.run(item);
                    if !run.is_empty() {
                        c.fill_color(da.color).begin_text(&fname, size);
                        c.td(inner.x0 + INDENT, centred_baseline(y0, lh, size));
                        show_run(&mut c, &*font, size, &run);
                        c.end_text();
                    }
                    y1 = y0;
                }
            }
        }
        c.restore();
        if f.kind != FieldKind::PushButton {
            c.op("EMC");
        }
        for ch in std::mem::take(&mut sh.missing) {
            if !missing.contains(&ch) {
                missing.push(ch);
            }
        }
        let used = std::mem::take(&mut sh.used);
        let mut fonts = Dict::new();
        for (name, obj) in font.resources(doc, &used) {
            fonts.set(name.as_bytes(), obj);
        }
        let stream = form_stream(&fr, fonts, xobjects, &c.0);
        let existing = normal_ref(doc, w.id);
        let r = self.store(doc, existing, stream);
        set_ap(doc, w.id, Object::Ref(r), None);
        true
    }

    fn button_widget(&mut self, doc: &mut Document, f: &Field, w: &Widget) -> bool {
        let da = DefaultAppearance::parse(
            w.da.as_deref()
                .or(f.da.as_deref())
                .map_or(b"/ZaDb 0 Tf 0 g".as_slice(), str::as_bytes),
        );
        let round = f.kind == FieldKind::Radio;
        let mark =
            w.mk.caption
                .as_deref()
                .and_then(|c| c.bytes().next())
                .unwrap_or(if round { b'l' } else { b'4' });
        let fr = frame(w.rect, w.mk.rotation);
        let on_state = w.on_state.clone().unwrap_or_else(|| "Yes".to_owned());
        let mut streams = Vec::new();
        for on in [true, false] {
            let mut c = Content::default();
            let inner = paint_box(&mut c, w, &fr, round);
            if on {
                let gw = fonts::dingbat_width(mark);
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    (inner.width() / gw).min(inner.height() / 0.8) * 0.8
                };
                c.save().rect(inner).clip();
                c.fill_color(da.color).begin_text("ZaDb", size);
                c.td((fr.w - gw * size) / 2.0, centred_baseline(0.0, fr.h, size));
                c.show(&[mark]).end_text().restore();
            }
            let mut fonts = Dict::new();
            fonts.set(b"ZaDb", fonts::dingbats());
            let name = if on { on_state.clone() } else { "Off".into() };
            streams.push((name, form_stream(&fr, fonts, Dict::new(), &c.0)));
        }
        let existing = state_refs(doc, w.id);
        let mut n = Dict::new();
        for (name, s) in streams {
            let old = existing.iter().find(|(k, _)| *k == name).map(|(_, r)| *r);
            let r = self.store(doc, old, s);
            n.set(name.as_bytes(), r);
        }
        let on = match w.state.as_deref() {
            Some(s) => s == on_state,
            None => matches!(&f.value, FieldValue::State(s) if *s == on_state),
        };
        let state = if on { on_state } else { "Off".to_owned() };
        set_ap(doc, w.id, Object::Dict(n), Some(&state));
        true
    }

    /// An unsigned signature field: its background and border.
    fn box_widget(&mut self, doc: &mut Document, w: &Widget) -> bool {
        let fr = frame(w.rect, w.mk.rotation);
        let mut c = Content::default();
        paint_box(&mut c, w, &fr, false);
        let stream = form_stream(&fr, Dict::new(), Dict::new(), &c.0);
        let existing = normal_ref(doc, w.id);
        let r = self.store(doc, existing, stream);
        set_ap(doc, w.id, Object::Ref(r), None);
        true
    }

    /// Writes `s` over a stream we made before, else as a new object.
    fn store(&mut self, doc: &mut Document, existing: Option<ObjRef>, s: Stream) -> ObjRef {
        match existing.filter(|r| self.generated.contains(&r.num)) {
            Some(r) => {
                doc.set(r, s);
                r
            }
            None => {
                let r = doc.add(s);
                self.generated.insert(r.num);
                r
            }
        }
    }
}

fn form_stream(fr: &Frame, fonts: Dict, xobjects: Dict, content: &[u8]) -> Stream {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"XObject"));
    d.set(b"Subtype", Object::name(b"Form"));
    d.set(
        b"BBox",
        Object::Array(vec![Object::Int(0), Object::Int(0), num(fr.w), num(fr.h)]),
    );
    if let Some(m) = fr.matrix {
        d.set(
            b"Matrix",
            Object::Array(m.iter().map(|&v| num(v)).collect()),
        );
    }
    let mut res = Dict::new();
    if !fonts.is_empty() {
        res.set(b"Font", fonts);
    }
    if !xobjects.is_empty() {
        res.set(b"XObject", xobjects);
    }
    d.set(b"Resources", res);
    if content.len() > 128 {
        let z = pagebara_annot::zlib(content);
        if z.len() < content.len() {
            d.set(b"Filter", Object::name(b"FlateDecode"));
            return Stream::new(d, z);
        }
    }
    Stream::new(d, content)
}

pub(crate) fn num(v: f64) -> Object {
    let r = (v * 1000.0).round() / 1000.0;
    if r.fract() == 0.0 && r.abs() < 1e15 {
        Object::Int(r as i64)
    } else {
        Object::Real(r)
    }
}

/// The bounding box of an icon's form XObject.
fn icon_bbox(doc: &Document, icon: ObjRef) -> Option<Rect> {
    let o = doc.get(icon)?;
    let s = o.as_stream()?;
    let r = Rect::from_object(&doc.get_in(&s.dict, b"BBox"))?;
    (r.width() > 0.0 && r.height() > 0.0).then_some(r)
}

/// The widget's `/AP /N` when it is one stream.
fn normal_ref(doc: &Document, w: ObjRef) -> Option<ObjRef> {
    let d = doc.get(w)?;
    let ap = doc.get_in(d.as_dict()?, b"AP");
    ap.as_dict()?.get(b"N")?.as_objref()
}

/// The widget's `/AP /N` states.
fn state_refs(doc: &Document, w: ObjRef) -> Vec<(String, ObjRef)> {
    let Some(d) = doc.get(w) else {
        return Vec::new();
    };
    let Some(d) = d.as_dict() else {
        return Vec::new();
    };
    let ap = doc.get_in(d, b"AP");
    let Some(n) = ap.as_dict().map(|ap| doc.get_in(ap, b"N")) else {
        return Vec::new();
    };
    let Some(n) = n.as_dict().filter(|_| n.as_stream().is_none()) else {
        return Vec::new();
    };
    n.iter()
        .filter_map(|(k, v)| Some((crate::text::name_text(k.as_bytes()), v.as_objref()?)))
        .collect()
}

/// Replaces the widget's appearance dictionary with just `/N`, and sets `/AS`.
fn set_ap(doc: &mut Document, w: ObjRef, n: Object, state: Option<&str>) {
    let Some(mut d) = doc.get(w).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let mut ap = Dict::new();
    ap.set(b"N", n);
    d.set(b"AP", ap);
    if let Some(s) = state {
        d.set(b"AS", Object::name(s.as_bytes()));
    }
    doc.set(w, d);
}
