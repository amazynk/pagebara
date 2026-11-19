//! The right panel (tool options, the selected annotation and its comment thread) and the
//! comment list in the sidebar.

use std::rc::Rc;

use pagebara_annot::Kind;
use pagebara_app_core::annots::{self, CommentEntry};
use pagebara_i18n::tr;
use slint::{ComponentHandle, ModelRc, VecModel};

use super::input::{colour, colour_index, is_signature};
use super::{App, Sel, State};
use crate::{AppState, AppWindow, CommentRow, ReplyRow, ViewState};

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_comment_clicked(move |i| a.act(move |app, s, ui| app.comment_clicked(s, ui, i as usize)));
    let a = app.clone();
    st.on_selection_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.selection_action(s, ui, &what))
    });
    let a = app.clone();
    st.on_reply(move |text| {
        let text = text.to_string();
        a.act(move |app, s, ui| app.reply(s, ui, &text))
    });
    let a = app.clone();
    st.on_contents_edited(move |text| {
        let text = text.to_string();
        a.act(move |app, s, ui| app.contents_edited(s, ui, &text))
    });
}

/// Fills the properties of the selected annotation into the panel.
pub fn sync_selection(s: &mut State, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let found = s.tab().and_then(|t| {
        let sel = t.selected?;
        let a = t.annot(sel)?;
        let all = t.annots(sel.page);
        Some((sel, a, all, t.session.page_label(sel.page)))
    });
    super::redaction::sync_selection(s, ui);
    let Some((sel, a, all, label)) = found else {
        st.set_has_selection(false);
        mark_comment(s, ui, None);
        return;
    };
    let entry = annots::entry(sel.page, &a, &all);
    st.set_has_selection(true);
    st.set_sel_kind(annots::kind_label(&a).into());
    st.set_sel_page(tr!("Page {}", label).into());
    st.set_sel_author(a.author.clone().unwrap_or_default().into());
    st.set_sel_when(
        entry
            .as_ref()
            .map(|e| e.date.clone())
            .unwrap_or_default()
            .into(),
    );
    st.set_sel_contents(a.contents.clone().unwrap_or_default().into());
    let replies: Vec<ReplyRow> = entry
        .map(|e| {
            e.replies
                .into_iter()
                .map(|r| ReplyRow {
                    author: r.author.into(),
                    when: r.date.into(),
                    body: r.contents.into(),
                })
                .collect()
        })
        .unwrap_or_default();
    st.set_sel_replies(ModelRc::new(VecModel::from(replies)));
    let c = match &a.kind {
        Kind::FreeText { text_color, .. } => Some(*text_color),
        _ => a.color,
    };
    st.set_sel_colour(colour_index(c));
    let has_width = matches!(
        a.kind,
        Kind::Ink { .. }
            | Kind::Square { .. }
            | Kind::Circle { .. }
            | Kind::Line { .. }
            | Kind::PolyLine { .. }
            | Kind::Polygon { .. }
    );
    st.set_sel_has_width(has_width);
    st.set_sel_width(a.border.width as f32);
    match a.kind {
        Kind::FreeText { font_size, .. } => {
            st.set_sel_has_font(true);
            st.set_sel_font_size(font_size as f32);
        }
        _ => st.set_sel_has_font(false),
    }
    st.set_sel_is_signature(is_signature(&a));
    st.set_sel_is_note(matches!(a.kind, Kind::Text { .. }));
    mark_comment(s, ui, Some(sel));
}

fn mark_comment(s: &State, ui: &AppWindow, sel: Option<Sel>) {
    let Some(t) = s.tab_ref() else { return };
    let rows = comment_rows(&t.comments, sel);
    ui.global::<AppState>()
        .set_comments(ModelRc::new(VecModel::from(rows)));
}

