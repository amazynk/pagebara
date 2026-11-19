//! Creating, changing, deleting and flattening annotations.

use std::sync::atomic::{AtomicU32, Ordering};

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::appearance::{self, Appearance};
use crate::embedded::default_font;
use crate::font::TextFont;
use crate::geom::{Color, Rect, num, points_object};
use crate::link::{self, LinkAction};
use crate::model::*;
use crate::page::{self, page_dict};
use crate::read::{from_dict, read};
use crate::{Error, Result, text};

/// Keys written from the model; anything else in an existing dictionary is kept on update.
/// Link targets (`/A`, `/Dest`) are handled separately so read-only ones survive.
const MANAGED: &[&[u8]] = &[
    b"Type",
    b"Subtype",
    b"Rect",
    b"Contents",
    b"T",
    b"Subj",
    b"NM",
    b"M",
    b"CreationDate",
    b"C",
    b"CA",
    b"Border",
    b"BS",
    b"F",
    b"IRT",
    b"RT",
    b"Popup",
    b"P",
    b"QuadPoints",
    b"InkList",
    b"L",
    b"LE",
    b"IC",
    b"Vertices",
    b"DA",
    b"Q",
    b"DS",
    b"RC",
    b"Name",
    b"Open",
    b"Parent",
    b"RD",
    b"Rotate",
];

/// Prefix of the `/NM` of annotations created here; their appearance streams are their own
/// and are rewritten in place on update.
const NAME_PREFIX: &str = "pagebara-";

/// The result of writing an annotation with [`add_with`] or [`update_with`].
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub id: ObjRef,
    /// Characters the font could not show, drawn with its fallback glyph. `/Contents` and
    /// `/RC` still hold the exact text, so readers that rebuild appearances show it.
    pub missing: Vec<char>,
}

/// Adds `a` to `page` (its object) with a generated appearance; returns its reference.
/// Kinds with geometry get their rectangle from it; `/NM`, `/M` and `/CreationDate` are
/// filled in when missing. Text is drawn in the [`default_font`] for it; see [`add_with`].
pub fn add(doc: &mut Document, page: ObjRef, a: &Annotation) -> Result<ObjRef> {
    add_with(doc, page, a, default_font(&drawn_text(a))).map(|w| w.id)
}

/// Every text an annotation's appearance may show (contents, popup title, stamp label).
fn drawn_text(a: &Annotation) -> String {
    let mut s = String::new();
    for t in [&a.contents, &a.author, &a.subject].into_iter().flatten() {
        s.push_str(t);
    }
    if let Kind::Stamp { icon: Some(i) } = &a.kind {
        s.push_str(&i.to_uppercase());
    }
    s
}

/// [`add`] with text drawn in `font`, reporting characters it lacks. Closed line endings
/// without an interior colour are filled with the stroke colour.
pub fn add_with(
    doc: &mut Document,
    page: ObjRef,
    a: &Annotation,
    font: &dyn TextFont,
) -> Result<Written> {
    if !a.kind.is_editable() {
        return Err(Error::ReadOnly);
    }
    if let Kind::Link {
        action: Some(act), ..
    } = &a.kind
        && !act.is_writable()
    {
        return Err(Error::ReadOnly);
    }
    let pd = page_dict(doc, page)?;
    let mut a = prepared(a, true, page::page_rotation(doc, &pd));
    a.fill_closed_endings();
    let mut d = Dict::new();
    let missing = write_dict(doc, &mut d, &a, Some(page), None, font);
    let r = doc.add(d);
    let mut list = page::annots(doc, &page_dict(doc, page)?);
    list.push(r.into());
    page::set_annots(doc, page, list)?;
    Ok(Written { id: r, missing })
}

/// Rewrites the annotation at `r` from `a`, regenerating its appearance (and that of a
/// drawn popup). Keys the model does not cover are kept; image stamps keep their
/// appearance (scaled to the new rect); links keep a read-only target that is unchanged.
pub fn update(doc: &mut Document, r: ObjRef, a: &Annotation) -> Result<()> {
    update_with(doc, r, a, default_font(&drawn_text(a))).map(drop)
}

