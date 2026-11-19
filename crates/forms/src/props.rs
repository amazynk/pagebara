//! Field properties as a form designer edits them ([`FieldProps`]): read from a field and
//! written back to its dictionaries, with appearances rebuilt.

use pagebara_annot::Color;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::da::DefaultAppearance;
use crate::form::{Change, Form, update_acroform};
use crate::model::{BorderKind, ChoiceOption, Field, FieldKind, FieldValue};
use crate::script::Native;
use crate::text::{encode, text_of};
use crate::{Error, FieldFlags, Result};

/// The standard fonts offered for new fields: resource name, base font, label.
pub const STANDARD_FONTS: [(&str, &str, &str); 4] = [
    ("Helv", "Helvetica", "Helvetica"),
    ("HeBo", "Helvetica-Bold", "Helvetica Bold"),
    ("TiRo", "Times-Roman", "Times"),
    ("Cour", "Courier", "Courier"),
];

/// Check box and radio button marks: ZapfDingbats character and label.
pub const CHECK_STYLES: [(char, &str); 6] = [
    ('4', "Check"),
    ('l', "Circle"),
    ('8', "Cross"),
    ('u', "Diamond"),
    ('n', "Square"),
    ('H', "Star"),
];

/// What a push button does when clicked (`/A`).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum ButtonAction {
    #[default]
    None,
    /// Resets every field to its default value.
    ResetForm,
    /// Opens a web address.
    Uri(String),
    /// A named action: `NextPage`, `PrevPage`, `FirstPage`, `LastPage`, `Print`, ...
    Named(String),
    /// An action we keep as it is (JavaScript, submit, ...).
    Other,
}

/// How a text field's value is shown, written as the Acrobat helper calls other readers
/// run and we run natively.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Format {
    #[default]
    None,
    /// `AFNumber_Format`: decimals, separator style (0 `1,234.56` … 4 `1'234.56`) and a
    /// currency symbol put in front.
    Number {
        decimals: u32,
        sep: u8,
        currency: String,
    },
    /// `AFDate_FormatEx` with a format such as `dd.mm.yyyy`.
    Date(String),
    /// A script we keep as it is.
    Other,
}

/// The properties of a field (and of its first widget) a form designer edits.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldProps {
    /// `/TU`: what screen readers and tooltips say.
    pub tooltip: String,
    pub required: bool,
    pub read_only: bool,
    pub no_export: bool,
    /// `/DV`: a text, an option's export value, or a button's on-state.
    pub default_value: String,
    /// The `/DA` font resource (one of [`STANDARD_FONTS`], or the form's own).
    pub font: String,
    /// 0 sizes text to fit.
    pub font_size: f64,
    pub text_color: Color,
    pub border_color: Option<Color>,
    pub background: Option<Color>,
    pub border_width: f64,
    pub border_style: BorderKind,
    /// `/Q`: 0 left, 1 centred, 2 right.
    pub align: u8,
    /// `/MK /R`: 0, 90, 180 or 270.
    pub rotation: u16,
    pub hidden: bool,
    // Text fields.
    pub multiline: bool,
    pub password: bool,
    pub comb: bool,
    pub max_len: Option<usize>,
    pub do_not_scroll: bool,
    pub do_not_spell_check: bool,
    pub format: Format,
    // Choice fields.
    pub options: Vec<ChoiceOption>,
    pub editable: bool,
    pub multi_select: bool,
    pub sort: bool,
    pub commit_on_change: bool,
    // Buttons.
    pub caption: String,
    pub action: ButtonAction,
    /// The ZapfDingbats mark of check boxes and radio buttons.
    pub check_style: char,
    pub radios_in_unison: bool,
    pub no_toggle_to_off: bool,
}

