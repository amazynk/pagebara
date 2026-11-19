//! Filling: values written to the field dictionaries, then appearances regenerated and
//! calculations rerun.

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::form::{Change, Form};
use crate::model::{Field, FieldKind, FieldValue};
use crate::script::{Native, js_number, parse_number};
use crate::text::encode;
use crate::{Error, FieldFlags, Result};

/// A new value for a field, with the widget states of buttons.
enum NewValue {
    /// `None` removes `/V`.
    Plain(Option<Object>),
    Button {
        state: Option<String>,
        /// The widgets to turn on.
        on: Vec<ObjRef>,
    },
    Choice {
        value: Option<Object>,
        indices: Vec<usize>,
    },
}

fn edit_dict(doc: &mut Document, r: ObjRef, f: impl FnOnce(&mut Dict)) {
    if let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) {
        let before = d.clone();
        f(&mut d);
        if d != before {
            doc.set(r, d);
        }
    }
}

fn name_obj(s: &str) -> Object {
    Object::name(s.as_bytes())
}

impl Form {
    fn editable(&self, doc: &Document, name: &str) -> Result<usize> {
        self.check_editable(doc)?;
        let i = self.index(name)?;
        if self.fields[i].is_read_only() {
            return Err(Error::ReadOnly(name.to_owned()));
        }
        Ok(i)
    }

    /// Runs the field's native keystroke and validate scripts on `value`.
    fn validate(f: &Field, value: &str) -> Result<()> {
        for (s, reason) in [
            (&f.scripts.keystroke, "not in the expected format"),
            (&f.scripts.validate, "out of the allowed range"),
        ] {
            if let Some(n) = s.as_ref().and_then(|s| s.native.as_ref())
                && !n.accepts(value)
            {
                return Err(Error::Rejected {
                    field: f.name.clone(),
                    value: value.to_owned(),
                    reason,
                });
            }
        }
        Ok(())
    }

    /// Sets a text field (or an editable combo box) to `value`. Line breaks in a
    /// single-line field become spaces.
    pub fn set_text(&mut self, doc: &mut Document, name: &str, value: &str) -> Result<Change> {
        let i = self.editable(doc, name)?;
        let f = &self.fields[i];
        if f.is_editable_combo() {
            return self.select_choices(doc, name, &[value]);
        }
        if f.kind != FieldKind::Text {
            return Err(Error::WrongKind {
                field: name.to_owned(),
                expected: "text",
            });
        }
        let value = if f.is_multiline() {
            value.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            value.replace("\r\n", " ").replace(['\r', '\n'], " ")
        };
        if let Some(max) = f.max_len
            && value.chars().count() > max
        {
            return Err(Error::TooLong {
                field: name.to_owned(),
                max,
            });
        }
        Self::validate(f, &value)?;
        self.commit(doc, i, NewValue::Plain(Some(encode(&value))))
    }

