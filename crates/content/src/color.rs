//! Colour operators (§8.6.8) and colour space classification. Values are recorded as given;
//! conversion is the renderer's.

use pagebara_syntax::Object;

use crate::display::{ColorSpace, ColorSpaceEntry, ColorSpaceKind, NONE, PatternRef};
use crate::geom::Matrix;
use crate::interp::{Builder, Scope};
use crate::lex::Operand;
use crate::ops::Op;
use crate::parse::Instr;
use crate::state::{Color, MAX_COMPONENTS};

const MAX_DEPTH: u32 = 8;

impl Builder<'_, '_> {
    pub(crate) fn color_op(&mut self, i: &Instr<'_, '_>, sc: &mut Scope<'_>) {
        let stroke = matches!(
            i.op,
            Op::StrokeSpace
                | Op::StrokeColor
                | Op::StrokeColorN
                | Op::StrokeGray
                | Op::StrokeRgb
                | Op::StrokeCmyk
        );
        let color = match i.op {
            Op::StrokeGray | Op::FillGray => i
                .numbers::<1>()
                .map(|c| Color::new(ColorSpace::DeviceGray, &c)),
            Op::StrokeRgb | Op::FillRgb => i
                .numbers::<3>()
                .map(|c| Color::new(ColorSpace::DeviceRgb, &c)),
            Op::StrokeCmyk | Op::FillCmyk => i
                .numbers::<4>()
                .map(|c| Color::new(ColorSpace::DeviceCmyk, &c)),
            Op::StrokeSpace | Op::FillSpace => i
                .args
                .last()
                .and_then(Operand::name)
                .and_then(|n| self.named_space(&n, sc))
                .map(|cs| self.initial_color(cs)),
            _ => Some(self.set_components(i, stroke, sc)),
        };
        if let Some(c) = color {
            if stroke {
                self.gs.stroke = c;
            } else {
                self.gs.fill = c;
            }
            self.gs.paints_changed();
        }
    }

    /// `sc`/`scn`: components for the current space, and a pattern name for patterns.
    /// Extra leading operands are ignored; missing ones keep their values.
    fn set_components(&mut self, i: &Instr<'_, '_>, stroke: bool, sc: &mut Scope<'_>) -> Color {
        let mut c = if stroke { self.gs.stroke } else { self.gs.fill };
        let mut vals = [0.0; MAX_COMPONENTS];
        let mut k = 0;
        for v in i.args.iter().filter_map(Operand::number) {
            if k == MAX_COMPONENTS {
                vals.rotate_left(1);
                vals[k - 1] = v;
            } else {
                vals[k] = v;
                k += 1;
            }
        }
        let expected = usize::from(self.space_info(c.space).0);
        let pattern = self.is_pattern(c.space);
        if pattern {
            c.pattern = match i.args.last().and_then(Operand::name) {
                Some(n) => self.pattern(&n, sc),
                None => NONE,
            };
        }
        let take = if expected == 0 { k } else { k.min(expected) };
        c.comps[..take].copy_from_slice(&vals[k - take..k]);
        c.n = if pattern || expected == 0 {
            take
        } else {
            expected
        } as u8;
        c
    }

    fn is_pattern(&self, cs: ColorSpace) -> bool {
        matches!(cs, ColorSpace::Other(i)
            if self.list.color_spaces[i as usize].kind == ColorSpaceKind::Pattern)
    }

    /// The initial colour of a space (§8.6.5).
    pub(crate) fn initial_color(&self, cs: ColorSpace) -> Color {
        let (n, kind) = self.space_info(cs);
        let mut c = Color::new(cs, &[0.0; MAX_COMPONENTS][..usize::from(n)]);
        match kind {
            Some(ColorSpaceKind::Separation | ColorSpaceKind::DeviceN) => {
                c.comps[..usize::from(n)].fill(1.0)
            }
            None if cs == ColorSpace::DeviceCmyk => c.comps[3] = 1.0,
            Some(ColorSpaceKind::Pattern) => c.n = 0,
            _ => {}
        }
        c
    }

    fn space_info(&self, cs: ColorSpace) -> (u8, Option<ColorSpaceKind>) {
        match cs {
            ColorSpace::DeviceGray => (1, None),
            ColorSpace::DeviceRgb => (3, None),
            ColorSpace::DeviceCmyk => (4, None),
            ColorSpace::Other(i) => {
                let e = &self.list.color_spaces[i as usize];
                (e.components, Some(e.kind))
            }
        }
    }

    /// The colour space a `cs`/`CS` operand names: a family name or a resource.
    pub(crate) fn named_space(&mut self, name: &[u8], sc: &mut Scope<'_>) -> Option<ColorSpace> {
        if let Some(cs) = device_space(name) {
            return Some(cs);
        }
        if name == b"Pattern" {
            return Some(self.space(&Object::name(b"Pattern"), 0));
        }
        if let Some((_, cs)) = sc.spaces.iter().find(|(n, _)| **n == *name) {
            return Some(*cs);
        }
        let raw = self.resource(sc.res, b"ColorSpace", name)?;
        let cs = self.space(&raw, 0);
        sc.spaces.push((name.into(), cs));
        Some(cs)
    }