impl FieldProps {
    /// Sensible properties for a new field of `kind`.
    pub fn for_kind(kind: FieldKind) -> FieldProps {
        let mut p = FieldProps {
            tooltip: String::new(),
            required: false,
            read_only: false,
            no_export: false,
            default_value: String::new(),
            font: "Helv".into(),
            font_size: 0.0,
            text_color: Color::BLACK,
            border_color: Some(Color::Gray(0.0)),
            background: None,
            border_width: 1.0,
            border_style: BorderKind::Solid,
            align: 0,
            rotation: 0,
            hidden: false,
            multiline: false,
            password: false,
            comb: false,
            max_len: None,
            do_not_scroll: false,
            do_not_spell_check: false,
            format: Format::None,
            options: Vec::new(),
            editable: false,
            multi_select: false,
            sort: false,
            commit_on_change: false,
            caption: String::new(),
            action: ButtonAction::None,
            check_style: '4',
            radios_in_unison: false,
            no_toggle_to_off: false,
        };
        match kind {
            FieldKind::Radio => {
                p.check_style = 'l';
                p.no_toggle_to_off = true;
            }
            FieldKind::PushButton => {
                p.caption = "Button".into();
                p.background = Some(Color::Gray(0.85));
                p.border_style = BorderKind::Beveled;
                p.align = 1;
            }
            FieldKind::ComboBox | FieldKind::ListBox => {
                p.font_size = 10.0;
            }
            FieldKind::Signature => p.background = Some(Color::Rgb(0.93, 0.95, 1.0)),
            _ => {}
        }
        p
    }

    fn flags(&self, kind: FieldKind, old: FieldFlags) -> FieldFlags {
        let mut f = old
            .with(FieldFlags::READ_ONLY, self.read_only)
            .with(FieldFlags::REQUIRED, self.required)
            .with(FieldFlags::NO_EXPORT, self.no_export);
        match kind {
            FieldKind::Text => {
                f = f
                    .with(FieldFlags::MULTILINE, self.multiline)
                    .with(FieldFlags::PASSWORD, self.password)
                    .with(FieldFlags::COMB, self.comb)
                    .with(FieldFlags::DO_NOT_SCROLL, self.do_not_scroll)
                    .with(FieldFlags::DO_NOT_SPELL_CHECK, self.do_not_spell_check);
            }
            FieldKind::ComboBox | FieldKind::ListBox => {
                let combo = kind == FieldKind::ComboBox;
                f = f
                    .with(FieldFlags::COMBO, combo)
                    .with(FieldFlags::EDIT, combo && self.editable)
                    .with(FieldFlags::MULTI_SELECT, !combo && self.multi_select)
                    .with(FieldFlags::SORT, self.sort)
                    .with(FieldFlags::COMMIT_ON_SEL_CHANGE, self.commit_on_change)
                    .with(
                        FieldFlags::DO_NOT_SPELL_CHECK,
                        combo && self.do_not_spell_check,
                    );
            }
            FieldKind::Radio => {
                f = f
                    .with(FieldFlags::RADIO, true)
                    .with(FieldFlags::PUSHBUTTON, false)
                    .with(FieldFlags::RADIOS_IN_UNISON, self.radios_in_unison)
                    .with(FieldFlags::NO_TOGGLE_TO_OFF, self.no_toggle_to_off);
            }
            FieldKind::CheckBox => {
                f = f
                    .with(FieldFlags::RADIO, false)
                    .with(FieldFlags::PUSHBUTTON, false);
            }
            FieldKind::PushButton => {
                f = f
                    .with(FieldFlags::PUSHBUTTON, true)
                    .with(FieldFlags::RADIO, false);
            }
            _ => {}
        }
        f
    }
}

/// The format a field's scripts give, if one of ours.
fn format_of(f: &Field) -> Format {
    match f.scripts.format.as_ref() {
        None => Format::None,
        Some(s) => match &s.native {
            Some(Native::Date(d)) => Format::Date(d.clone()),
            Some(Native::Number(n)) => Format::Number {
                decimals: n.decimals,
                sep: n.sep,
                currency: n.currency.clone(),
            },
            _ => Format::Other,
        },
    }
}

/// What the widget's `/A` does.
pub(crate) fn action_of(doc: &Document, widget: &Dict) -> ButtonAction {
    let a = doc.get_in(widget, b"A");
    let Some(a) = a.as_dict() else {
        return ButtonAction::None;
    };
    match a.name(b"S") {
        Some(b"ResetForm") if a.get(b"Fields").is_none() => ButtonAction::ResetForm,
        Some(b"URI") => doc
            .get_in(a, b"URI")
            .as_string()
            .map_or(ButtonAction::Other, |u| {
                ButtonAction::Uri(String::from_utf8_lossy(u).into_owned())
            }),
        Some(b"Named") => a.name(b"N").map_or(ButtonAction::Other, |n| {
            ButtonAction::Named(crate::text::name_text(n))
        }),
        _ => ButtonAction::Other,
    }
}