    /// Turns a check box on (its first widget's on-state) or off.
    pub fn set_checked(&mut self, doc: &mut Document, name: &str, on: bool) -> Result<Change> {
        let i = self.editable(doc, name)?;
        let f = &self.fields[i];
        if !matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio) {
            return Err(Error::WrongKind {
                field: name.to_owned(),
                expected: "check box",
            });
        }
        if !on {
            return self.commit(
                doc,
                i,
                NewValue::Button {
                    state: None,
                    on: Vec::new(),
                },
            );
        }
        let state = f
            .widgets
            .iter()
            .find_map(|w| w.on_state.clone())
            .unwrap_or_else(|| "Yes".to_owned());
        let on = self.widgets_for(i, &state, None);
        self.commit(
            doc,
            i,
            NewValue::Button {
                state: Some(state),
                on,
            },
        )
    }

    /// Widgets that turn on for `state`: all with that state when in unison (or for check
    /// boxes), else `preferred` or the first.
    fn widgets_for(&self, i: usize, state: &str, preferred: Option<ObjRef>) -> Vec<ObjRef> {
        let f = &self.fields[i];
        let matching: Vec<ObjRef> = f
            .widgets
            .iter()
            .filter(|w| w.on_state.as_deref().unwrap_or("Yes") == state)
            .map(|w| w.id)
            .collect();
        if f.kind == FieldKind::CheckBox || f.flags.contains(FieldFlags::RADIOS_IN_UNISON) {
            return matching;
        }
        match preferred {
            Some(p) if matching.contains(&p) => vec![p],
            _ => matching.into_iter().take(1).collect(),
        }
    }

    /// Selects the radio button (or check box widget) with export value or on-state name
    /// `value`; `None` turns the group off.
    pub fn select_radio(
        &mut self,
        doc: &mut Document,
        name: &str,
        value: Option<&str>,
    ) -> Result<Change> {
        let i = self.editable(doc, name)?;
        let f = &self.fields[i];
        if !matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio) {
            return Err(Error::WrongKind {
                field: name.to_owned(),
                expected: "radio button",
            });
        }
        let Some(value) = value else {
            return self.commit(
                doc,
                i,
                NewValue::Button {
                    state: None,
                    on: Vec::new(),
                },
            );
        };
        let w = f
            .widgets
            .iter()
            .find(|w| w.export.as_deref() == Some(value))
            .or_else(|| {
                f.widgets
                    .iter()
                    .find(|w| w.on_state.as_deref() == Some(value))
            })
            .ok_or_else(|| Error::NotAnOption {
                field: name.to_owned(),
                value: value.to_owned(),
            })?;
        let state = w.on_state.clone().unwrap_or_else(|| value.to_owned());
        let on = self.widgets_for(i, &state, Some(w.id));
        self.commit(
            doc,
            i,
            NewValue::Button {
                state: Some(state),
                on,
            },
        )
    }

    /// A click on a check box or radio button widget: check boxes toggle; radio buttons
    /// turn on, or off again unless the field has NoToggleToOff.
    pub fn click(&mut self, doc: &mut Document, widget: ObjRef) -> Result<Change> {
        let name = self
            .field_of_widget(widget)
            .map(|f| f.name.clone())
            .ok_or_else(|| Error::NoSuchField(format!("{widget:?}")))?;
        let i = self.editable(doc, &name)?;
        let f = &self.fields[i];
        let Some(w) = f.widgets.iter().find(|w| w.id == widget) else {
            return Err(Error::NoSuchField(name));
        };
        let state = w.on_state.clone().unwrap_or_else(|| "Yes".to_owned());
        let is_on = f.value == FieldValue::State(state.clone()) && w.is_on();
        let new = match (f.kind, is_on) {
            (FieldKind::Radio, true) if f.flags.contains(FieldFlags::NO_TOGGLE_TO_OFF) => {
                return Ok(Change::default());
            }
            (FieldKind::CheckBox | FieldKind::Radio, true) => NewValue::Button {
                state: None,
                on: Vec::new(),
            },
            (FieldKind::CheckBox | FieldKind::Radio, false) => NewValue::Button {
                on: self.widgets_for(i, &state, Some(widget)),
                state: Some(state),
            },
            _ => {
                return Err(Error::WrongKind {
                    field: name,
                    expected: "check box or radio button",
                });
            }
        };
        self.commit(doc, i, new)
    }

    /// Selects one option (by export value or displayed text) of a combo or list box; an
    /// editable combo box also takes any text.
    pub fn select_choice(&mut self, doc: &mut Document, name: &str, value: &str) -> Result<Change> {
        self.select_choices(doc, name, &[value])
    }

    /// Selects options of a choice field: several only in a multi-select list box. An empty
    /// list clears the selection.
    pub fn select_choices(
        &mut self,
        doc: &mut Document,
        name: &str,
        values: &[&str],
    ) -> Result<Change> {
        let i = self.editable(doc, name)?;
        let f = &self.fields[i];
        if !f.kind.is_choice() {
            return Err(Error::WrongKind {
                field: name.to_owned(),
                expected: "choice",
            });
        }
        if values.len() > 1 && !f.is_multi_select() {
            return Err(Error::NotMultiSelect(name.to_owned()));
        }
        let mut exports = Vec::new();
        let mut indices = Vec::new();
        for &v in values {
            let found = f
                .options
                .iter()
                .enumerate()
                .find(|(i, o)| o.export == v && !indices.contains(i))
                .or_else(|| {
                    f.options
                        .iter()
                        .enumerate()
                        .find(|(i, o)| o.display == v && !indices.contains(i))
                });
            match found {
                Some((idx, o)) => {
                    exports.push(o.export.clone());
                    indices.push(idx);
                }
                None if f.is_editable_combo() => {
                    if let Some(max) = f.max_len
                        && v.chars().count() > max
                    {
                        return Err(Error::TooLong {
                            field: name.to_owned(),
                            max,
                        });
                    }
                    exports.push(v.to_owned());
                }
                None => {
                    return Err(Error::NotAnOption {
                        field: name.to_owned(),
                        value: v.to_owned(),
                    });
                }
            }
        }
        if let [one] = exports.as_slice() {
            Self::validate(f, one)?;
        }
        indices.sort_unstable();
        let value = match exports.len() {
            0 => None,
            1 if !f.is_multi_select() => Some(encode(&exports[0])),
            _ => Some(Object::Array(exports.iter().map(|e| encode(e)).collect())),
        };
        self.commit(doc, i, NewValue::Choice { value, indices })
    }

    /// Empties a field (text, selection, or buttons off).
    pub fn clear(&mut self, doc: &mut Document, name: &str) -> Result<Change> {
        let i = self.editable(doc, name)?;
        let new = match self.fields[i].kind {
            FieldKind::CheckBox | FieldKind::Radio => NewValue::Button {
                state: None,
                on: Vec::new(),
            },
            FieldKind::ComboBox | FieldKind::ListBox => NewValue::Choice {
                value: None,
                indices: Vec::new(),
            },
            FieldKind::Text => NewValue::Plain(None),
            _ => return Ok(Change::default()),
        };
        self.commit(doc, i, new)
    }

    /// Resets fields to their defaults (`/DV`), as a reset button does: all fields, or
    /// those named (a name also covers the fields below it).
    pub fn reset(&mut self, doc: &mut Document, names: Option<&[&str]>) -> Result<Change> {
        self.check_editable(doc)?;
        let selected: Vec<usize> = (0..self.fields.len())
            .filter(|&i| {
                let n = &self.fields[i].name;
                names.is_none_or(|list| {
                    list.iter()
                        .any(|l| n == l || n.strip_prefix(l).is_some_and(|r| r.starts_with('.')))
                })
            })
            .collect();
        if let Some(list) = names
            && let Some(missing) = list.iter().find(|l| {
                !self
                    .fields
                    .iter()
                    .any(|f| f.name == **l || f.name.starts_with(&format!("{l}.")))
            })
        {
            return Err(Error::NoSuchField((*missing).to_owned()));
        }
        let mut change = Change::default();
        for i in selected {
            let f = &self.fields[i];
            let new = match (f.kind, &f.default_value) {
                (FieldKind::Signature | FieldKind::PushButton | FieldKind::Unknown, _) => {
                    continue;
                }
                (FieldKind::CheckBox | FieldKind::Radio, FieldValue::State(s)) => {
                    NewValue::Button {
                        state: Some(s.clone()),
                        on: self.widgets_for(i, s, None),
                    }
                }
                (FieldKind::CheckBox | FieldKind::Radio, _) => NewValue::Button {
                    state: None,
                    on: Vec::new(),
                },
                (FieldKind::ComboBox | FieldKind::ListBox, dv) => {
                    let dv_obj = doc
                        .get(f.id)
                        .and_then(|d| d.as_dict().and_then(|d| d.get(b"DV").cloned()));
                    let value = match dv {
                        FieldValue::None => None,
                        _ => dv_obj.or_else(|| {
                            f.chain[1..]
                                .iter()
                                .find_map(|&p| doc.get(p)?.as_dict()?.get(b"DV").cloned())
                        }),
                    };
                    NewValue::Choice {
                        value,
                        indices: Vec::new(),
                    }
                }
                (_, FieldValue::Text(t)) => NewValue::Plain(Some(encode(t))),
                _ => NewValue::Plain(None),
            };
            change.merge(self.commit_one(doc, i, new)?);
        }
        change.merge(self.recalculate_inner(doc)?);
        Ok(change)
    }

    fn commit(&mut self, doc: &mut Document, i: usize, new: NewValue) -> Result<Change> {
        let mut change = self.commit_one(doc, i, new)?;
        change.merge(self.recalculate_inner(doc)?);
        Ok(change)
    }

    /// Writes the value of field `i` and regenerates its appearances.
    fn commit_one(&mut self, doc: &mut Document, i: usize, new: NewValue) -> Result<Change> {
        let mut change = self.prepare(doc)?;
        let f = self.fields[i].clone();
        let before = f.value.clone();
        let before_states: Vec<Option<String>> =
            f.widgets.iter().map(|w| w.state.clone()).collect();
        let rich = f.flags.contains(FieldFlags::RICH_TEXT);
        let set_v = |d: &mut Dict, v: Option<Object>| {
            match v {
                Some(v) => d.set(b"V", v),
                None => {
                    d.remove(b"V");
                }
            }
            // A rich text value would contradict the plain one.
            if rich {
                d.remove(b"RV");
            }
        };
        match new {
            NewValue::Plain(v) => edit_dict(doc, f.id, |d| set_v(d, v)),
            NewValue::Choice { value, indices } => edit_dict(doc, f.id, |d| {
                set_v(d, value);
                if indices.len() > 1 || (f.is_multi_select() && !indices.is_empty()) {
                    d.set(
                        b"I",
                        Object::Array(indices.iter().map(|&i| Object::Int(i as i64)).collect()),
                    );
                } else {
                    d.remove(b"I");
                }
            }),
            NewValue::Button { state, on } => {
                let v = name_obj(state.as_deref().unwrap_or("Off"));
                edit_dict(doc, f.id, |d| d.set(b"V", v));
                for w in &f.widgets {
                    let s = match &state {
                        Some(s) if on.contains(&w.id) => s.as_str(),
                        _ => "Off",
                    };
                    edit_dict(doc, w.id, |d| d.set(b"AS", name_obj(s)));
                }
            }
        }
        self.reload_field(doc, i);
        let f = &self.fields[i];
        let states: Vec<Option<String>> = f.widgets.iter().map(|w| w.state.clone()).collect();
        if f.value != before || states != before_states {
            change.fields.push(f.name.clone());
        }
        // Button appearances already exist for both states; others show the new value.
        let regenerate = !f.kind.is_button() || f.widgets.iter().any(|w| !w.has_appearance);
        if regenerate {
            change.merge(self.appearance_for(doc, i, false)?);
        } else if states != before_states {
            change.widgets.extend(f.widgets.iter().map(|w| w.id));
        }
        Ok(change)
    }

    /// Reruns the native calculation scripts, in `/CO` order.
    pub fn recalculate(&mut self, doc: &mut Document) -> Result<Change> {
        let mut change = self.prepare(doc)?;
        change.merge(self.recalculate_inner(doc)?);
        Ok(change)
    }

    fn recalculate_inner(&mut self, doc: &mut Document) -> Result<Change> {
        let mut order: Vec<usize> = self
            .calculation_order
            .iter()
            .filter_map(|r| self.fields.iter().position(|f| f.id == *r))
            .collect();
        for (i, f) in self.fields.iter().enumerate() {
            if f.scripts.calculate.is_some() && !order.contains(&i) {
                order.push(i);
            }
        }
        let mut change = Change::default();
        for i in order {
            let Some(Native::Calculate { op, fields }) = self.fields[i]
                .scripts
                .calculate
                .as_ref()
                .and_then(|s| s.native.clone())
            else {
                continue;
            };
            let values: Vec<f64> = self
                .fields
                .iter()
                .filter(|f| {
                    fields.iter().any(|n| {
                        f.name == *n
                            || f.name
                                .strip_prefix(n.as_str())
                                .is_some_and(|r| r.starts_with('.'))
                    })
                })
                .map(|f| {
                    let sep = number_sep(f);
                    let text = match &f.value {
                        FieldValue::State(_) => f.export_value().unwrap_or_default(),
                        v => v.display(),
                    };
                    parse_number(&text, sep).unwrap_or(0.0)
                })
                .collect();
            let result = js_number(op.apply(&values));
            if self.fields[i].value == FieldValue::Text(result.clone()) {
                continue;
            }
            let new = NewValue::Plain(Some(encode(&result)));
            change.merge(self.commit_one(doc, i, new)?);
        }
        Ok(change)
    }

    /// The text a field shows: its value through a native format script.
    pub fn display_text(&self, field: &Field) -> crate::Formatted {
        let value = match &field.value {
            FieldValue::Choices(c) if field.kind == FieldKind::ComboBox => {
                let v = c.first().cloned().unwrap_or_default();
                field
                    .options
                    .iter()
                    .find(|o| o.export == v)
                    .map_or(v, |o| o.display.clone())
            }
            v => v.display(),
        };
        field
            .scripts
            .format
            .as_ref()
            .and_then(|s| s.native.as_ref())
            .and_then(|n| n.format(&value))
            .unwrap_or(crate::Formatted {
                text: value,
                color: None,
            })
    }
}

/// The separator style numbers in a field are typed with.
fn number_sep(f: &Field) -> u8 {
    [&f.scripts.format, &f.scripts.keystroke]
        .into_iter()
        .flatten()
        .find_map(|s| match &s.native {
            Some(Native::Number(n)) => Some(n.sep),
            Some(Native::Percent { sep, .. }) => Some(*sep),
            _ => None,
        })
        .unwrap_or(0)
}