/// [`update`] with text drawn in `font`.
pub fn update_with(
    doc: &mut Document,
    r: ObjRef,
    a: &Annotation,
    font: &dyn TextFont,
) -> Result<Written> {
    let old = doc.get(r).ok_or(Error::NotFound)?;
    let old = old.as_dict().ok_or(Error::NotFound)?.clone();
    let current = from_dict(doc, &old, Some(r)).ok_or(Error::NotFound)?;
    if !current.kind.is_editable() || !a.kind.is_editable() {
        return Err(Error::ReadOnly);
    }
    let current_action = match current.kind {
        Kind::Link { action, .. } => action,
        _ => None,
    };
    if let Kind::Link {
        action: Some(act), ..
    } = &a.kind
        && !act.is_writable()
        && current_action.as_ref() != Some(act)
    {
        return Err(Error::ReadOnly);
    }
    let page = old.get(b"P").and_then(Object::as_objref);
    let rotation = page
        .and_then(|p| page_dict(doc, p).ok())
        .map_or(0, |pd| page::page_rotation(doc, &pd));
    let ours = current
        .name
        .as_deref()
        .is_some_and(|n| n.starts_with(NAME_PREFIX));
    let prev = Previous {
        ap: old.get(b"AP").cloned(),
        own_stream: ours
            .then(|| doc.get_in(&old, b"AP").as_dict()?.get(b"N")?.as_objref())
            .flatten()
            .filter(|n| doc.get(*n).is_some_and(|o| o.as_stream().is_some())),
        action: current_action,
    };
    let mut d = old;
    for k in MANAGED {
        d.remove(k);
    }
    let a = prepared(a, false, rotation);
    let mut missing = write_dict(doc, &mut d, &a, page, Some(prev), font);
    doc.set(r, d);
    if let Some(p) = a.popup
        && doc
            .get(p)
            .is_some_and(|o| o.as_dict().is_some_and(|d| d.contains(b"AP")))
    {
        for c in draw_popup(doc, p, font)? {
            if !missing.contains(&c) {
                missing.push(c);
            }
        }
    }
    Ok(Written { id: r, missing })
}

/// Removes the annotation at `r` from `page`, with its popup and, recursively, its replies.
/// Returns every reference removed.
pub fn remove(doc: &mut Document, page: ObjRef, r: ObjRef) -> Result<Vec<ObjRef>> {
    let d = page_dict(doc, page)?;
    let list = page::annots(doc, &d);
    let all = crate::read::annotations(doc, &d);
    let mut gone = vec![r];
    let mut i = 0;
    while i < gone.len() {
        let cur = gone[i];
        for a in &all {
            let Some(id) = a.id else { continue };
            let linked = a.in_reply_to == Some(cur)
                || matches!(a.kind, Kind::Popup { parent: Some(p), .. } if p == cur);
            if linked && !gone.contains(&id) {
                gone.push(id);
            }
        }
        if let Some(p) = all.iter().find(|a| a.id == Some(cur)).and_then(|a| a.popup)
            && !gone.contains(&p)
        {
            gone.push(p);
        }
        i += 1;
    }
    if !list.iter().any(|o| o.as_objref() == Some(r)) {
        return Err(Error::NotFound);
    }
    let kept = list
        .into_iter()
        .filter(|o| o.as_objref().is_none_or(|x| !gone.contains(&x)))
        .collect();
    page::set_annots(doc, page, kept)?;
    for &g in &gone {
        doc.delete(g);
    }
    // A parent that stays must not point at its removed popup.
    for a in &all {
        if let (Some(id), Some(p)) = (a.id, a.popup)
            && gone.contains(&p)
            && !gone.contains(&id)
            && let Some(mut d) = doc.get(id).and_then(|o| o.as_dict().cloned())
        {
            d.remove(b"Popup");
            doc.set(id, d);
        }
    }
    Ok(gone)
}

