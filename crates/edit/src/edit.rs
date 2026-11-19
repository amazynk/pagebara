//! The editor: operations collected on a parsed page, written as one change.

use std::collections::{BTreeMap, HashMap};

use pagebara_content::font::Font;
use pagebara_content::{Matrix, Point, Rect};
use pagebara_syntax::{Dict, Document, Object};

use crate::assemble::{Parsed, assemble};
use crate::emit;
use crate::encode::{EncodeRequest, FontChoice, FontSlot, ShowItem, TextEncoder};
use crate::fonts::Fonts;
use crate::geom::{corners, in_user_space, intersects, is_identity, relative};
use crate::image::{ImageFit, NewImage};
use crate::model::{Color, ObjectData, PageObjects, Target};
use crate::paragraph::{
    BlockLayout, BlockReport, BlockText, StyledText, TextFrame, TextState, block_text, layout_block,
};
use crate::scan::{Item, ItemData, Scanner, ShowKind, Source};
use crate::write::{Changes, FormEdit, commit};
use crate::{Error, Result};
use pagebara_typeset::RunFont;

/// Text properties to set on whole runs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextStyle {
    pub size: Option<f32>,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    /// `Tr`, 0 to 7.
    pub render_mode: Option<u8>,
}

/// Where in the painting order objects move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZOrder {
    /// Over everything on the page.
    Front,
    /// Under everything.
    Back,
    /// Over the next object it overlaps.
    Forward,
    /// Under the previous object it overlaps.
    Backward,
}

/// New text to add to a page.
#[derive(Clone, Debug)]
pub struct NewText<'a> {
    /// Lines separated by `\n`.
    pub text: &'a str,
    /// The first baseline's start in page space.
    pub origin: Point,
    pub size: f32,
    pub color: Color,
    /// Counterclockwise, in degrees (to stay upright on a rotated page, use the page's
    /// rotation).
    pub angle: f32,
    /// Baseline distance; `None` for 1.2 × size.
    pub leading: Option<f32>,
}

#[derive(Clone, Debug)]
struct NewShow {
    /// The operand and operator.
    ops: Vec<u8>,
    /// A font resource name replacing the run's font.
    font: Option<Vec<u8>>,
    /// The advance in text space along the writing direction.
    advance: f32,
    /// `ops` select other fonts too.
    switches: bool,
}

/// Encoded items in one font: the name to select it with (if it changes), the font.
type Segment = (Option<Vec<u8>>, std::sync::Arc<Font>, Vec<ShowItem>);

#[derive(Clone, Debug, Default)]
struct ItemEdit {
    delete: bool,
    transform: Option<Matrix>,
    fill: Option<Color>,
    stroke: Option<Color>,
    fill_alpha: Option<f32>,
    stroke_alpha: Option<f32>,
    /// Line width and dash pattern in the item's user space.
    line_width: Option<f32>,
    dash: Option<Vec<f32>>,
    /// Crop in the object's original page space.
    crop: Option<[Point; 4]>,
    /// A replacement image's resource name and its matrix in the unit square.
    image: Option<(Vec<u8>, Matrix)>,
    text: Option<NewShow>,
    size: Option<f32>,
    render_mode: Option<u8>,
    copies: Vec<Matrix>,
    /// Operators drawn instead of a run (they leave the text state as it was).
    replace: Option<Vec<u8>>,
}

impl ItemEdit {
    fn visual(&self) -> bool {
        self.transform.is_some()
            || self.fill.is_some()
            || self.stroke.is_some()
            || self.fill_alpha.is_some()
            || self.stroke_alpha.is_some()
            || self.line_width.is_some()
            || self.dash.is_some()
            || self.crop.is_some()
            || self.image.is_some()
            || self.text.is_some()
            || self.size.is_some()
            || self.render_mode.is_some()
            || self.replace.is_some()
    }
}

/// Edits on one page, written by [`Editor::commit`]. Object ids are those of
/// [`Editor::objects`]; they stay valid until the commit.
#[derive(Debug)]
pub struct Editor<'d> {
    doc: &'d mut Document,
    p: Parsed,
    edits: BTreeMap<usize, ItemEdit>,
    ch: Changes,
    prepend: Vec<Vec<u8>>,
    append: Vec<Vec<u8>>,
    moved: bool,
    /// How objects in forms drawn elsewhere are edited.
    pub forms: FormEdit,
}

impl<'d> Editor<'d> {
    pub fn open(doc: &'d mut Document, page: usize) -> Result<Self> {
        if !doc.permissions().modify && !doc.is_owner() {
            return Err(Error::NotPermitted);
        }
        let p = parse_page(doc, page, true)?;
        Ok(Editor {
            doc,
            p,
            edits: BTreeMap::new(),
            ch: Changes::default(),
            prepend: Vec::new(),
            append: Vec::new(),
            moved: false,
            forms: FormEdit::default(),
        })
    }

    pub fn objects(&self) -> &PageObjects {
        &self.p.model
    }

    pub fn document(&self) -> &Document {
        self.doc
    }

