//! `gs`: graphics state parameter dictionaries (§8.4.5).

use pagebara_syntax::Object;

use crate::display::{BlendMode, NONE, RenderingIntent, SoftMask};
use crate::interp::{Builder, Scope, line_cap, line_join};

impl Builder<'_, '_> {
    pub(crate) fn ext_gstate(&mut self, name: &[u8], sc: &mut Scope<'_>) {
        let Some(raw) = self.resource(sc.res, b"ExtGState", name) else {
            return;
        };
        let doc = self.doc;
        let o = doc.resolve(&raw);
        let Some(d) = o.as_dict() else { return };
        let mut paints = false;
        for (k, v) in d.iter() {
            let v = doc.resolve(v);
            let num = v.as_f64().map(|n| n as f32);
            match k.as_bytes() {
                b"LW" => {
                    if let Some(n) = num {
                        self.gs.line_width = n.abs();
                    }
                }
                b"LC" => {
                    if let Some(n) = num {
                        self.gs.cap = line_cap(n);
                    }
                }
                b"LJ" => {
                    if let Some(n) = num {
                        self.gs.join = line_join(n);
                    }
                }
                b"ML" => {
                    if let Some(n) = num {
                        self.gs.miter_limit = n.max(1.0);
                    }
                }
                b"D" => {
                    if let Some([arr, phase]) = v.as_array() {
                        let dash: Vec<f32> = doc
                            .resolve(arr)
                            .as_array()
                            .unwrap_or_default()
                            .iter()
                            .take(16)
                            .filter_map(|o| doc.resolve(o).as_f64().map(|v| v.abs() as f32))
                            .collect();
                        let phase = doc.resolve(phase).as_f64().unwrap_or(0.0) as f32;
                        self.set_dash(&dash, phase);
                    }
                }
                b"SA" => self.gs.stroke_adjust = v.as_bool().unwrap_or(false),
                b"FL" => self.gs.flatness = num.unwrap_or(1.0).clamp(0.0, 100.0),
                b"SM" => self.gs.smoothness = num.unwrap_or(0.0).clamp(0.0, 1.0),
                b"RI" => {
                    if let Some(n) = v.as_name() {
                        self.gs.intent = RenderingIntent::from_name(n);
                        paints = true;
                    }
                }
                b"CA" => {
                    if let Some(n) = num {
                        self.gs.stroke_alpha = n.clamp(0.0, 1.0);
                        paints = true;
                    }
                }
                b"ca" => {
                    if let Some(n) = num {
                        self.gs.fill_alpha = n.clamp(0.0, 1.0);
                        paints = true;
                    }
                }
                b"BM" => {
                    let bm = match &*v {
                        Object::Name(n) => BlendMode::from_name(n.as_bytes()),
                        Object::Array(a) => a
                            .iter()
                            .find_map(|n| n.as_name().and_then(BlendMode::from_name)),
                        _ => None,
                    };
                    self.gs.blend = bm.unwrap_or_default();
                    paints = true;
                }
                b"SMask" => {
                    self.gs.soft_mask = match &*v {
                        Object::Dict(_) => {
                            let raw = d.get(b"SMask").cloned().unwrap_or_default();
                            self.list.soft_masks.push(SoftMask {
                                object: raw,
                                ctm: self.gs.ctm,
                            });
                            self.list.soft_masks.len() as u32 - 1
                        }
                        _ => NONE,
                    };
                    paints = true;
                }
                b"AIS" => self.gs.alpha_is_shape = v.as_bool().unwrap_or(false),
                b"TK" => self.gs.text_knockout = v.as_bool().unwrap_or(true),
                b"OP" => {
                    let op = v.as_bool().unwrap_or(false);
                    self.gs.overprint_stroke = op;
                    if !d.contains(b"op") {
                        self.gs.overprint_fill = op;
                    }
                    paints = true;
                }
                b"op" => {
                    self.gs.overprint_fill = v.as_bool().unwrap_or(false);
                    paints = true;
                }
                b"OPM" => {
                    self.gs.overprint_mode = u8::from(num == Some(1.0));
                    paints = true;
                }
                b"Font" => {
                    if let Some([font, size]) = v.as_array() {
                        self.gs.text.font = self.load_font(b"", font);
                        self.gs.text.size = doc.resolve(size).as_f64().unwrap_or(0.0) as f32;
                    }
                }
                _ => {}
            }
        }
        self.gs.style_id = NONE;
        if paints {
            self.gs.paints_changed();
        }
    }
}