/// Adds a reply by `author` to the annotation `parent`, threaded through `/IRT`. Replies
/// get an empty appearance: readers list them with the comment instead of drawing them.
pub fn reply(
    doc: &mut Document,
    page: ObjRef,
    parent: ObjRef,
    author: &str,
    contents: &str,
) -> Result<ObjRef> {
    let p = read(doc, parent).ok_or(Error::NotFound)?;
    let mut a = Annotation::new(
        Kind::Text {
            icon: "Comment".into(),
            open: false,
        },
        p.rect,
    );
    a.author = Some(author.into());
    a.contents = Some(contents.into());
    a.in_reply_to = Some(parent);
    a.subject = p.subject.clone();
    add(doc, page, &a)
}

/// Adds a popup window for the markup annotation `parent`, linking both ways. Viewers draw
/// popups themselves; [`draw_popup`] gives it an appearance for those that do not.
pub fn add_popup(
    doc: &mut Document,
    page: ObjRef,
    parent: ObjRef,
    rect: Rect,
    open: bool,
) -> Result<ObjRef> {
    let mut a = Annotation::new(
        Kind::Popup {
            parent: Some(parent),
            open,
        },
        rect,
    );
    a.color = None;
    a.flags = Flags::PRINT | Flags::NO_ZOOM | Flags::NO_ROTATE;
    let r = add(doc, page, &a)?;
    let mut pd = doc
        .get(parent)
        .and_then(|o| o.as_dict().cloned())
        .ok_or(Error::NotFound)?;
    pd.set(b"Popup", r);
    doc.set(parent, pd);
    Ok(r)
}

/// Gives the popup at `popup` an appearance drawn from its parent: a title bar with the
/// author in the parent's colour, and the contents. Kept current by [`update`] of the
/// parent. Returns the characters `font` could not show.
pub fn draw_popup(doc: &mut Document, popup: ObjRef, font: &dyn TextFont) -> Result<Vec<char>> {
    let p = read(doc, popup).ok_or(Error::NotFound)?;
    let Kind::Popup {
        parent: Some(parent),
        ..
    } = p.kind
    else {
        return Err(Error::Invalid("not a popup with a parent"));
    };
    let parent = read(doc, parent).ok_or(Error::NotFound)?;
    let title = parent
        .author
        .as_deref()
        .or(parent.subject.as_deref())
        .unwrap_or(parent.subtype());
    let ap = appearance::popup(
        p.rect,
        title,
        parent.contents.as_deref().unwrap_or(""),
        parent.color,
        font,
    );
    let missing = ap.missing.clone();
    let mut d = doc
        .get(popup)
        .and_then(|o| o.as_dict().cloned())
        .ok_or(Error::NotFound)?;
    let own = doc
        .get_in(&d, b"AP")
        .as_dict()
        .and_then(|ap| ap.get(b"N")?.as_objref())
        .filter(|n| doc.get(*n).is_some_and(|o| o.as_stream().is_some()));
    set_appearance(doc, &mut d, ap, own, font);
    doc.set(popup, d);
    Ok(missing)
}

/// Draws the annotation at `r` into the page content and removes it (and its popup and
/// replies). Hidden annotations are only removed. One without an appearance stream gets
/// one generated in the [`default_font`] for its text; see [`flatten_with`].
pub fn flatten(doc: &mut Document, page: ObjRef, r: ObjRef) -> Result<()> {
    let text = read(doc, r).map(|a| drawn_text(&a)).unwrap_or_default();
    flatten_with(doc, page, r, default_font(&text))
}