fn action_object(a: &ButtonAction) -> Option<Dict> {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Action"));
    match a {
        ButtonAction::ResetForm => d.set(b"S", Object::name(b"ResetForm")),
        ButtonAction::Uri(u) => {
            d.set(b"S", Object::name(b"URI"));
            d.set(b"URI", Object::string(u.as_bytes()));
        }
        ButtonAction::Named(n) => {
            d.set(b"S", Object::name(b"Named"));
            d.set(b"N", Object::name(n.as_bytes()));
        }
        ButtonAction::None | ButtonAction::Other => return None,
    }
    Some(d)
}

fn js(src: &str) -> Object {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Action"));
    d.set(b"S", Object::name(b"JavaScript"));
    d.set(b"JS", encode(src));
    Object::Dict(d)
}

/// The keystroke and format scripts for `fmt`.
fn format_scripts(fmt: &Format) -> Option<(String, String)> {
    match fmt {
        Format::Number {
            decimals,
            sep,
            currency,
        } => {
            let cur = currency.replace('\\', "\\\\").replace('"', "\\\"");
            Some((
                format!("AFNumber_Keystroke({decimals}, {sep}, 0, 0, \"{cur}\", true);"),
                format!("AFNumber_Format({decimals}, {sep}, 0, 0, \"{cur}\", true);"),
            ))
        }
        Format::Date(f) => {
            let f = f.replace('\\', "\\\\").replace('"', "\\\"");
            Some((
                format!("AFDate_KeystrokeEx(\"{f}\");"),
                format!("AFDate_FormatEx(\"{f}\");"),
            ))
        }
        Format::None | Format::Other => None,
    }
}

pub(crate) fn edit_dict(doc: &mut Document, r: ObjRef, f: impl FnOnce(&Document, &mut Dict)) {
    if let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) {
        let before = d.clone();
        f(doc, &mut d);
        if d != before {
            doc.set(r, d);
        }
    }
}

/// Makes sure the form's `/DR` has font `name` (standard ones are added when missing) and
/// ZapfDingbats.
pub(crate) fn ensure_fonts(doc: &mut Document, name: &str) {
    update_acroform(doc, |doc, af| {
        let mut dr = doc.get_in(af, b"DR").as_dict().cloned().unwrap_or_default();
        let mut fonts = doc
            .get_in(&dr, b"Font")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let mut changed = false;
        for (res, base, _) in STANDARD_FONTS {
            if (res == name || res == "Helv") && !fonts.contains(res.as_bytes()) {
                let mut f = Dict::new();
                f.set(b"Type", Object::name(b"Font"));
                f.set(b"Subtype", Object::name(b"Type1"));
                f.set(b"BaseFont", Object::name(base.as_bytes()));
                f.set(b"Encoding", Object::name(b"WinAnsiEncoding"));
                fonts.set(res.as_bytes(), f);
                changed = true;
            }
        }
        if !fonts.contains(b"ZaDb") {
            fonts.set(b"ZaDb", crate::fonts::dingbats());
            changed = true;
        }
        if changed {
            // An indirect font dictionary is replaced by the merged copy.
            dr.set(b"Font", fonts);
            af.set(b"DR", dr);
        }
        if !af.contains(b"DA") {
            af.set(b"DA", Object::string(b"/Helv 0 Tf 0 g"));
        }
    });
}