    fn items(&self, targets: &[Target]) -> Result<Vec<usize>> {
        let mut out = Vec::new();
        for &t in targets {
            match t {
                Target::Object(o) => out.extend(
                    self.p
                        .obj_items
                        .get(o.0 as usize)
                        .ok_or(Error::NoSuchObject(t))?,
                ),
                Target::Run(r) => out.push(
                    *self
                        .p
                        .run_items
                        .get(r.0 as usize)
                        .ok_or(Error::NoSuchObject(t))?,
                ),
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    fn edit(&mut self, k: usize) -> &mut ItemEdit {
        self.edits.entry(k).or_default()
    }

    /// Removes objects. Text after a removed run keeps its place.
    pub fn delete(&mut self, targets: &[Target]) -> Result<()> {
        for k in self.items(targets)? {
            self.edit(k).delete = true;
            self.text_changed(k, None);
            self.note_unused(k);
        }
        Ok(())
    }

    /// Moves, scales or rotates objects by a page-space matrix (see
    /// [`geom::scale_about`](crate::geom::scale_about) and
    /// [`geom::rotate_about`](crate::geom::rotate_about)).
    pub fn transform(&mut self, targets: &[Target], m: Matrix) -> Result<()> {
        for k in self.items(targets)? {
            let e = self.edit(k);
            e.transform = Some(e.transform.map_or(m, |t| t.then(&m)));
        }
        Ok(())
    }

    pub fn set_fill(&mut self, targets: &[Target], c: Color) -> Result<()> {
        for k in self.items(targets)? {
            self.edit(k).fill = Some(c);
        }
        Ok(())
    }

    pub fn set_stroke(&mut self, targets: &[Target], c: Color) -> Result<()> {
        for k in self.items(targets)? {
            self.edit(k).stroke = Some(c);
        }
        Ok(())
    }

    /// Sets the constant opacity (0 to 1) of filling and stroking.
    pub fn set_opacity(
        &mut self,
        targets: &[Target],
        fill: Option<f32>,
        stroke: Option<f32>,
    ) -> Result<()> {
        for k in self.items(targets)? {
            let e = self.edit(k);
            e.fill_alpha = fill.map(|v| v.clamp(0.0, 1.0)).or(e.fill_alpha);
            e.stroke_alpha = stroke.map(|v| v.clamp(0.0, 1.0)).or(e.stroke_alpha);
        }
        Ok(())
    }

    /// The line width (in page space) and dash pattern (page-space lengths, empty for
    /// solid) of paths.
    pub fn set_line(
        &mut self,
        targets: &[Target],
        width: Option<f32>,
        dash: Option<&[f32]>,
    ) -> Result<()> {
        for k in self.items(targets)? {
            let it = &self.p.scan.items[k];
            if !matches!(it.data, ItemData::Object(ObjectData::Path(_))) {
                continue;
            }
            let s = it.snap.ctm.determinant().abs().sqrt();
            let s = if s > 1e-6 { s } else { 1.0 };
            let e = self.edit(k);
            if let Some(w) = width.filter(|w| w.is_finite() && *w >= 0.0) {
                e.line_width = Some(w / s);
            }
            if let Some(d) = dash {
                e.dash = Some(d.iter().map(|v| v.abs() / s).collect());
            }
        }
        Ok(())
    }

    /// Size, colours and rendering mode of whole runs.
    pub fn set_text_style(&mut self, targets: &[Target], s: TextStyle) -> Result<()> {
        for k in self.items(targets)? {
            if !matches!(self.p.scan.items[k].data, ItemData::Run(_)) {
                continue;
            }
            let e = self.edit(k);
            if let Some(v) = s.size.filter(|v| v.is_finite()) {
                e.size = Some(v);
            }
            e.fill = s.fill.or(e.fill);
            e.stroke = s.stroke.or(e.stroke);
            e.render_mode = s.render_mode.map(|m| m.min(7)).or(e.render_mode);
        }
        Ok(())
    }

    /// Clips images (or any object) to a page-space rectangle, as they are drawn now.
    pub fn crop(&mut self, targets: &[Target], rect: Rect) -> Result<()> {
        for k in self.items(targets)? {
            let e = self.edit(k);
            let q = corners(&rect);
            let q = match e.transform.and_then(|t| t.invert()) {
                Some(inv) => q.map(|p| inv.apply(p)),
                None => q,
            };
            e.crop = Some(q);
        }
        Ok(())
    }

    /// Copies of objects moved by `offset`, drawn right over the originals.
    pub fn duplicate(&mut self, targets: &[Target], offset: Matrix) -> Result<()> {
        for k in self.items(targets)? {
            self.edit(k).copies.push(offset);
        }
        Ok(())
    }

    /// Replaces an image (XObject or inline) keeping its placement.
    pub fn replace_image(
        &mut self,
        target: Target,
        img: &NewImage<'_>,
        how: ImageFit,
    ) -> Result<()> {
        let ks = self.items(&[target])?;
        let (r, w, h) = crate::image::embed(self.doc, img)?;
        for k in ks {
            let it = &self.p.scan.items[k];
            let ItemData::Object(ObjectData::Image(info)) = &it.data else {
                return Err(Error::Unsupported("not an image"));
            };
            let fit = crate::image::fit(&it.snap.ctm, w, h, how);
            let stream = it.stream;
            let old = info.name.clone();
            let name = self
                .ch
                .add_resource(self.doc, &self.p, stream, b"XObject", Object::Ref(r));
            if let Some(n) = old {
                self.ch.unused(stream, b"XObject", n.as_bytes());
            }
            self.edit(k).image = Some((name, fit));
        }
        Ok(())
    }

    /// Replaces the text of a run, encoded by `enc` in the run's font (or one `enc`
    /// picks). The run keeps its start; text after it on its line keeps its place.
    pub fn replace_text(&mut self, run: Target, text: &str, enc: &dyn TextEncoder) -> Result<()> {
        let ks = self.items(&[run])?;
        let &[k] = &ks[..] else {
            return Err(Error::Unsupported("replace the text of one run at a time"));
        };
        let it = &self.p.scan.items[k];
        let ItemData::Run(_) = &it.data else {
            return Err(Error::Unsupported("not a text run"));
        };
        let g = it.snap.clone();
        let stream = it.stream;
        let size = self.edits.get(&k).and_then(|e| e.size).unwrap_or(g.size);
        let slot = g.font.as_ref().map(|f| FontSlot {
            name: f.name.as_ref().map(|n| n.as_bytes()),
            object: &f.object,
            font: &f.font,
        });
        let enc = enc.encode(
            self.doc,
            &EncodeRequest {
                text,
                font: slot,
                size,
            },
        )?;
        let (font_name, font) = match &enc.font {
            FontChoice::Same => (
                None,
                g.font
                    .as_ref()
                    .map(|f| f.font.clone())
                    .ok_or(Error::Unsupported("the run has no font"))?,
            ),
            FontChoice::New(o) => {
                let font = load_font(self.doc, o).ok_or(Error::Unsupported("unreadable font"))?;
                let name = self
                    .ch
                    .add_resource(self.doc, &self.p, stream, b"Font", o.clone());
                (Some(name), font)
            }
        };
        let base_name = match &font_name {
            Some(n) => n.clone(),
            None => match &g.font_raw {
                Some(n) => pagebara_content::lex::name(n).into_owned(),
                None => {
                    let o = g
                        .font
                        .as_ref()
                        .map(|f| f.object.clone())
                        .ok_or(Error::Unsupported("the run has no font"))?;
                    self.ch.add_resource(self.doc, &self.p, stream, b"Font", o)
                }
            },
        };
        let mut ops = Vec::new();
        let (mut tx, mut ty) = (0.0f32, 0.0f32);
        let mut switches = false;
        for (name, f, items) in self.segments(stream, &enc.show, &base_name, &font)? {
            if let Some(n) = name {
                font_op(&mut ops, &n, size);
                switches = true;
            }
            let (x, y) = advance(&f, &items, size, g.tc, g.tw, g.th);
            tx += x;
            ty += y;
            show_ops(&mut ops, &items);
        }
        self.edit(k).text = Some(NewShow {
            ops,
            font: font_name,
            advance: if font.vertical { ty } else { tx },
            switches,
        });
        self.text_changed(k, Some(text));
        Ok(())
    }

    /// Encoded items split where the font changes: the font to select first (none for the
    /// first segment in the base font), the font and the items.
    fn segments(
        &mut self,
        stream: crate::StreamId,
        show: &[ShowItem],
        base_name: &[u8],
        base: &std::sync::Arc<Font>,
    ) -> Result<Vec<Segment>> {
        let mut out: Vec<Segment> = vec![(None, base.clone(), Vec::new())];
        for it in show {
            match it {
                ShowItem::Font(c) => {
                    let (name, f) = match c {
                        FontChoice::Same => (base_name.to_vec(), base.clone()),
                        FontChoice::New(o) => {
                            let f = load_font(self.doc, o)
                                .ok_or(Error::Unsupported("unreadable font"))?;
                            let n =
                                self.ch
                                    .add_resource(self.doc, &self.p, stream, b"Font", o.clone());
                            (n, f)
                        }
                    };
                    out.push((Some(name), f, Vec::new()));
                }
                other => out.last_mut().expect("segment").2.push(other.clone()),
            }
        }
        out.retain(|s| !s.2.is_empty());
        Ok(out)
    }

    /// Replaces the text of line `line` of a text block: the first run gets the text, the
    /// line's other runs go.
    pub fn replace_line(
        &mut self,
        block: crate::ObjectId,
        line: usize,
        text: &str,
        enc: &dyn TextEncoder,
    ) -> Result<()> {
        let runs = self
            .p
            .model
            .get(block)
            .and_then(|o| o.text())
            .and_then(|t| t.lines.get(line))
            .map(|l| l.runs.clone())
            .ok_or(Error::NoSuchObject(Target::Object(block)))?;
        let Some((&first, rest)) = runs.split_first() else {
            return Err(Error::NoSuchObject(Target::Object(block)));
        };
        self.replace_text(Target::Run(first), text, enc)?;
        let rest: Vec<Target> = rest.iter().map(|&r| Target::Run(r)).collect();
        self.delete(&rest)
    }

    /// Notes the resource a deleted item named, to drop it if nothing else uses it.
    fn note_unused(&mut self, k: usize) {
        let it = &self.p.scan.items[k];
        let (cat, name): (&'static [u8], Option<Vec<u8>>) = match &it.data {
            ItemData::Object(ObjectData::Image(i)) => {
                (b"XObject", i.name.as_ref().map(|n| n.as_bytes().to_vec()))
            }
            ItemData::Object(ObjectData::Form(f)) => (b"XObject", Some(f.name.as_bytes().to_vec())),
            ItemData::Object(ObjectData::Shading(s)) => {
                (b"Shading", Some(s.name.as_bytes().to_vec()))
            }
            ItemData::Run(_) => (b"Font", it.snap.font_raw.clone()),
            _ => return,
        };
        if let Some(n) = name {
            let n = pagebara_content::lex::name(&n).into_owned();
            self.ch.unused(it.stream, cat, &n);
        }
    }

    /// `/ActualText` of marked content around a changed run: kept up to date when the
    /// sequence holds only that run, removed otherwise.
    fn text_changed(&mut self, k: usize, text: Option<&str>) {
        self.items_text_changed(&[k], text);
    }

    /// `/ActualText` of marked content around changed items: `text` when the sequence
    /// holds only those items, removed otherwise.
    fn items_text_changed(&mut self, ks: &[usize], text: Option<&str>) {
        let mut frames: Vec<u32> = ks
            .iter()
            .flat_map(|&k| self.p.scan.items[k].frames.clone())
            .collect();
        frames.sort_unstable();
        frames.dedup();
        for f in frames {
            let fr = &self.p.scan.frames[f as usize];
            let Some((s, e)) = fr.actual_text else {
                continue;
            };
            let Some(mut d) = fr.info.properties.clone() else {
                continue;
            };
            match text {
                Some(t) if fr.items.iter().all(|i| ks.contains(i)) => {
                    d.set(b"ActualText", Object::String(utf16(t)));
                }
                _ => {
                    d.remove(b"ActualText");
                }
            }
            let mut b = b"\n".to_vec();
            emit::name(&mut b, fr.info.tag.as_bytes());
            b.push(b' ');
            pagebara_syntax::write_object(&mut b, &Object::Dict(d));
            b.extend_from_slice(b" BDC\n");
            let stream = fr.stream;
            self.ch.patch(stream, s, e, b);
            self.p.scan.frames[f as usize].actual_text = None;
        }
    }

    /// Replaces the text of a text block with styled paragraphs (separated by `\n`), laid
    /// out again in the block's box ([`block_text`](crate::block_text)): same alignment,
    /// indent, leading and width, growing downwards when longer. The new text is drawn
    /// where the block's first run was, inside its marked content; the block's other runs
    /// go. Characters the fonts lack are added to them or shown in fallback fonts.
    pub fn edit_block(
        &mut self,
        block: crate::ObjectId,
        spans: &[StyledText],
        fonts: &mut Fonts,
    ) -> Result<BlockReport> {
        let bt = block_text(&self.p.model, block)?;
        self.write_block(&bt, spans, fonts)
    }

    /// [`Editor::edit_block`] with plain text: unchanged text keeps its styles, inserted
    /// text takes the style of the text before it.
    pub fn edit_block_text(
        &mut self,
        block: crate::ObjectId,
        text: &str,
        fonts: &mut Fonts,
    ) -> Result<BlockReport> {
        let bt = block_text(&self.p.model, block)?;
        let spans = crate::rich::restyle_plain(&bt.spans, text);
        self.write_block(&bt, &spans, fonts)
    }

    fn write_block(
        &mut self,
        bt: &BlockText,
        spans: &[StyledText],
        fonts: &mut Fonts,
    ) -> Result<BlockReport> {
        fonts.sync(self.doc);
        let lay = layout_block(self.doc, fonts, &bt.frame, spans)?;
        if !lay.missing.is_empty() {
            return Err(Error::Encode(crate::EncodeError::Missing(
                lay.missing.iter().map(|m| m.1).collect(),
            )));
        }
        let first = bt
            .runs
            .iter()
            .copied()
            .find(|r| {
                self.p
                    .model
                    .run(*r)
                    .is_some_and(|x| x.font.is_some() && !x.glyphs.is_empty())
            })
            .ok_or(Error::NoSuchObject(Target::Object(bt.block)))?;
        let k = self.p.run_items[first.0 as usize];
        let ks: Vec<usize> = bt
            .runs
            .iter()
            .map(|r| self.p.run_items[r.0 as usize])
            .collect();
        if ks.iter().any(|k| self.edits.contains_key(k)) {
            return Err(Error::Conflict);
        }
        fonts.finish(self.doc);
        let it = &self.p.scan.items[k];
        let stream = it.stream;
        let g = it.snap.clone();
        let mut names: HashMap<RunFont, Vec<u8>> = HashMap::new();
        for gl in lay.lines.iter().flat_map(|l| &l.glyphs) {
            if names.contains_key(&gl.font) {
                continue;
            }
            let o = fonts
                .object(gl.font)
                .ok_or(Error::Unsupported("a font was not written"))?;
            let n = self.ch.add_resource(self.doc, &self.p, stream, b"Font", o);
            names.insert(gl.font, n);
        }
        let mut font_op_bytes = Vec::new();
        match (&g.font_raw, &g.font) {
            (Some(n), _) => font_op(&mut font_op_bytes, &pagebara_content::lex::name(n), g.size),
            (None, Some(f)) => {
                let n = self
                    .ch
                    .add_resource(self.doc, &self.p, stream, b"Font", f.object.clone());
                font_op(&mut font_op_bytes, &n, g.size);
            }
            _ => {}
        }
        let from = TextState {
            tc: g.tc,
            tw: g.tw,
            th: g.th,
            rise: g.rise,
            mode: g.mode,
            line_width: g.line_width,
            font_op: font_op_bytes,
            fill_ops: g.fill_ops.clone(),
            stroke_ops: g.stroke_ops.clone(),
        };
        let ops = lay.write_ops(&g.ctm, &from, &|f| {
            names.get(&f).cloned().unwrap_or_default()
        })?;
        self.edit(k).replace = Some(ops);
        for &kk in &ks {
            if kk != k {
                self.edit(kk).delete = true;
            }
            self.note_unused(kk);
        }
        let plain = lay.text.replace('\n', " ");
        self.items_text_changed(&ks, Some(&plain));
        let old_text = bt.text();
        let mcids: Vec<i64> = ks
            .iter()
            .flat_map(|&k| self.p.scan.items[k].frames.clone())
            .filter_map(|f| {
                let fr = &self.p.scan.frames[f as usize];
                (fr.stream.0 == 0).then_some(fr.info.mcid).flatten()
            })
            .collect();
        crate::tagged::update_text(self.doc, self.p.model.page_ref, &mcids, &old_text, &plain);
        Ok(self.report(&lay, &bt.blocks))
    }

    /// Adds a text box: styled paragraphs laid out in `frame` (see
    /// [`TextFrame::new_box`]), over the page's content.
    pub fn add_text_box(
        &mut self,
        frame: &TextFrame,
        spans: &[StyledText],
        fonts: &mut Fonts,
    ) -> Result<BlockReport> {
        fonts.sync(self.doc);
        let lay = layout_block(self.doc, fonts, frame, spans)?;
        if !lay.missing.is_empty() {
            return Err(Error::Encode(crate::EncodeError::Missing(
                lay.missing.iter().map(|m| m.1).collect(),
            )));
        }
        if lay.text.trim().is_empty() {
            return Err(Error::Unsupported("no text"));
        }
        fonts.finish(self.doc);
        let stream = crate::StreamId(0);
        let mut names: HashMap<RunFont, Vec<u8>> = HashMap::new();
        for gl in lay.lines.iter().flat_map(|l| &l.glyphs) {
            if names.contains_key(&gl.font) {
                continue;
            }
            let o = fonts
                .object(gl.font)
                .ok_or(Error::Unsupported("a font was not written"))?;
            let n = self.ch.add_resource(self.doc, &self.p, stream, b"Font", o);
            names.insert(gl.font, n);
        }
        let mut b = b"q\nBT\n".to_vec();
        let ops = lay.write_ops(&Matrix::IDENTITY, &TextState::default(), &|f| {
            names.get(&f).cloned().unwrap_or_default()
        })?;
        b.extend(ops);
        b.extend_from_slice(b"ET\nQ\n");
        self.append.push(b);
        Ok(self.report(&lay, &[]))
    }

    fn report(&self, lay: &BlockLayout, blocks: &[crate::ObjectId]) -> BlockReport {
        let fr = &lay.frame;
        let end = lay
            .lines
            .last()
            .map_or(fr.bottom, |l| l.baseline + l.descent);
        let mut overlaps = Vec::new();
        if end < fr.bottom - 0.5 {
            let grown = Rect::from_points(&[
                fr.matrix.apply(Point::new(fr.left, end)),
                fr.matrix.apply(Point::new(fr.left + fr.width, fr.bottom)),
            ]);
            for o in self.p.model.top_level() {
                if !blocks.contains(&o.id) && !o.hidden && intersects(&o.bbox, &grown) {
                    overlaps.push(o.id);
                }
            }
        }
        BlockReport {
            bbox: lay.bbox,
            lines: lay.lines.len(),
            overflow: lay.overflow,
            overfull: lay.overfull,
            overlaps,
        }
    }

    /// Adds an image over the page's content, drawn by a page-space matrix of its unit
    /// square (see [`image::placement_in`](crate::image::placement_in)).
    pub fn add_image(&mut self, img: &NewImage<'_>, placement: Matrix) -> Result<()> {
        let (r, _, _) = crate::image::embed(self.doc, img)?;
        self.place_image(r, placement);
        Ok(())
    }

    /// Adds an image drawn into a page-space rectangle, keeping its proportions.
    pub fn add_image_in(&mut self, img: &NewImage<'_>, rect: Rect) -> Result<()> {
        let (r, w, h) = crate::image::embed(self.doc, img)?;
        self.place_image(r, crate::image::placement_in(&rect, w, h));
        Ok(())
    }

    fn place_image(&mut self, r: pagebara_syntax::ObjRef, placement: Matrix) {
        let name = self.ch.add_resource(
            self.doc,
            &self.p,
            crate::StreamId(0),
            b"XObject",
            Object::Ref(r),
        );
        let mut b = b"q\n".to_vec();
        emit::cm(&mut b, &placement);
        emit::named_op(&mut b, &name, b"Do");
        b.extend_from_slice(b"Q\n");
        self.append.push(b);
    }

    /// Adds text over the page's content in a font `enc` provides.
    pub fn add_text(&mut self, t: &NewText<'_>, enc: &dyn TextEncoder) -> Result<()> {
        let mut b = b"q\nBT\n".to_vec();
        emit::color(&mut b, &t.color, false);
        emit::nums(&mut b, &[t.leading.unwrap_or(1.2 * t.size).into()]);
        b.extend_from_slice(b"TL\n");
        let m = crate::geom::rotate_about(Point::default(), t.angle)
            .then(&Matrix::translate(t.origin.x, t.origin.y));
        emit::tm(&mut b, &m);
        let stream = crate::StreamId(0);
        for (k, line) in t.text.split('\n').enumerate() {
            let e = enc.encode(
                self.doc,
                &EncodeRequest {
                    text: line,
                    font: None,
                    size: t.size,
                },
            )?;
            let FontChoice::New(o) = &e.font else {
                return Err(Error::Unsupported("the encoder gave no font for new text"));
            };
            let font = load_font(self.doc, o).ok_or(Error::Unsupported("unreadable font"))?;
            let name = self
                .ch
                .add_resource(self.doc, &self.p, stream, b"Font", o.clone());
            if k > 0 {
                b.extend_from_slice(b"T*\n");
            }
            font_op(&mut b, &name, t.size);
            for (n, _, items) in self.segments(stream, &e.show, &name, &font)? {
                if let Some(n) = n {
                    font_op(&mut b, &n, t.size);
                }
                show_ops(&mut b, &items);
            }
        }
        b.extend_from_slice(b"ET\nQ\n");
        self.append.push(b);
        Ok(())
    }

    /// Moves objects drawn by the page itself in the painting order.
    pub fn reorder(&mut self, targets: &[Target], z: ZOrder) -> Result<()> {
        let ks = self.items(targets)?;
        if ks.is_empty() {
            return Ok(());
        }
        if ks
            .iter()
            .any(|k| self.edits.contains_key(k) || self.p.scan.items[*k].stream.0 != 0)
        {
            return Err(Error::Unsupported(
                "only objects drawn by the page itself, without other pending edits, move",
            ));
        }
        let scan = &self.p.scan;
        let bbox = ks
            .iter()
            .map(|&k| scan.items[k].bbox)
            .reduce(|a, b| a.union(&b))
            .unwrap_or_default();
        let first = ks
            .iter()
            .map(|&k| scan.items[k].span().start)
            .min()
            .unwrap_or(0);
        let last = ks
            .iter()
            .map(|&k| scan.items[k].span().end)
            .max()
            .unwrap_or(0);
        let others = |k: &usize| {
            let it = &scan.items[*k];
            !ks.contains(k) && it.stream.0 == 0 && !it.hidden && intersects(&it.bbox, &bbox)
        };
        // A place to insert: outside text objects, in marked content the objects are in.
        let valid = |b: &crate::scan::Boundary| {
            let snap = &scan.boundary_snaps[b.snap as usize];
            !b.in_text
                && ks.iter().all(|&k| {
                    let it = &scan.items[k];
                    (b.frame == 0 || it.frames.contains(&(b.frame - 1)))
                        && it.snap.clips.starts_with(&snap.clips)
                })
        };
        enum Dest {
            Start,
            End,
            At(usize),
        }
        let dest = match z {
            ZOrder::Front => Dest::End,
            ZOrder::Back => Dest::Start,
            ZOrder::Forward => {
                let Some(b) = (0..scan.items.len())
                    .filter(others)
                    .find(|&k| scan.items[k].span().start >= last)
                else {
                    return Ok(());
                };
                let end = scan.items[b].span().end;
                match scan
                    .boundaries
                    .iter()
                    .position(|x| x.offset >= end && valid(x))
                {
                    Some(i) => Dest::At(i),
                    None => Dest::End,
                }
            }
            ZOrder::Backward => {
                let Some(b) = (0..scan.items.len())
                    .filter(others)
                    .rfind(|&k| scan.items[k].span().end <= first)
                else {
                    return Ok(());
                };
                let start = scan.items[b].span().start;
                match scan
                    .boundaries
                    .iter()
                    .rposition(|x| x.offset <= start && valid(x))
                {
                    Some(i) => Dest::At(i),
                    None => Dest::Start,
                }
            }
        };
        let (base, clips, frame) = match dest {
            Dest::At(i) => {
                let b = scan.boundaries[i];
                let s = &scan.boundary_snaps[b.snap as usize];
                (s.ctm, s.clips.clone(), b.frame)
            }
            _ => (Matrix::IDENTITY, Vec::new(), 0),
        };
        let mut snippet = Vec::new();
        for &k in &ks {
            snippet.extend(self.standalone(k, &base, &clips, frame)?);
        }
        // Marked content left empty goes; the objects take it along.
        let scan = &self.p.scan;
        let mut gone = Vec::new();
        for (f, fr) in scan.frames.iter().enumerate() {
            if fr.stream.0 == 0
                && !fr.items.is_empty()
                && fr.items.iter().all(|k| ks.contains(k))
                && let Some(end) = fr.end
            {
                gone.push((fr.begin, end, f));
            }
        }
        for (b, e, _) in gone {
            self.ch.patch(crate::StreamId(0), b.0, b.1, b"\n".to_vec());
            self.ch.patch(crate::StreamId(0), e.0, e.1, b"\n".to_vec());
        }
        for &k in &ks {
            self.edit(k).delete = true;
        }
        match dest {
            Dest::Start => self.prepend.push(snippet),
            Dest::End => self.append.push(snippet),
            Dest::At(i) => {
                let at = self.p.scan.boundaries[i].offset;
                self.ch.insert(crate::StreamId(0), at, snippet);
            }
        }
        self.moved = true;
        Ok(())
    }

    /// An item drawn on its own where the CTM is `base`, the clips `clips` are in effect
    /// and marked content `frame` (plus one) is innermost.
    fn standalone(&self, k: usize, base: &Matrix, clips: &[u32], frame: u32) -> Result<Vec<u8>> {
        let scan = &self.p.scan;
        let it = &scan.items[k];
        let data = &scan.streams[it.stream.0 as usize].data;
        let wrap: Vec<u32> = match it.frames.iter().position(|&f| f + 1 == frame) {
            Some(i) => it.frames[i + 1..].to_vec(),
            None => it.frames.clone(),
        };
        let mut out = b"\n".to_vec();
        for &f in &wrap {
            out.extend_from_slice(&scan.frames[f as usize].begin_ops);
        }
        out.extend_from_slice(b"q\n");
        if !is_identity(base) {
            emit::cm(
                &mut out,
                &base.invert().ok_or(Error::Unsupported("singular matrix"))?,
            );
        }
        for &c in &it.snap.clips[clips.len().min(it.snap.clips.len())..] {
            let rec = &scan.clips[c as usize];
            let path = rec
                .path
                .as_ref()
                .ok_or(Error::Unsupported("clipped by text"))?;
            let inv = rec
                .ctm
                .invert()
                .ok_or(Error::Unsupported("singular matrix"))?;
            emit::cm(&mut out, &rec.ctm);
            out.extend_from_slice(path);
            emit::cm(&mut out, &inv);
        }
        emit::cm(&mut out, &it.snap.ctm);
        it.snap.state_ops(&mut out);
        out.extend_from_slice(&body(it, data));
        if let Source::Run(r) = &it.src {
            let mut t = b"BT\n".to_vec();
            it.snap.text_ops(&mut t);
            emit::tm(&mut t, &r.tm);
            t.extend_from_slice(&r.operand);
            t.extend_from_slice(if r.show == ShowKind::TJ {
                b" TJ\n"
            } else {
                b" Tj\n"
            });
            t.extend_from_slice(b"ET\n");
            out.extend_from_slice(&t);
        }
        out.extend_from_slice(b"\nQ\n");
        for _ in &wrap {
            out.extend_from_slice(b"EMC\n");
        }
        Ok(out)
    }

    /// Writes the edits into the document.
    pub fn commit(mut self) -> Result<()> {
        let edits = std::mem::take(&mut self.edits);
        for (k, e) in &edits {
            let r = match &self.p.scan.items[*k].src {
                Source::Run(_) => self.render_run(*k, e)?,
                _ => self.render_op(*k, e)?,
            };
            if let Some(b) = r {
                let s = self.p.scan.items[*k].span();
                self.ch.patch(s.stream, s.start, s.end, b);
            }
        }
        let rec = &self.p.scan.streams[0];
        let len = rec.data.len() as u32;
        for b in std::mem::take(&mut self.prepend) {
            self.ch.insert(crate::StreamId(0), 0, b);
        }
        if !self.append.is_empty() {
            // The page's content in its own q/Q, so added content starts afresh.
            let (saves, text) = (rec.open_saves, rec.open_text);
            self.ch.insert(crate::StreamId(0), 0, b"q\n".to_vec());
            let mut end = b"\n".to_vec();
            if text {
                end.extend_from_slice(b"ET\n");
            }
            for _ in 0..=saves {
                end.extend_from_slice(b"Q\n");
            }
            for b in std::mem::take(&mut self.append) {
                end.extend(b);
            }
            self.ch.insert(crate::StreamId(0), len, end);
        }
        if self.ch.is_empty() {
            return Ok(());
        }
        let ch = std::mem::take(&mut self.ch);
        commit(self.doc, &self.p, ch, self.forms)
    }

    /// The replacement of a path, image, form or shading.
    fn render_op(&mut self, k: usize, e: &ItemEdit) -> Result<Option<Vec<u8>>> {
        let it = &self.p.scan.items[k];
        let data = self.p.scan.streams[it.stream.0 as usize].data.clone();
        let body = body(it, &data);
        let keep = match it.src {
            Source::Paths {
                start,
                clip: Some((cs, ce)),
                ..
            } => {
                let mut b = data[start as usize..cs as usize].to_vec();
                b.extend_from_slice(&data[cs as usize..ce as usize]);
                b.extend_from_slice(b" n\n");
                Some(b)
            }
            _ => None,
        };
        let mut out = b"\n".to_vec();
        if !e.delete {
            if e.visual() {
                let w = self.wrapped(k, e, e.transform, &body)?;
                out.extend(w);
            } else {
                out.extend_from_slice(&body);
                out.push(b'\n');
            }
        }
        for c in &e.copies {
            let t = Some(e.transform.map_or(*c, |t| t.then(c)));
            let w = self.wrapped(k, e, t, &body)?;
            out.extend(w);
        }
        if let Some(kp) = keep {
            out.extend(kp);
        }
        Ok(Some(out))
    }

    /// `q`, the edited state, the body, `Q`.
    fn wrapped(
        &mut self,
        k: usize,
        e: &ItemEdit,
        t: Option<Matrix>,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        let it = &self.p.scan.items[k];
        let ctm = it.snap.ctm;
        let stream = it.stream;
        let mut out = b"q\n".to_vec();
        if e.fill_alpha.is_some() || e.stroke_alpha.is_some() {
            let name = self.alpha_gs(stream, e.fill_alpha, e.stroke_alpha);
            emit::named_op(&mut out, &name, b"gs");
        }
        if let Some(c) = &e.fill {
            emit::color(&mut out, c, false);
        }
        if let Some(c) = &e.stroke {
            emit::color(&mut out, c, true);
        }
        if let Some(w) = e.line_width {
            emit::nums(&mut out, &[w.into()]);
            out.extend_from_slice(b"w\n");
        }
        if let Some(d) = &e.dash {
            out.push(b'[');
            emit::nums(&mut out, &d.iter().map(|&v| v.into()).collect::<Vec<_>>());
            out.extend_from_slice(b"] 0 d\n");
        }
        if let Some(t) = t {
            let m = in_user_space(&t, &ctm).ok_or(Error::Unsupported("singular matrix"))?;
            emit::cm(&mut out, &m);
        }
        if let Some(q) = e.crop {
            let inv = ctm.invert().ok_or(Error::Unsupported("singular matrix"))?;
            for (i, p) in q.iter().enumerate() {
                let p = inv.apply(*p);
                emit::nums(&mut out, &[p.x.into(), p.y.into()]);
                out.extend_from_slice(if i == 0 { b"m\n" } else { b"l\n" });
            }
            out.extend_from_slice(b"h W n\n");
        }
        match &e.image {
            Some((name, fit)) => {
                if !is_identity(fit) {
                    emit::cm(&mut out, fit);
                }
                emit::named_op(&mut out, name, b"Do");
            }
            None => out.extend_from_slice(body),
        }
        out.extend_from_slice(b"\nQ\n");
        Ok(out)
    }

    /// An ExtGState setting constant opacity.
    fn alpha_gs(
        &mut self,
        stream: crate::StreamId,
        fill: Option<f32>,
        stroke: Option<f32>,
    ) -> Vec<u8> {
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"ExtGState"));
        if let Some(v) = fill {
            d.set(b"ca", Object::Real(f64::from(v)));
        }
        if let Some(v) = stroke {
            d.set(b"CA", Object::Real(f64::from(v)));
        }
        let r = self.doc.add(d);
        self.ch
            .add_resource(self.doc, &self.p, stream, b"ExtGState", Object::Ref(r))
    }

    /// The replacement of a text-showing operator, inside its text object.
    fn render_run(&mut self, k: usize, e: &ItemEdit) -> Result<Option<Vec<u8>>> {
        let it = self.p.scan.items[k].clone();
        let Source::Run(src) = &it.src else {
            return Ok(None);
        };
        let ItemData::Run(rd) = &it.data else {
            return Ok(None);
        };
        let g = &it.snap;
        let vertical = rd.vertical;
        let mut out = b"\n".to_vec();
        out.extend_from_slice(&src.prefix);
        let off = |a: &Matrix, b: &Matrix| relative(a, b).map(|m| (m.e, m.f));
        if let Some(ops) = &e.replace {
            out.extend_from_slice(ops);
            emit::tm(&mut out, &src.tlm);
            let (u, v) = off(&src.tm_after, &src.tlm).unwrap_or((0.0, 0.0));
            let font_size = g.size * if vertical { 1.0 } else { g.th };
            if !adjust(&mut out, u, v, vertical, font_size) {
                emit::tm(&mut out, &src.tm_after);
            }
            return Ok(Some(out));
        }
        let mut set = Vec::new();
        let mut restore = Vec::new();
        if let Some(c) = &e.fill {
            emit::color(&mut set, c, false);
            restore.extend_from_slice(&g.fill_ops);
        }
        if let Some(c) = &e.stroke {
            emit::color(&mut set, c, true);
            restore.extend_from_slice(&g.stroke_ops);
        }
        if e.fill_alpha.is_some() || e.stroke_alpha.is_some() {
            let n = self.alpha_gs(it.stream, e.fill_alpha, e.stroke_alpha);
            emit::named_op(&mut set, &n, b"gs");
            let n = self.alpha_gs(
                it.stream,
                e.fill_alpha.map(|_| g.fill_alpha),
                e.stroke_alpha.map(|_| g.stroke_alpha),
            );
            emit::named_op(&mut restore, &n, b"gs");
        }
        if let Some(m) = e.render_mode {
            emit::nums(&mut set, &[m.into()]);
            set.extend_from_slice(b"Tr\n");
            emit::nums(&mut restore, &[g.mode.into()]);
            restore.extend_from_slice(b"Tr\n");
        }
        let new_font = e.text.as_ref().and_then(|t| t.font.clone());
        let switches = e.text.as_ref().is_some_and(|t| t.switches);
        let size = e.size.unwrap_or(g.size);
        if new_font.is_some() || e.size.is_some() || switches {
            let old = match &g.font_raw {
                Some(n) => n.clone(),
                None => {
                    let f = g
                        .font
                        .as_ref()
                        .ok_or(Error::Unsupported("the run has no font"))?;
                    self.ch
                        .add_resource(self.doc, &self.p, it.stream, b"Font", f.object.clone())
                }
            };
            let name = new_font.unwrap_or_else(|| old.clone());
            font_op(&mut set, &name, size);
            font_op(&mut restore, &old, g.size);
        }
        // The old advance, and the new one as shown.
        let (u, v) = off(&src.tm_after, &src.tm).unwrap_or((0.0, 0.0));
        let old_adv = if vertical { v } else { u };
        let new_adv = match &e.text {
            Some(t) => t.advance,
            None if e.size.is_some() && g.size != 0.0 => {
                let spacing: f32 = rd.glyphs.iter().map(|gl| gl.advance - gl.scaled).sum();
                (old_adv - spacing) * size / g.size + spacing
            }
            None => old_adv,
        };
        let show = match &e.text {
            Some(t) => t.ops.clone(),
            None => {
                let mut b = src.operand.clone();
                b.extend_from_slice(if src.show == ShowKind::TJ {
                    b" TJ\n"
                } else {
                    b" Tj\n"
                });
                b
            }
        };
        let ctm = g.ctm;
        let ctm_inv = ctm.invert();
        let mut moved = false;
        out.extend_from_slice(&set);
        if !e.delete {
            if let Some(t) = e.transform {
                let inv = ctm_inv.ok_or(Error::Unsupported("singular matrix"))?;
                emit::tm(&mut out, &src.tm.then(&ctm).then(&t).then(&inv));
                moved = true;
            }
            out.extend_from_slice(&show);
        }
        for c in &e.copies {
            let inv = ctm_inv.ok_or(Error::Unsupported("singular matrix"))?;
            let t = e.transform.map_or(*c, |t| t.then(c));
            emit::tm(&mut out, &src.tm.then(&ctm).then(&t).then(&inv));
            out.extend_from_slice(&show);
            moved = true;
        }
        out.extend_from_slice(&restore);
        // Back to where the original text left the text position.
        let font_size = g.size * if vertical { 1.0 } else { g.th };
        if moved {
            emit::tm(&mut out, &src.tlm);
            let (u, v) = off(&src.tm_after, &src.tlm).unwrap_or((0.0, 0.0));
            if !adjust(&mut out, u, v, vertical, font_size) {
                emit::tm(&mut out, &src.tm_after);
            }
        } else {
            let shown = if e.delete { 0.0 } else { new_adv };
            let d = old_adv - shown;
            if d.abs() > 1e-4 {
                let (du, dv) = if vertical { (0.0, d) } else { (d, 0.0) };
                if !adjust(&mut out, du, dv, vertical, font_size) {
                    emit::tm(&mut out, &src.tm_after);
                }
            }
        }
        Ok(Some(out))
    }
}

/// The operators of an item that draw it (a path that clips without its `W`).
fn body(it: &Item, data: &[u8]) -> Vec<u8> {
    match it.src {
        Source::Paths {
            start,
            end,
            clip: Some((cs, ce)),
        } => {
            let mut b = data[start as usize..cs as usize].to_vec();
            b.extend_from_slice(&data[ce as usize..end as usize]);
            b
        }
        Source::Paths { start, end, .. } | Source::Op { start, end } => {
            data[start as usize..end as usize].to_vec()
        }
        Source::Run(_) => Vec::new(),
    }
}

fn font_op(out: &mut Vec<u8>, name: &[u8], size: f32) {
    emit::name(out, name);
    out.push(b' ');
    emit::nums(out, &[size.into()]);
    out.extend_from_slice(b"Tf\n");
}

/// A `TJ` moving the text position by `(u, v)` in text space along the writing direction;
/// false when it cannot (movement across it, or no font size).
fn adjust(out: &mut Vec<u8>, u: f32, v: f32, vertical: bool, size: f32) -> bool {
    let (along, across) = if vertical { (v, u) } else { (u, v) };
    if across.abs() > 1e-3 || size == 0.0 || !size.is_finite() {
        return along.abs() < 1e-4 && across.abs() < 1e-3;
    }
    if along.abs() < 1e-4 {
        return true;
    }
    out.push(b'[');
    emit::num(out, f64::from(-along) * 1000.0 / f64::from(size));
    out.extend_from_slice(b"] TJ\n");
    true
}

/// The operand and operator showing encoded items.
fn show_ops(out: &mut Vec<u8>, show: &[ShowItem]) {
    if let [ShowItem::Text(b)] = show {
        emit::hex_string(out, b);
        out.extend_from_slice(b" Tj\n");
        return;
    }
    out.push(b'[');
    for s in show {
        match s {
            ShowItem::Text(b) => emit::hex_string(out, b),
            ShowItem::Adjust(n) => {
                out.push(b' ');
                emit::num(out, f64::from(*n));
                out.push(b' ');
            }
            ShowItem::Font(_) => {}
        }
    }
    out.extend_from_slice(b"] TJ\n");
}

/// Where showing `items` leaves the text position, in text space.
fn advance(font: &Font, items: &[ShowItem], size: f32, tc: f32, tw: f32, th: f32) -> (f32, f32) {
    let (mut tx, mut ty) = (0.0f32, 0.0f32);
    for it in items {
        match it {
            ShowItem::Adjust(n) => {
                let adj = n / 1000.0 * size;
                if font.vertical {
                    ty -= adj;
                } else {
                    tx -= adj * th;
                }
            }
            ShowItem::Font(_) => {}
            ShowItem::Text(b) => {
                let mut s = &b[..];
                while !s.is_empty() {
                    let (code, len) = font.next_code(s);
                    let len = len.clamp(1, s.len());
                    s = &s[len..];
                    let spacing = tc + if len == 1 && code == 32 { tw } else { 0.0 };
                    if font.vertical {
                        let (w1, _) = font.vertical_metrics(code);
                        ty += w1 * size + spacing;
                    } else {
                        tx += (font.width(code) * size + spacing) * th;
                    }
                }
            }
        }
    }
    (tx, ty)
}

fn load_font(doc: &Document, o: &Object) -> Option<std::sync::Arc<Font>> {
    let d = doc.resolve(o);
    let programs = pagebara_content::font::embedded_programs();
    Some(std::sync::Arc::new(Font::load_with(
        doc,
        d.as_dict()?,
        Some(&*programs),
    )))
}

/// A PDF text string in UTF-16BE.
fn utf16(s: &str) -> pagebara_syntax::PdfString {
    let mut b = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_be_bytes());
    }
    pagebara_syntax::PdfString {
        bytes: b,
        hex: true,
    }
}

/// Reads page `page` of `doc`.
pub(crate) fn parse_page(doc: &Document, page: usize, boundaries: bool) -> Result<Parsed> {
    let tree = pagebara_pages::PageTree::open(doc);
    let pg = tree.page(page).ok_or(Error::NoSuchPage(page))?;
    let po = doc.get(pg.r).ok_or(Error::NotAPage)?;
    let pd = po.as_dict().ok_or(Error::NotAPage)?;
    let frame = crate::geom::PageFrame::of(&pg);
    let res = crate::write::inherited_resources(doc, pd);
    let scan = Scanner::new(doc, frame, boundaries).page(pd, res);
    Ok(assemble(doc, page, pg.r, frame, scan))
}