/// [`flatten`], generating missing appearances with text drawn in `font`.
pub fn flatten_with(
    doc: &mut Document,
    page: ObjRef,
    r: ObjRef,
    font: &dyn TextFont,
) -> Result<()> {
    let d = doc.get(r).ok_or(Error::NotFound)?;
    let d = d.as_dict().ok_or(Error::NotFound)?.clone();
    let flags = Flags(d.int(b"F").unwrap_or(0) as u32);
    let visible = !flags.contains(Flags::HIDDEN) && !flags.contains(Flags::NO_VIEW);
    let form = match normal_appearance(doc, &d) {
        Some(form) => Some(form),
        None if visible => generated_appearance(doc, &d, r, font),
        None => None,
    };
    if visible && let Some(form) = form {
        let rect = Rect::from_object(&doc.get_in(&d, b"Rect")).unwrap_or_default();
        let m = form_matrix(doc, form, rect);
        page::draw_form(doc, page, form, m)?;
    }
    remove(doc, page, r)?;
    Ok(())
}

/// A new appearance stream for an annotation without one, when its kind is drawn here.
fn generated_appearance(
    doc: &mut Document,
    d: &Dict,
    r: ObjRef,
    font: &dyn TextFont,
) -> Option<ObjRef> {
    let a = from_dict(doc, d, Some(r))?;
    if !a.kind.is_editable() {
        return None;
    }
    let ap = appearance::generate(&a, font)?;
    let form = ap.into_stream(doc, font);
    Some(doc.add(form))
}

/// Draws `a` straight into the page content without adding an annotation, for marks that
/// must not be removable. Text is drawn in the [`default_font`] for it; see
/// [`add_flattened_with`].
pub fn add_flattened(doc: &mut Document, page: ObjRef, a: &Annotation) -> Result<()> {
    add_flattened_with(doc, page, a, default_font(&drawn_text(a))).map(drop)
}

/// [`add_flattened`] with text drawn in `font`; returns the characters it lacks.
pub fn add_flattened_with(
    doc: &mut Document,
    page: ObjRef,
    a: &Annotation,
    font: &dyn TextFont,
) -> Result<Vec<char>> {
    let pd = page_dict(doc, page)?;
    let mut a = prepared(a, true, page::page_rotation(doc, &pd));
    a.fill_closed_endings();
    let ap = appearance::generate(&a, font).ok_or(Error::ReadOnly)?;
    let missing = ap.missing.clone();
    let form = ap.into_stream(doc, font);
    let form = doc.add(form);
    let m = form_matrix(doc, form, a.rect);
    page::draw_form(doc, page, form, m)?;
    Ok(missing)
}

/// The `/AP /N` stream, choosing the `/AS` state when there are several.
fn normal_appearance(doc: &Document, d: &Dict) -> Option<ObjRef> {
    let ap = doc.get_in(d, b"AP");
    let n = ap.as_dict()?.get(b"N")?;
    let r = n.as_objref()?;
    let target = doc.get(r)?;
    if target.as_stream().is_some() {
        return Some(r);
    }
    let state = d.name(b"AS")?;
    target.as_dict()?.get(state)?.as_objref()
}

/// The matrix that maps a form's transformed bounding box onto `rect` (ISO 32000-2
/// §12.5.5), as a `cm` operand when the form is painted with `Do`.
pub(crate) fn form_matrix(doc: &Document, form: ObjRef, rect: Rect) -> [f64; 6] {
    let Some(f) = doc.get(form) else {
        return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    };
    let d = f.as_dict().cloned().unwrap_or_default();
    let bbox = Rect::from_object(&doc.get_in(&d, b"BBox")).unwrap_or(rect);
    let m = crate::geom::numbers(&doc.get_in(&d, b"Matrix"))
        .filter(|m| m.len() == 6)
        .unwrap_or(vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let tr = |x: f64, y: f64| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
    let corners = [
        tr(bbox.x0, bbox.y0),
        tr(bbox.x1, bbox.y0),
        tr(bbox.x0, bbox.y1),
        tr(bbox.x1, bbox.y1),
    ];
    let b = Rect::bounding(corners.map(|(x, y)| crate::geom::Point::new(x, y))).unwrap_or(rect);
    let sx = if b.width() > 0.0 {
        rect.width() / b.width()
    } else {
        1.0
    };
    let sy = if b.height() > 0.0 {
        rect.height() / b.height()
    } else {
        1.0
    };
    [sx, 0.0, 0.0, sy, rect.x0 - b.x0 * sx, rect.y0 - b.y0 * sy]
}

static SERIAL: AtomicU32 = AtomicU32::new(0);

/// A copy of `a` with its rectangle derived from its geometry, its bookkeeping fields set,
/// and free text and stamps following the page `rotation` unless they have their own.
fn prepared(a: &Annotation, new: bool, rotation: u16) -> Annotation {
    let mut a = a.clone();
    if let Some(r) = a.geometry_rect() {
        a.rect = r;
    }
    if matches!(a.kind, Kind::FreeText { .. } | Kind::Stamp { .. }) && a.rotate.is_none() {
        a.rotate = Some(rotation);
    }
    let now = text::now();
    if new && a.created.is_none() {
        a.created = Some(now.clone());
    }
    a.modified = Some(now);
    if a.name.is_none() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = SERIAL.fetch_add(1, Ordering::Relaxed);
        a.name = Some(format!("{NAME_PREFIX}{nanos:x}-{n}"));
    }
    a
}