impl Form {
    /// The properties of field `name`, from the field and its first widget.
    pub fn props(&self, doc: &Document, name: &str) -> Option<FieldProps> {
        let f = self.field(name)?;
        let mut p = FieldProps::for_kind(f.kind);
        let w = f.widgets.first();
        let da = w
            .and_then(|w| w.da.as_deref())
            .or(f.da.as_deref())
            .map(|d| DefaultAppearance::parse(d.as_bytes()))
            .unwrap_or_default();
        p.tooltip = f.alternate_name.clone().unwrap_or_default();
        p.required = f.flags.required();
        p.read_only = f.flags.read_only();
        p.no_export = f.flags.no_export();
        p.default_value = match &f.default_value {
            FieldValue::None => String::new(),
            FieldValue::Text(s) | FieldValue::State(s) => s.clone(),
            FieldValue::Choices(c) => c.first().cloned().unwrap_or_default(),
        };
        if let Some(font) = da.font.filter(|n| n != "ZaDb") {
            p.font = font;
        }
        p.font_size = da.size;
        p.text_color = da.color;
        p.align = w.and_then(|w| w.quadding).unwrap_or(f.quadding);
        p.multiline = f.flags.contains(FieldFlags::MULTILINE);
        p.password = f.flags.contains(FieldFlags::PASSWORD);
        p.comb = f.flags.contains(FieldFlags::COMB);
        p.max_len = f.max_len;
        p.do_not_scroll = f.flags.contains(FieldFlags::DO_NOT_SCROLL);
        p.do_not_spell_check = f.flags.contains(FieldFlags::DO_NOT_SPELL_CHECK);
        p.format = format_of(f);
        p.options = f.options.clone();
        p.editable = f.flags.contains(FieldFlags::EDIT);
        p.multi_select = f.flags.contains(FieldFlags::MULTI_SELECT);
        p.sort = f.flags.contains(FieldFlags::SORT);
        p.commit_on_change = f.flags.contains(FieldFlags::COMMIT_ON_SEL_CHANGE);
        p.radios_in_unison = f.flags.contains(FieldFlags::RADIOS_IN_UNISON);
        p.no_toggle_to_off = f.flags.contains(FieldFlags::NO_TOGGLE_TO_OFF);
        if let Some(w) = w {
            p.border_color = w.mk.border;
            p.background = w.mk.background;
            p.border_width = if w.mk.border.is_some() {
                w.border.width
            } else {
                0.0
            };
            p.border_style = w.border.kind;
            p.rotation = w.mk.rotation;
            p.hidden = w.is_hidden();
            match f.kind {
                FieldKind::CheckBox | FieldKind::Radio => {
                    if let Some(c) = w.mk.caption.as_deref().and_then(|c| c.chars().next()) {
                        p.check_style = c;
                    }
                }
                _ => p.caption = w.mk.caption.clone().unwrap_or_default(),
            }
            if let Some(d) = doc.get(w.id).and_then(|o| o.as_dict().cloned()) {
                p.action = action_of(doc, &d);
            }
        }
        Some(p)
    }

    /// What clicking push button `name` does.
    pub fn button_action(&self, doc: &Document, name: &str) -> ButtonAction {
        self.field(name)
            .and_then(|f| f.widgets.first())
            .and_then(|w| doc.get(w.id)?.as_dict().cloned())
            .map_or(ButtonAction::None, |d| action_of(doc, &d))
    }