fn comment_rows(list: &[CommentEntry], sel: Option<Sel>) -> Vec<CommentRow> {
    list.iter()
        .enumerate()
        .map(|(i, c)| CommentRow {
            index: i as i32,
            page: c.page as i32 + 1,
            kind: c.kind.as_str().into(),
            author: c.author.as_str().into(),
            when: c.date.as_str().into(),
            // A highlight without a comment shows the words it marks.
            // (on one line: the marked text keeps the page's line breaks).
            body: if c.contents.trim().is_empty() && !c.quote.trim().is_empty() {
                format!(
                    "“{}”",
                    c.quote.split_whitespace().collect::<Vec<_>>().join(" ")
                )
                .into()
            } else {
                c.contents.as_str().into()
            },
            replies: c.replies.len() as i32,
            selected: sel.is_some_and(|s| s.id == c.id),
        })
        .collect()
}

/// Reads every page's annotations for the comment list, on a worker thread.
pub fn refresh_comments(_app: &Rc<App>, s: &mut State, _ui: &AppWindow) {
    let Some(t) = s.tab().filter(|t| !t.held) else {
        return;
    };
    let rev = t.session.rev();
    if t.comments_rev == Some(rev) {
        return;
    }
    t.comments_rev = Some(rev);
    let uid = t.uid;
    let doc = t.session.doc().clone();
    let tree = t.session.tree.clone();
    pagebara_thread::spawn("comments", move || {
        let list = annots::comments(&doc, &tree);
        super::post_act(move |_app, s, ui| {
            let Some(i) = s.tab_index(uid) else { return };
            if s.tabs[i].session.rev() != rev {
                return;
            }
            s.tabs[i].comments = list;
            if i == s.active {
                let sel = s.tabs[i].selected;
                mark_comment(s, ui, sel);
            }
        });
    });
}