/// What an updated dictionary had before.
struct Previous {
    ap: Option<Object>,
    /// The normal appearance stream when it belongs to this annotation alone.
    own_stream: Option<ObjRef>,
    action: Option<LinkAction>,
}

/// Writes the model's keys and the appearance; returns the characters the font lacked.
fn write_dict(
    doc: &mut Document,
    d: &mut Dict,
    a: &Annotation,
    page: Option<ObjRef>,
    prev: Option<Previous>,
    font: &dyn TextFont,
) -> Vec<char> {
    d.set(b"Type", Object::name(b"Annot"));
    d.set(b"Subtype", Object::name(a.subtype().as_bytes()));
    d.set(b"Rect", a.rect.to_object());
    if let Some(p) = page {
        d.set(b"P", p);
    }
    let mut s = |k: &[u8], v: &Option<String>| {
        if let Some(v) = v {
            d.set(k, text::encode(v));
        }
    };
    s(b"Contents", &a.contents);
    s(b"T", &a.author);
    s(b"Subj", &a.subject);
    s(b"NM", &a.name);
    s(b"M", &a.modified);
    s(b"CreationDate", &a.created);
    d.set(b"F", i64::from(a.flags.0));
    if let Some(c) = a.color {
        d.set(b"C", c.to_object());
    }
    if a.opacity < 1.0 {
        d.set(b"CA", num(a.opacity.clamp(0.0, 1.0)));
    }
    write_border(d, &a.border);
    if let Some(irt) = a.in_reply_to {
        d.set(b"IRT", irt);
        if a.reply_type == ReplyType::Group {
            d.set(b"RT", Object::name(b"Group"));
        }
    }
    if let Some(p) = a.popup {
        d.set(b"Popup", p);
    }
    if let Some(r) = a.rotate.filter(|&r| r != 0) {
        d.set(b"Rotate", i64::from(r));
    }
    let le =
        |e: &[LineEnding; 2]| Object::Array(e.iter().map(|e| Object::name(e.name())).collect());
    let interior = |d: &mut Dict, c: &Option<Color>| {
        if let Some(c) = c {
            d.set(b"IC", c.to_object());
        }
    };
    match &a.kind {
        Kind::Text { icon, open } => {
            d.set(b"Name", Object::name(icon.as_bytes()));
            d.set(b"Open", *open);
        }
        Kind::Link { action, quads } => {
            let unchanged = prev.as_ref().is_some_and(|p| p.action == *action);
            if !unchanged {
                d.remove(b"A");
                d.remove(b"Dest");
                if let Some(act) = action {
                    link::write(d, act);
                }
            }
            if !quads.is_empty() {
                let pts: Vec<_> = quads.iter().flat_map(|q| q.0).collect();
                d.set(b"QuadPoints", points_object(&pts));
            }
        }
        Kind::FreeText {
            font_size,
            text_color,
            align,
        } => {
            let mut c = crate::content::Content::new();
            c.fill_color(*text_color);
            let col = String::from_utf8_lossy(&c.0).trim().to_string();
            let da = format!("/{} {} Tf {col}", font.resource_name(), num_str(*font_size));
            d.set(b"DA", Object::string(da.as_bytes()));
            d.set(b"Q", *align as i64);
            let style = text::css_style(font.family(), *font_size, *text_color, *align);
            d.set(b"DS", text::encode(&style));
            let rc = text::rich_text(a.contents.as_deref().unwrap_or(""), &style);
            d.set(b"RC", text::encode(&rc));
        }
        Kind::Line {
            start,
            end,
            endings,
            interior: ic,
        } => {
            d.set(b"L", points_object(&[*start, *end]));
            d.set(b"LE", le(endings));
            interior(d, ic);
        }
        Kind::Square { interior: ic } | Kind::Circle { interior: ic } => interior(d, ic),
        Kind::Polygon {
            vertices,
            interior: ic,
        } => {
            d.set(b"Vertices", points_object(vertices));
            interior(d, ic);
        }
        Kind::PolyLine {
            vertices,
            endings,
            interior: ic,
        } => {
            d.set(b"Vertices", points_object(vertices));
            d.set(b"LE", le(endings));
            interior(d, ic);
        }
        Kind::Markup { quads, .. } => {
            let pts: Vec<_> = quads.iter().flat_map(|q| q.0).collect();
            d.set(b"QuadPoints", points_object(&pts));
        }
        Kind::Ink { strokes } => {
            d.set(
                b"InkList",
                Object::Array(strokes.iter().map(|s| points_object(s)).collect()),
            );
        }
        Kind::Stamp { icon } => {
            if let Some(i) = icon {
                d.set(b"Name", Object::name(i.as_bytes()));
            }
        }
        Kind::Popup { parent, open } => {
            if let Some(p) = parent {
                d.set(b"Parent", *p);
            }
            d.set(b"Open", *open);
        }
        Kind::Widget { .. } | Kind::Other(_) => {}
    }
    let ap = if a.in_reply_to.is_some() && matches!(a.kind, Kind::Text { .. }) {
        Some(Appearance::empty(a.rect))
    } else {
        appearance::generate(a, font)
    };
    let (old_ap, own) = prev.map_or((None, None), |p| (p.ap, p.own_stream));
    match ap {
        Some(ap) => {
            let missing = ap.missing.clone();
            set_appearance(doc, d, ap, own, font);
            missing
        }
        None => {
            if let Some(old) = old_ap {
                d.set(b"AP", old);
            }
            Vec::new()
        }
    }
}

