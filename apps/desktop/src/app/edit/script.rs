//! `--script` queries and commands of the Edit tool.
//!
//! Queries: `edit.selected` (the selection described), `edit.count`, `edit.kinds`,
//! `edit.bbox` (page space, rounded), `edit.fill` (#rrggbb), `edit.opacity`, `edit.order`
//! (painting order of the first selected object, 1-based), `edit.mirrored` (an image drawn
//! mirrored), `edit.objects` (objects on the
//! current page), `edit.text-editing`, `edit.text`, `edit.caret`, `edit.preview` (the laid-out
//! lines of the paragraph being edited), `edit.cropping`, `edit.image` (pixel size),
//! `edit.lines`, `edit.overflow`, `edit.style` (bold, italic, size, fill and font family of the
//! selected text being edited), `edit.font-size` (of the selected text object),
//! `edit.preview-scale` (device pixels per
//! point the text being edited or the dragged objects are shown at), `edit.preview-zoom`
//! (the rendered preview's zoom percentage on this display), `edit.preview-sharp`
//! (the preview matches the current view scale), `object.kinds:P`, `object.bbox:P,N`,
//! `object.fill:P,N`, `object.fonts:P,N` (font names of a text object's runs, with any
//! render mode but fill).
//!
//! Commands: `edit ACTION [VALUE]` (the panel's actions: fill, stroke, opacity, font-size,
//! front, back, forward, backward, duplicate, delete, crop, crop-apply, edit-text,
//! text-done, select-all, add-text-centre...), `edit-select P N...` (objects by painting
//! order), `edit-crop X Y W H` (the crop frame, view space), `edit-replace-image PATH`,
//! `edit-add-image PATH`, `edit-extract-image PATH`, `edit-preedit TEXT` (an input
//! method's composition at the caret).

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_app_core::object_edit::{self as oe, Handle};
use pagebara_app_core::view::PX_PER_PT;
use pagebara_edit::{Kind, PageObjects};
use slint::ComponentHandle;

use super::super::{App, State, pick};
use super::{TabEdit, view_rect};
use crate::{AppWindow, ViewState};

fn hex(c: Option<[f32; 3]>) -> String {
    c.map(|c| {
        let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        format!("#{r:02x}{g:02x}{b:02x}")
    })
    .unwrap_or_default()
}

fn kind(k: Kind) -> &'static str {
    match k {
        Kind::Text => "text",
        Kind::Image => "image",
        Kind::Path => "path",
        Kind::Form => "form",
        Kind::Shading => "shading",
    }
}

fn bbox(r: &pagebara_edit::Rect) -> String {
    format!("{:.0},{:.0},{:.0},{:.0}", r.x0, r.y0, r.x1, r.y1)
}

fn nth(objs: &PageObjects, n: usize) -> Result<&pagebara_edit::PageObject, String> {
    objs.top_level()
        .nth(n.saturating_sub(1))
        .ok_or_else(|| format!("no object {n}"))
}

pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let (k, arg) = key.split_once(':').unwrap_or((key, ""));
    if !k.starts_with("edit.") && !k.starts_with("object.") {
        return None;
    }
    let Some(t) = s.tab() else {
        return Some(Err("no document is open".into()));
    };
    if let Some(what) = k.strip_prefix("object.") {
        let mut parts = arg
            .split(',')
            .map(|v| v.trim().parse::<usize>().unwrap_or(1));
        let page = parts.next().unwrap_or(1).saturating_sub(1);
        let n = parts.next().unwrap_or(1);
        let Some(objs) = TabEdit::objs(t, page) else {
            return Some(Err(format!("no page {}", page + 1)));
        };
        return Some(match what {
            "kinds" => Ok(objs
                .top_level()
                .map(|o| kind(o.kind))
                .collect::<Vec<_>>()
                .join(",")),
            "bbox" => nth(&objs, n).map(|o| bbox(&o.bbox)),
            "fill" => nth(&objs, n).map(|o| hex(super::actions::fill_of(&objs, o).0)),
            "fonts" => nth(&objs, n).map(|o| {
                let mut names: Vec<String> = o
                    .text()
                    .into_iter()
                    .flat_map(|t| t.runs())
                    .filter_map(|r| objs.run(r))
                    .map(|r| match r.render_mode {
                        0 => oe::run_look(r).font,
                        m => format!("{} (Tr {m})", oe::run_look(r).font),
                    })
                    .collect();
                names.dedup();
                names.join(",")
            }),
            _ => Err(format!("unknown property {k}")),
        });
    }
    let te = t.edit.text.as_ref();
    let simple = match k {
        "edit.text-editing" => Some(te.is_some().to_string()),
        "edit.text" => Some(te.map(|e| e.text()).unwrap_or_default()),
        "edit.caret" => Some(te.map_or(String::new(), |e| {
            let (a, c) = e.selection();
            format!("{a},{c}")
        })),
        "edit.preview" => Some(
            t.edit
                .text_view
                .as_ref()
                .map_or(String::new(), |v| v.lines.join("\n")),
        ),
        "edit.cover" => Some(
            t.edit
                .text_view
                .as_ref()
                .and_then(|v| v.cover)
                .map(|r| bbox(&r))
                .unwrap_or_default(),
        ),
        "edit.lines" => Some(
            t.edit
                .text_view
                .as_ref()
                .map_or(0, |v| v.lines.len())
                .to_string(),
        ),
        "edit.overflow" => Some(
            t.edit
                .text_view
                .as_ref()
                .map_or(0.0, |v| v.overflow)
                .round()
                .to_string(),
        ),
        "edit.style" => {
            let session = &t.session;
            let st = t.edit.text.as_mut().map(|e| e.style(session));
            Some(st.map_or(String::new(), |st| {
                let tri = |v: Option<bool>| v.map_or("mixed".to_string(), |b| b.to_string());
                let fill = if st.fill.is_some() {
                    hex(st.fill)
                } else {
                    "mixed".to_string()
                };
                format!(
                    "bold={} italic={} size={} fill={fill} family={}",
                    tri(st.bold),
                    tri(st.italic),
                    st.size.map_or("mixed".to_string(), |v| format!("{v:.0}")),
                    st.family.as_deref().unwrap_or("mixed")
                )
            }))
        }
        "edit.cropping" => Some(t.edit.crop.is_some().to_string()),
        "edit.preview-scale" => Some(
            super::preview_made(t)
                .map(|v| format!("{v:.2}"))
                .unwrap_or_default(),
        ),
        "edit.preview-zoom" => Some(
            super::preview_made(t)
                .map(|scale| {
                    let pixels_per_point = PX_PER_PT * f64::from(ui.window().scale_factor());
                    format!("{:.0}", 100.0 * f64::from(scale) / pixels_per_point)
                })
                .unwrap_or_default(),
        ),
        "edit.preview-sharp" => {
            Some((super::preview_made(t) == Some(super::view_scale(t))).to_string())
        }
        "edit.objects" => {
            let page = t.view.current_page;
            Some(
                TabEdit::objs(t, page)
                    .map_or(0, |o| o.top_level().count())
                    .to_string(),
            )
        }
        _ => None,
    };
    if let Some(v) = simple {
        return Some(Ok(v));
    }
    let Some((sel, objs)) = TabEdit::current(t) else {
        return Some(Ok(match k {
            "edit.count" => "0".into(),
            _ => String::new(),
        }));
    };
    let first = sel.objects(&objs).next();
    Some(match k {
        "edit.selected" => Ok(oe::describe_selection(&objs, &sel)),
        "edit.count" => Ok(sel.ids.len().to_string()),
        "edit.kinds" => Ok(sel
            .objects(&objs)
            .map(|o| kind(o.kind))
            .collect::<Vec<_>>()
            .join(",")),
        "edit.bbox" => Ok(sel.bbox(&objs).map(|b| bbox(&b)).unwrap_or_default()),
        "edit.fill" => {
            Ok(first.map_or(String::new(), |o| hex(super::actions::fill_of(&objs, o).0)))
        }
        "edit.font-size" => Ok(sel
            .single_text(&objs)
            .and_then(|t| oe::text_look(&objs, t))
            .map_or(String::new(), |l| format!("{:.0}", l.size))),
        "edit.opacity" => Ok(first.map_or(String::new(), |o| {
            format!("{:.0}", super::actions::fill_of(&objs, o).1 * 100.0)
        })),
        "edit.mirrored" => Ok(first.is_some_and(oe::mirrored).to_string()),
        "edit.order" => Ok(first
            .and_then(|o| objs.top_level().position(|p| p.id == o.id))
            .map_or(String::new(), |i| (i + 1).to_string())),
        // Line width in points, "dashed" when it is.
        "edit.line" => Ok(first.and_then(|o| o.path()).map_or(String::new(), |p| {
            let w = p.line_width * oe::scale_of(&p.ctm);
            format!("{w:.1}{}", if p.dashed { " dashed" } else { "" })
        })),
        "edit.image" => Ok(sel
            .single_image(&objs)
            .and_then(|o| o.image())
            .map_or(String::new(), |i| format!("{}x{}", i.width, i.height))),
        _ => Err(format!("unknown property {k}")),
    })
}