impl App {
    /// The selection (annotation or text) changed: panel, comment list and overlay.
    pub fn selection_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        ui.global::<AppState>().set_text_warning("".into());
        sync_selection(s, ui);
        self.overlay_changed(s, ui);
    }

    pub fn tool_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        s.gesture = super::Gesture::None;
        // The last tool's hint (the new one may give its own).
        self.clear_message(s, ui);
        self.editor_done(s, ui, true);
        self.field_done(s, ui, true);
        self.edit_tool_changed(s, ui);
        self.prep_tool_changed(s, ui);
        let tool = ui.global::<AppState>().get_tool();
        self.show_tool(s, ui);
        // Text selected first, then Highlight (or Redact): the selection is marked.
        if (tool == 1 || tool == 6) && s.tab_ref().is_some_and(|t| t.text_sel.is_some()) {
            self.markup_selection(s, ui, None);
        }
        if let Some(t) = s.tab() {
            if tool != 0 {
                t.text_sel = None;
            }
            if tool != 0 && tool != 4 {
                t.selected = None;
            }
        }
        if tool == 6 {
            super::redaction::count_marks(s, ui);
        }
        super::forms::sync_fields(s, ui);
        self.selection_changed(s, ui);
    }

    /// Updates the controls without applying a tool to the current selection.
    pub(super) fn show_tool(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let tool = ui.global::<AppState>().get_tool();
        if let Some(&c) = s.tool_colours.get(tool.max(0) as usize)
            && (1..=3).contains(&tool)
        {
            ui.global::<AppState>().set_colour(c);
        }
        ui.global::<ViewState>().set_cursor(match tool {
            1 => 1,
            2..=4 => 3,
            _ => 0,
        });
        if tool == 4 {
            self.sign_panel_refresh(s, ui);
        }
    }

    pub fn option_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        match what {
            "flatten" => {
                s.settings.flatten_signatures = ui.global::<AppState>().get_flatten_signatures();
                self.save_settings(s);
            }
            "flatten-forms" => self.flatten_forms_changed(s, ui),
            "colour" => {
                let st = ui.global::<AppState>();
                if let Some(c) = s.tool_colours.get_mut(st.get_tool().max(0) as usize) {
                    *c = st.get_colour();
                }
            }
            _ => {}
        }
        self.overlay_changed(s, ui);
    }

    fn comment_clicked(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        let Some(t) = s.tab() else { return };
        let Some(c) = t.comments.get(i).cloned() else {
            return;
        };
        let sel = Sel {
            page: c.page,
            id: c.id,
        };
        let Some(a) = t.annot(sel) else { return };
        t.selected = Some(sel);
        t.text_sel = None;
        let g = t.session.geoms[c.page];
        let top = g.rect_to_view([a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1])[1];
        self.jump(s, ui, c.page, Some((top - 40.0).max(0.0)));
        self.selection_changed(s, ui);
    }

    pub fn selection_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        let Some(t) = s.tab() else { return };
        let Some(sel) = t.selected else { return };
        let Some(mut a) = t.annot(sel) else { return };
        let Some(page) = t.session.page_ref(sel.page) else {
            return;
        };
        if !self.may_annotate(s, ui) {
            return;
        }
        let label = annots::kind_label(&a).to_lowercase();
        let (cmd, value) = what.split_once(':').unwrap_or((what, ""));
        let v: f64 = value.parse().unwrap_or(0.0);
        match cmd {
            "delete" => {
                self.edit(s, ui, &tr!("Delete {}", label), |d| {
                    Ok(pagebara_annot::remove(d, page, sel.id)?)
                });
                if let Some(t) = s.tab() {
                    t.selected = None;
                }
                self.selection_changed(s, ui);
            }
            "flatten" => {
                self.edit(s, ui, &tr!("Flatten {}", label), |d| {
                    Ok(pagebara_annot::flatten(d, page, sel.id)?)
                });
                if let Some(t) = s.tab() {
                    t.selected = None;
                }
                self.selection_changed(s, ui);
                self.message(s, &tr!("Flattened: it is now part of the page."));
            }
            "colour" => {
                let c = colour(v as i32);
                match &mut a.kind {
                    Kind::FreeText { text_color, .. } => *text_color = c,
                    _ => a.set_color(Some(c)),
                }
                self.update_annotation(s, ui, sel, a, &tr!("Change colour"));
            }
            "width" if v > 0.0 => {
                a.border.width = v;
                self.update_annotation(s, ui, sel, a, &tr!("Change width"));
            }
            "font" if v > 0.0 => {
                if let Kind::FreeText { font_size, .. } = &mut a.kind {
                    *font_size = v;
                }
                self.update_annotation(s, ui, sel, a, &tr!("Change font size"));
            }
            _ => {}
        }
    }

    pub(super) fn contents_edited(self: &Rc<Self>, s: &mut State, ui: &AppWindow, text: &str) {
        let Some(t) = s.tab() else { return };
        let Some(sel) = t.selected else { return };
        let Some(mut a) = t.annot(sel) else { return };
        if a.contents.as_deref().unwrap_or("") == text {
            return;
        }
        a.contents = (!text.is_empty()).then(|| text.to_owned());
        a.modified = None;
        let missing = pagebara_annot::unsupported_chars(&pagebara_annot::Helvetica, text);
        self.update_annotation(s, ui, sel, a, &tr!("Edit comment"));
        if !missing.is_empty()
            && matches!(
                s.tab().and_then(|t| t.annot(sel)).map(|a| a.kind),
                Some(Kind::FreeText { .. })
            )
        {
            let chars: String = missing.iter().take(8).collect();
            ui.global::<AppState>().set_text_warning(
                tr!(
                    "The built-in font cannot show “{}”; other apps may draw boxes instead.",
                    chars
                )
                .into(),
            );
        }
    }

    fn reply(self: &Rc<Self>, s: &mut State, ui: &AppWindow, text: &str) {
        let author = ui.global::<AppState>().get_author().to_string();
        let Some(t) = s.tab() else { return };
        let Some(sel) = t.selected else { return };
        let Some(page) = t.session.page_ref(sel.page) else {
            return;
        };
        if !self.may_annotate(s, ui) {
            return;
        }
        let who = if author.is_empty() {
            // TRANSLATORS: the author of a reply when no name is set in the settings
            tr!("Me")
        } else {
            author
        };
        self.edit(s, ui, &tr!("Reply"), |d| {
            Ok(pagebara_annot::reply(d, page, sel.id, &who, text)?)
        });
        if let Some(t) = s.tab() {
            t.selected = Some(sel);
        }
        self.selection_changed(s, ui);
    }
}