    /// Writes `p` to field `name` and all its widgets, and rebuilds their appearances.
    pub fn set_props(&mut self, doc: &mut Document, name: &str, p: &FieldProps) -> Result<Change> {
        let mut change = self.prepare_edit(doc)?;
        let i = self.index(name)?;
        let f = self.fields[i].clone();
        if p.comb && p.max_len.is_none() && f.kind == FieldKind::Text {
            return Err(Error::Rejected {
                field: name.to_owned(),
                value: "comb".into(),
                reason: "a comb field needs a maximum length",
            });
        }
        let button = matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio);
        ensure_fonts(doc, &p.font);
        let font = if button { "ZaDb" } else { p.font.as_str() };
        let da = DefaultAppearance {
            font: Some(font.to_owned()),
            size: p.font_size.max(0.0),
            color: p.text_color,
        }
        .to_string_with(font);
        let flags = p.flags(f.kind, f.flags);
        let kind = f.kind;
        edit_dict(doc, f.id, |doc, d| {
            set_text(d, b"TU", &p.tooltip);
            if flags.0 == 0 {
                d.remove(b"Ff");
            } else {
                d.set(b"Ff", Object::Int(i64::from(flags.0)));
            }
            if kind != FieldKind::Signature {
                d.set(b"DA", Object::string(da.as_bytes()));
            }
            if p.align == 0 {
                d.remove(b"Q");
            } else {
                d.set(b"Q", Object::Int(i64::from(p.align.min(2))));
            }
            match p.max_len {
                Some(n) if kind == FieldKind::Text => d.set(b"MaxLen", Object::Int(n as i64)),
                _ => {
                    d.remove(b"MaxLen");
                }
            }
            match kind {
                FieldKind::CheckBox | FieldKind::Radio => {
                    if p.default_value.is_empty() || p.default_value == "Off" {
                        d.remove(b"DV");
                    } else {
                        d.set(b"DV", Object::name(p.default_value.as_bytes()));
                    }
                }
                FieldKind::Text | FieldKind::ComboBox | FieldKind::ListBox => {
                    if p.default_value.is_empty() {
                        d.remove(b"DV");
                    } else {
                        d.set(b"DV", encode(&p.default_value));
                    }
                }
                _ => {}
            }
            if kind.is_choice() {
                let opts: Vec<Object> = p
                    .options
                    .iter()
                    .map(|o| {
                        if o.export == o.display {
                            encode(&o.display)
                        } else {
                            Object::Array(vec![encode(&o.export), encode(&o.display)])
                        }
                    })
                    .collect();
                d.set(b"Opt", opts);
                d.remove(b"I");
            }
            if kind == FieldKind::Text && p.format != Format::Other {
                let mut aa = doc.get_in(d, b"AA").as_dict().cloned().unwrap_or_default();
                match format_scripts(&p.format) {
                    Some((k, fm)) => {
                        aa.set(b"K", js(&k));
                        aa.set(b"F", js(&fm));
                    }
                    None => {
                        // Only formats we recognise are dropped; other scripts stay.
                        for key in [&b"K"[..], b"F"] {
                            let ours = aa.get(key).is_some_and(|a| {
                                crate::script::javascript(doc, a)
                                    .and_then(|s| crate::script::parse_native(&s))
                                    .is_some_and(|n| {
                                        matches!(n, Native::Date(_) | Native::Number(_))
                                    })
                            });
                            if ours {
                                aa.remove(key);
                            }
                        }
                    }
                }
                if aa.is_empty() {
                    d.remove(b"AA");
                } else {
                    d.set(b"AA", aa);
                }
            }
        });
        // Choice values no longer among the options are dropped.
        if kind.is_choice() && !p.editable {
            let exports: Vec<&str> = p.options.iter().map(|o| o.export.as_str()).collect();
            let keep = match &f.value {
                FieldValue::Choices(c) => c.iter().all(|v| exports.contains(&v.as_str())),
                _ => true,
            };
            if !keep {
                edit_dict(doc, f.id, |_, d| {
                    d.remove(b"V");
                });
            }
        }
        for w in &f.widgets {
            edit_dict(doc, w.id, |doc, d| {
                let mut mk = doc.get_in(d, b"MK").as_dict().cloned().unwrap_or_default();
                set_color(
                    &mut mk,
                    b"BC",
                    p.border_color.filter(|_| p.border_width > 0.0),
                );
                set_color(&mut mk, b"BG", p.background);
                match kind {
                    FieldKind::CheckBox | FieldKind::Radio => {
                        mk.set(b"CA", Object::string(p.check_style.to_string().as_bytes()))
                    }
                    FieldKind::PushButton => set_text(&mut mk, b"CA", &p.caption),
                    _ => {}
                }
                if p.rotation == 0 {
                    mk.remove(b"R");
                } else {
                    mk.set(b"R", Object::Int(i64::from(p.rotation % 360)));
                }
                if mk.is_empty() {
                    d.remove(b"MK");
                } else {
                    d.set(b"MK", mk);
                }
                let mut bs = Dict::new();
                bs.set(b"W", crate::appearance::num(p.border_width.max(0.0)));
                let s: &[u8] = match p.border_style {
                    BorderKind::Solid => b"S",
                    BorderKind::Dashed => b"D",
                    BorderKind::Beveled => b"B",
                    BorderKind::Inset => b"I",
                    BorderKind::Underline => b"U",
                };
                bs.set(b"S", Object::name(s));
                if p.border_style == BorderKind::Dashed {
                    bs.set(b"D", Object::Array(vec![Object::Int(3)]));
                }
                d.set(b"BS", bs);
                d.remove(b"Border");
                if d.contains(b"DA") && kind != FieldKind::Signature {
                    d.set(b"DA", Object::string(da.as_bytes()));
                }
                if d.contains(b"Q") {
                    d.set(b"Q", Object::Int(i64::from(p.align.min(2))));
                }
                let flags = d.int(b"F").unwrap_or(4);
                let flags = if p.hidden { flags | 2 } else { flags & !2 };
                d.set(b"F", Object::Int(flags));
                if kind == FieldKind::PushButton && p.action != ButtonAction::Other {
                    match action_object(&p.action) {
                        Some(a) => d.set(b"A", a),
                        None => {
                            d.remove(b"A");
                        }
                    }
                }
            });
        }
        self.reload(doc);
        let i = self.index(name)?;
        change.merge(self.appearance_for(doc, i, true)?);
        change.fields.push(name.to_owned());
        Ok(change)
    }

    /// Sets the value a check box or radio button widget stands for (its on-state).
    pub fn set_export(
        &mut self,
        doc: &mut Document,
        widget: ObjRef,
        value: &str,
    ) -> Result<Change> {
        let mut change = self.prepare_edit(doc)?;
        let value = value.trim();
        let f = self
            .field_of_widget(widget)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(format!("{widget:?}")))?;
        if !matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio) {
            return Err(Error::WrongKind {
                field: f.name,
                expected: "check box or radio button",
            });
        }
        if value.is_empty() || value == "Off" {
            return Err(Error::Rejected {
                field: f.name,
                value: value.to_owned(),
                reason: "an export value must not be empty or Off",
            });
        }
        let Some(w) = f.widgets.iter().find(|w| w.id == widget) else {
            return Err(Error::NoSuchField(f.name));
        };
        let old = w.on_state.clone().unwrap_or_else(|| "Yes".into());
        if old == value {
            return Ok(change);
        }
        rename_state(doc, widget, &old, value);
        // The field's value and default follow when no other widget still has the old name.
        let others = f
            .widgets
            .iter()
            .any(|o| o.id != widget && o.on_state.as_deref() == Some(old.as_str()));
        if !others {
            edit_dict(doc, f.id, |_, d| {
                for k in [&b"V"[..], b"DV"] {
                    if d.get(k).and_then(text_of).as_deref() == Some(old.as_str()) {
                        d.set(k, Object::name(value.as_bytes()));
                    }
                }
                // `/Opt` export values would contradict the state names.
                d.remove(b"Opt");
            });
        }
        self.reload(doc);
        let i = self.index(&f.name)?;
        change.merge(self.appearance_for(doc, i, true)?);
        Ok(change)
    }
}