    /// Classifies a colour space object (a name or an array, possibly a reference).
    pub(crate) fn space(&mut self, raw: &Object, depth: u32) -> ColorSpace {
        if let Object::Ref(r) = raw
            && let Some(cs) = self.space_ids.get(r)
        {
            return *cs;
        }
        let o = self.doc.resolve(raw);
        let cs = self.classify(raw, &o, depth);
        if let Object::Ref(r) = raw {
            self.space_ids.insert(*r, cs);
        }
        cs
    }

    fn classify(&mut self, raw: &Object, o: &Object, depth: u32) -> ColorSpace {
        let gray = ColorSpace::DeviceGray;
        if depth > MAX_DEPTH {
            return gray;
        }
        let (family, args): (&[u8], &[Object]) = match o {
            Object::Name(n) => (n.as_bytes(), &[]),
            Object::Array(a) => match a.first().map(|f| self.doc.resolve(f)) {
                Some(f) => match f.as_name() {
                    Some(n) => (
                        // Families are short names; copy out of the resolved object.
                        match n {
                            b"Indexed" | b"I" => b"Indexed",
                            b"ICCBased" => b"ICCBased",
                            b"Separation" => b"Separation",
                            b"DeviceN" => b"DeviceN",
                            b"CalGray" => b"CalGray",
                            b"CalRGB" => b"CalRGB",
                            b"Lab" => b"Lab",
                            b"Pattern" => b"Pattern",
                            b"DeviceGray" | b"G" => b"DeviceGray",
                            b"DeviceRGB" | b"RGB" => b"DeviceRGB",
                            b"DeviceCMYK" | b"CMYK" => b"DeviceCMYK",
                            _ => b"",
                        },
                        &a[1..],
                    ),
                    None => return gray,
                },
                None => return gray,
            },
            _ => return gray,
        };
        if let Some(cs) = device_space(family) {
            return cs;
        }
        let arg = |i: usize| args.get(i).unwrap_or(&Object::Null);
        let (kind, n, base) = match family {
            b"CalGray" => (ColorSpaceKind::CalGray, 1, None),
            b"CalRGB" => (ColorSpaceKind::CalRgb, 3, None),
            b"Lab" => (ColorSpaceKind::Lab, 3, None),
            b"ICCBased" => {
                let s = self.doc.resolve(arg(0));
                let d = s.as_dict();
                let n = d
                    .and_then(|d| d.get(b"N"))
                    .and_then(|o| self.doc.resolve(o).as_int());
                let alt = d.and_then(|d| d.get(b"Alternate")).cloned();
                let base = match alt {
                    Some(a) => self.space(&a, depth + 1),
                    None => match n {
                        Some(1) => ColorSpace::DeviceGray,
                        Some(4) => ColorSpace::DeviceCmyk,
                        _ => ColorSpace::DeviceRgb,
                    },
                };
                let n = n.map_or_else(|| self.space_info(base).0, |n| n.clamp(1, 4) as u8);
                (ColorSpaceKind::IccBased, n, Some(base))
            }
            b"Indexed" => {
                let base = self.space(arg(0), depth + 1);
                (ColorSpaceKind::Indexed, 1, Some(base))
            }
            b"Separation" => {
                let base = self.space(arg(1), depth + 1);
                (ColorSpaceKind::Separation, 1, Some(base))
            }
            b"DeviceN" => {
                let names = self.doc.resolve(arg(0));
                let n = names
                    .as_array()
                    .map_or(1, |a| a.len().clamp(1, MAX_COMPONENTS));
                let base = self.space(arg(1), depth + 1);
                (ColorSpaceKind::DeviceN, n as u8, Some(base))
            }
            b"Pattern" => match args.first() {
                Some(u) => {
                    let base = self.space(u, depth + 1);
                    (ColorSpaceKind::Pattern, self.space_info(base).0, Some(base))
                }
                None => (ColorSpaceKind::Pattern, 0, None),
            },
            _ => (ColorSpaceKind::Unknown, 1, None),
        };
        let entry = ColorSpaceEntry {
            kind,
            components: n,
            base,
            object: raw.clone(),
        };
        let spaces = &mut self.list.color_spaces;
        let i = match spaces.iter().rposition(|e| *e == entry) {
            Some(i) => i,
            None => {
                spaces.push(entry);
                spaces.len() - 1
            }
        };
        ColorSpace::Other(i as u32)
    }

    /// A pattern resource as a pattern table entry.
    fn pattern(&mut self, name: &[u8], sc: &Scope<'_>) -> u32 {
        let Some(raw) = self.resource(sc.res, b"Pattern", name) else {
            return NONE;
        };
        let o = self.doc.resolve(&raw);
        let m = o
            .as_dict()
            .and_then(|d| d.get(b"Matrix"))
            .and_then(|m| Matrix::from_object(&self.doc.resolve(m)))
            .unwrap_or_default();
        let p = PatternRef {
            object: raw,
            matrix: m.then(&sc.base),
        };
        let pats = &mut self.list.patterns;
        match pats.iter().rposition(|e| *e == p) {
            Some(i) => i as u32,
            None => {
                pats.push(p);
                pats.len() as u32 - 1
            }
        }
    }
}

pub(crate) fn device_space(n: &[u8]) -> Option<ColorSpace> {
    Some(match n {
        b"DeviceGray" | b"G" => ColorSpace::DeviceGray,
        b"DeviceRGB" | b"RGB" => ColorSpace::DeviceRgb,
        b"DeviceCMYK" | b"CMYK" => ColorSpace::DeviceCmyk,
        _ => return None,
    })
}
