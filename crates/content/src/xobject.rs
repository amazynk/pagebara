//! XObjects (§8.8, §8.10), inline images (§8.9.7) and shadings (`sh`).

use std::sync::Arc;

use pagebara_syntax::{Dict, Name, ObjRef, Object, Stream};

use crate::display::{Cmd, Group, Image, ImageSource};
use crate::geom::{Matrix, Rect};
use crate::interp::{Builder, Scope};
use crate::lex::Operand;
use crate::parse::Instr;

impl Builder<'_, '_> {
    pub(crate) fn xobject(&mut self, name: &[u8], sc: &mut Scope<'_>) {
        if !self.visible() {
            return;
        }
        let Some(raw) = self.resource(sc.res, b"XObject", name) else {
            return;
        };
        let o = self.doc.resolve(&raw);
        let Some(s) = o.as_stream() else { return };
        if !self.oc_visible(&s.dict) {
            return;
        }
        match s.dict.name(b"Subtype") {
            Some(b"Image") => {
                let d = &s.dict;
                let int = |k: &[u8]| {
                    d.get(k)
                        .and_then(|o| self.doc.resolve(o).as_int())
                        .unwrap_or(0)
                        .clamp(0, u32::MAX.into()) as u32
                };
                let image = Image {
                    width: int(b"Width"),
                    height: int(b"Height"),
                    is_mask: d
                        .get(b"ImageMask")
                        .is_some_and(|o| self.doc.resolve(o).as_bool() == Some(true)),
                    source: ImageSource::XObject(raw.clone()),
                };
                self.draw_image(image);
            }
            Some(b"Form") | None => self.form(raw.as_objref(), s, sc.res),
            _ => {}
        }
    }

    fn draw_image(&mut self, image: Image) {
        self.list.images.push(image);
        let image = self.list.images.len() as u32 - 1;
        let ctm = self.ctm_id();
        let paint = self.fill_paint();
        self.list.cmds.push(Cmd::Image { image, ctm, paint });
    }

    /// Runs a form XObject: matrix, bounding box clip, transparency group, own resources.
    pub(crate) fn form(&mut self, r: Option<ObjRef>, s: &Stream, parent_res: &Dict) {
        if self.forms.len() >= self.it.options.max_form_depth
            || r.is_some_and(|r| self.forms.contains(&r))
        {
            return;
        }
        let doc = self.doc;
        let Ok(data) = doc.decode(s) else { return };
        let d = &s.dict;
        let height = self.enter();
        if let Some(m) = d
            .get(b"Matrix")
            .and_then(|m| Matrix::from_object(&doc.resolve(m)))
            .map(|m| m.then(&self.gs.ctm))
            .filter(Matrix::is_finite)
        {
            self.gs.ctm = m;
            self.gs.ctm_id = crate::display::NONE;
        }
        let bbox = d
            .get(b"BBox")
            .and_then(|b| Rect::from_object(&doc.resolve(b)));
        if let Some(b) = bbox {
            self.clip_rect(b);
        }
        let group = doc.get_in(d, b"Group");
        let group = group
            .as_dict()
            .filter(|g| g.name(b"S") == Some(b"Transparency"));
        // A form drawn under a soft mask is masked as a whole, as an isolated group (as
        // in MuPDF and PDFium), so its blend modes do not reach through to the page.
        let masked = group.is_none() && self.gs.soft_mask != crate::display::NONE;
        if group.is_some() || masked {
            let color_space = group.and_then(|g| g.get(b"CS")).map(|cs| self.space(cs, 0));
            let flag = |k: &[u8]| {
                group
                    .and_then(|g| g.get(k))
                    .is_some_and(|o| doc.resolve(o).as_bool() == Some(true))
            };
            let ctm = self.ctm_id();
            let paint = self.fill_paint();
            self.list.groups.push(Group {
                bbox: bbox.unwrap_or_default(),
                ctm,
                isolated: masked || flag(b"I"),
                knockout: flag(b"K"),
                color_space,
                paint,
            });
            let group = self.list.groups.len() as u32 - 1;
            self.list.cmds.push(Cmd::BeginGroup { group });
            // The group composites with these; its contents start fresh.
            self.gs.fill_alpha = 1.0;
            self.gs.stroke_alpha = 1.0;
            self.gs.blend = Default::default();
            self.gs.soft_mask = crate::display::NONE;
            self.gs.paints_changed();
        }
        let res = doc.get_in(d, b"Resources");
        let res = res.as_dict().unwrap_or(parent_res);
        if let Some(r) = r {
            self.forms.push(r);
        } else {
            // Unreferenced forms still count towards the depth limit.
            self.forms.push(ObjRef::new(0, u16::MAX));
        }
        let base = self.gs.ctm;
        self.run(&data.data, res, base);
        self.forms.pop();
        if group.is_some() || masked {
            self.list.cmds.push(Cmd::EndGroup);
        }
        self.leave(height);
    }