/// Renames appearance state `old` of a widget to `new` (and `/AS`).
fn rename_state(doc: &mut Document, widget: ObjRef, old: &str, new: &str) {
    edit_dict(doc, widget, |doc, d| {
        let mut ap = doc.get_in(d, b"AP").as_dict().cloned().unwrap_or_default();
        for k in [&b"N"[..], b"D"] {
            let states = doc.get_in(&ap, k);
            let Some(states) = states.as_dict().filter(|_| states.as_stream().is_none()) else {
                continue;
            };
            let mut n = Dict::new();
            for (key, v) in states.iter() {
                if crate::text::name_text(key.as_bytes()) == old {
                    n.set(new.as_bytes(), v.clone());
                } else {
                    n.set(key.as_bytes(), v.clone());
                }
            }
            ap.set(k, n);
        }
        if !ap.is_empty() {
            d.set(b"AP", ap);
        }
        if d.name(b"AS").map(crate::text::name_text).as_deref() == Some(old) {
            d.set(b"AS", Object::name(new.as_bytes()));
        }
    });
}

fn set_text(d: &mut Dict, key: &[u8], s: &str) {
    if s.is_empty() {
        d.remove(key);
    } else {
        d.set(key, encode(s));
    }
}

fn set_color(d: &mut Dict, key: &[u8], c: Option<Color>) {
    match c {
        Some(c) => d.set(key, c.to_object()),
        None => {
            d.remove(key);
        }
    }
}