pub fn call(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    args: &[String],
) -> Option<Result<(), String>> {
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    match cmd {
        "edit" => app.edit_action(s, ui, arg(0), arg(1)),
        "edit-select" => {
            let page = arg(0).parse::<usize>().unwrap_or(1).saturating_sub(1);
            let t = s.tab()?;
            let objs = TabEdit::objs(t, page)?;
            let mut ids = Vec::new();
            for a in &args[1..] {
                let n: usize = a.parse().unwrap_or(1);
                match nth(&objs, n) {
                    Ok(o) => ids.push(o.id),
                    Err(e) => return Some(Err(e)),
                }
            }
            TabEdit::select(t, page, ids);
            app.edit_selection_changed(s, ui, true);
        }
        "edit-crop" => {
            let v: Vec<f32> = args.iter().map(|a| a.parse().unwrap_or(0.0)).collect();
            let t = s.tab()?;
            match (t.edit.crop.as_mut(), &v[..]) {
                (Some(c), [x, y, w, h]) => c.2 = [*x, *y, *w, *h],
                _ => return Some(Err("edit-crop needs a crop in progress and X Y W H".into())),
            }
            app.overlay_changed(s, ui);
        }
        "edit-replace-image" | "edit-add-image" | "edit-extract-image" => {
            pick::answer_next(PathBuf::from(arg(0)));
            let what = &cmd["edit-".len()..];
            app.edit_action(s, ui, what, "");
        }
        "edit-preedit" => {
            let vs = ui.global::<ViewState>();
            let text = vs.get_text_edit_text().to_string();
            let t = s.tab()?;
            let te = t.edit.text.as_mut()?;
            let (a, c) = te.selection();
            te.set_field(&text, c, a, arg(0));
            super::text::refresh_view(t);
            app.overlay_changed(s, ui);
        }
        _ => return None,
    }
    Some(Ok(()))
}

/// Where handle `name` of the selection (or crop) frame is: page and view point.
pub fn edit_handle(s: &mut State, name: &str) -> Result<Option<(usize, f64, f64)>, String> {
    let h = match name {
        "tl" => Handle::Corner(0),
        "tr" => Handle::Corner(1),
        "br" => Handle::Corner(2),
        "bl" => Handle::Corner(3),
        "t" => Handle::Edge(0),
        "r" => Handle::Edge(1),
        "b" => Handle::Edge(2),
        "l" => Handle::Edge(3),
        "rotate" => Handle::Rotate,
        n => return Err(format!("unknown handle {n}")),
    };
    if s.tab_ref().is_some_and(|t| !t.prep.sel.is_empty()) {
        return Ok(crate::app::prepare::handle_pos(s, h));
    }
    let t = s.tab().ok_or("no document is open")?;
    let k = t.view.last.k.max(0.01) as f32;
    let (page, r) = match t.edit.crop {
        Some((page, _, r)) => (page, r),
        None => {
            let (sel, objs) = TabEdit::current(t).ok_or("nothing is selected")?;
            let b = sel.bbox(&objs).ok_or("nothing is selected")?;
            let r = view_rect(&objs, &b);
            (sel.page, oe::outset(r, super::FRAME_PAD_PX / k))
        }
    };
    let (_, (x, y)) = oe::handles(r, super::ROTATE_PX / k)
        .into_iter()
        .find(|(hh, _)| *hh == h)
        .ok_or("no such handle")?;
    Ok(Some((page, f64::from(x), f64::from(y))))
}