    pub(crate) fn shade(&mut self, name: &[u8], sc: &mut Scope<'_>) {
        if !self.visible() {
            return;
        }
        let Some(raw) = self.resource(sc.res, b"Shading", name) else {
            return;
        };
        self.list.shadings.push(raw);
        let shading = self.list.shadings.len() as u32 - 1;
        let ctm = self.ctm_id();
        let paint = self.fill_paint();
        self.list.cmds.push(Cmd::Shading {
            shading,
            ctm,
            paint,
        });
    }

    pub(crate) fn inline_image(&mut self, i: &Instr<'_, '_>, sc: &mut Scope<'_>) {
        if !self.visible() {
            return;
        }
        let mut dict = Dict::new();
        // Abbreviated keys are the inline image's own (§8.9.7) and win over full names
        // given too: those go first.
        let pairs = i.args.as_chunks::<2>().0;
        let (full, short): (Vec<&[Operand<'_>; 2]>, Vec<_>) = pairs
            .iter()
            .partition(|kv| kv[0].name().is_some_and(|k| *expand_key(&k) == *k.as_ref()));
        for kv in full.into_iter().chain(short) {
            let Some(k) = kv[0].name() else { continue };
            let key = expand_key(&k);
            let mut v = i.object(&kv[1]);
            match key {
                b"ColorSpace" => v = self.inline_space(v, sc),
                b"Filter" => expand_names(&mut v, expand_filter),
                _ => {}
            }
            dict.set(key, v);
        }
        let int = |k: &[u8]| dict.int(k).unwrap_or(0).clamp(0, u32::MAX.into()) as u32;
        let image = Image {
            width: int(b"Width"),
            height: int(b"Height"),
            is_mask: dict.get(b"ImageMask").and_then(Object::as_bool) == Some(true),
            source: ImageSource::Inline(Arc::new(Stream::new(dict, i.data))),
        };
        self.draw_image(image);
    }

    /// Full names for an inline image colour space, or the resource it names, resolved.
    fn inline_space(&mut self, mut v: Object, sc: &mut Scope<'_>) -> Object {
        if let Object::Name(n) = &v {
            if let Some(full) = expand_space(n.as_bytes()) {
                return Object::name(full);
            }
            if let Some(raw) = self.resource(sc.res, b"ColorSpace", n.as_bytes()) {
                return (*self.doc.resolve(&raw)).clone();
            }
            return v;
        }
        if let Object::Array(a) = &mut v {
            for (k, e) in a.iter_mut().enumerate() {
                if let Object::Name(n) = e {
                    if let Some(full) = expand_space(n.as_bytes()) {
                        *e = Object::name(full);
                    } else if k == 1
                        && let Some(raw) = self.resource(sc.res, b"ColorSpace", n.as_bytes())
                    {
                        *e = raw;
                    }
                }
            }
        }
        v
    }
}

fn expand_key(k: &[u8]) -> &[u8] {
    match k {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        b"L" => b"Length",
        _ => k,
    }
}

fn expand_space(n: &[u8]) -> Option<&'static [u8]> {
    Some(match n {
        b"G" | b"DeviceGray" => b"DeviceGray",
        b"RGB" | b"DeviceRGB" => b"DeviceRGB",
        b"CMYK" | b"DeviceCMYK" => b"DeviceCMYK",
        b"I" | b"Indexed" => b"Indexed",
        _ => return None,
    })
}

fn expand_filter(n: &[u8]) -> Option<&'static [u8]> {
    Some(match n {
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"LZW" => b"LZWDecode",
        b"Fl" => b"FlateDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        _ => return None,
    })
}

fn expand_names(v: &mut Object, f: fn(&[u8]) -> Option<&'static [u8]>) {
    match v {
        Object::Name(n) => {
            if let Some(full) = f(n.as_bytes()) {
                *n = Name::new(full);
            }
        }
        Object::Array(a) => a.iter_mut().for_each(|e| expand_names(e, f)),
        _ => {}
    }
}