/// Stores `ap` as the normal appearance, rewriting `own` in place when given.
fn set_appearance(
    doc: &mut Document,
    d: &mut Dict,
    ap: Appearance,
    own: Option<ObjRef>,
    font: &dyn TextFont,
) {
    let stream = ap.into_stream(doc, font);
    let n = match own {
        Some(r) => {
            doc.set(r, stream);
            r
        }
        None => doc.add(stream),
    };
    let mut apd = Dict::new();
    apd.set(b"N", n);
    d.set(b"AP", apd);
}

fn write_border(d: &mut Dict, b: &Border) {
    let mut bs = Dict::new();
    bs.set(b"W", num(b.width));
    let s: &[u8] = match &b.style {
        BorderStyle::Solid => b"S",
        BorderStyle::Dashed(dash) => {
            bs.set(b"D", Object::Array(dash.iter().map(|&v| num(v)).collect()));
            b"D"
        }
        BorderStyle::Beveled => b"B",
        BorderStyle::Inset => b"I",
        BorderStyle::Underline => b"U",
    };
    bs.set(b"S", Object::name(s));
    d.set(b"BS", bs);
}

fn num_str(v: f64) -> String {
    let mut out = Vec::new();
    pagebara_syntax::write_object(&mut out, &num(v));
    String::from_utf8_lossy(&out).into_owned()
}
