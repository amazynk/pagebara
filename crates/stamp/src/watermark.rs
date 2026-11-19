//! Watermarks: text, a picture or a page of another PDF (drawn as vectors), placed by a
//! grid position and offset or tiled over the page, turned, faded and scaled.

use std::fmt::Write as _;
use std::path::PathBuf;

use pagebara_syntax::{Dict, Document, ObjRef, Object};
use serde::{Deserialize, Serialize};

use crate::draw::{self, PageStamp, concat, num};
use crate::text::Typesetter;
use crate::{Error, Kind, Layer, PageSet, Report, Result, TextStyle, Visibility, oc};

/// What the watermark shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Source {
    /// Text, one line per `\n`, centred.
    Text { text: String, style: TextStyle },
    /// A PNG or JPEG file; `data` (not saved in presets) wins over `path` when set.
    Image {
        path: PathBuf,
        #[serde(skip)]
        data: Vec<u8>,
    },
    /// Page `page` (1-based) of a PDF file, drawn as vectors.
    Pdf {
        path: PathBuf,
        page: usize,
        #[serde(skip)]
        data: Vec<u8>,
    },
}

/// How big the watermark is on each page.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Size {
    /// Times its natural size (text at its font size, a picture at 96 dpi, a page as it
    /// is).
    Scale(f32),
    /// As large as fits into this fraction of the page (turned as it is).
    Fit(f32),
}

/// Where on the page, in a 3 × 3 grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// Column and row: 0 left/top, 1 centre, 2 right/bottom.
    fn grid(self) -> (usize, usize) {
        let i = Anchor::ALL.iter().position(|a| *a == self).unwrap_or(4);
        (i % 3, i / 3)
    }
}

/// Repeating the watermark over the whole page.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tiling {
    /// Space between copies across and down, in points.
    pub gap: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watermark {
    pub source: Source,
    pub size: Size,
    /// 0 (invisible) to 1.
    pub opacity: f32,
    /// Degrees, counter-clockwise.
    pub rotation: f32,
    pub anchor: Anchor,
    /// Points from the anchored position, right and up as the page is shown.
    pub offset: [f32; 2],
    /// Distance from the page edge for anchors other than the centre, in points.
    pub margin: f32,
    pub tiling: Option<Tiling>,
    pub layer: Layer,
    pub pages: PageSet,
    /// The optional content group to draw it in (`None`: always drawn).
    pub group: Option<String>,
    pub visibility: Visibility,
    /// Remove our earlier watermarks first.
    pub replace: bool,
}

impl Default for Watermark {
    fn default() -> Self {
        Watermark {
            source: Source::Text {
                text: "CONFIDENTIAL".into(),
                style: TextStyle {
                    size: 72.0,
                    color: crate::Rgb::RED,
                    font: "Helvetica-Bold".into(),
                    background: None,
                },
            },
            size: Size::Fit(0.8),
            opacity: 0.3,
            rotation: 45.0,
            anchor: Anchor::Center,
            offset: [0.0, 0.0],
            margin: 36.0,
            tiling: None,
            layer: Layer::Front,
            pages: PageSet::all(),
            group: Some("Watermark".into()),
            visibility: Visibility::Always,
            replace: false,
        }
    }
}

/// The drawn unit: a form centred on the origin, with its natural size.
struct Unit {
    form: ObjRef,
    w: f64,
    h: f64,
}

fn read(path: &std::path::Path, data: &[u8]) -> Result<Vec<u8>> {
    if !data.is_empty() {
        return Ok(data.to_vec());
    }
    std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))
}

fn gs(opacity: f32) -> Dict {
    let o = f64::from(opacity.clamp(0.0, 1.0));
    let mut g = Dict::new();
    g.set(b"Type", Object::name(b"ExtGState"));
    g.set(b"ca", o);
    g.set(b"CA", o);
    let mut d = Dict::new();
    d.set(b"PagebaraGS", g);
    d
}

fn unit(doc: &mut Document, wm: &Watermark, ts: &mut Typesetter, spec: &str) -> Result<Unit> {
    let mut res = Dict::new();
    res.set(b"ExtGState", gs(wm.opacity));
    let mut c = String::from("/PagebaraGS gs\n");
    let (w, h) = match &wm.source {
        Source::Text { text, style } => {
            let size = f64::from(style.size.max(0.5));
            let lines: Vec<_> = text
                .lines()
                .map(|l| ts.line(doc, &style.font, size, l))
                .collect();
            if lines.iter().all(|l| l.width <= 0.0) {
                return Err(Error::Empty("the watermark text is empty"));
            }
            let lh = size * 1.2;
            let (asc, desc) = (lines[0].ascent, lines[0].descent);
            let w = lines.iter().map(|l| l.width).fold(0.0, f64::max);
            let h = (lines.len() - 1) as f64 * lh + (asc - desc);
            if let Some(bg) = style.background {
                let pad = size * 0.2;
                let [r, g, b] = bg.0;
                let _ = writeln!(
                    c,
                    "{} {} {} rg {} {} {} {} re f",
                    num(r.into()),
                    num(g.into()),
                    num(b.into()),
                    num(-w / 2.0 - pad),
                    num(-h / 2.0 - pad),
                    num(w + 2.0 * pad),
                    num(h + 2.0 * pad)
                );
            }
            let [r, g, b] = style.color.0;
            let _ = writeln!(
                c,
                "BT {} {} {} rg",
                num(r.into()),
                num(g.into()),
                num(b.into())
            );
            for (i, l) in lines.iter().enumerate() {
                let y = h / 2.0 - asc - i as f64 * lh;
                let _ = writeln!(c, "1 0 0 1 {} {} Tm {}", num(-l.width / 2.0), num(y), l.ops);
            }
            c.push_str("ET\n");
            res.set(b"Font", ts.resources());
            (w, h)
        }
        Source::Image { path, data } => {
            let bytes = read(path, data)?;
            let img = if bytes.starts_with(&[0xff, 0xd8]) {
                pagebara_edit::NewImage::Jpeg(&bytes)
            } else {
                pagebara_edit::NewImage::Png(&bytes)
            };
            let (im, pw, ph) =
                pagebara_edit::embed_image(doc, &img).map_err(|e| Error::Image(e.to_string()))?;
            let (w, h) = (f64::from(pw) * 0.75, f64::from(ph) * 0.75);
            let mut xo = Dict::new();
            xo.set(b"PagebaraIm", im);
            res.set(b"XObject", xo);
            let _ = writeln!(
                c,
                "q {} 0 0 {} {} {} cm /PagebaraIm Do Q",
                num(w),
                num(h),
                num(-w / 2.0),
                num(-h / 2.0)
            );
            (w, h)
        }
        Source::Pdf { path, page, data } => {
            let bytes = read(path, data)?;
            let src = Document::open(bytes).map_err(|e| Error::Source(e.to_string()))?;
            let (src, bbox) = page_as_form(doc, &src, page.saturating_sub(1))?;
            let (w, h) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
            let mut xo = Dict::new();
            xo.set(b"PagebaraPg", src);
            res.set(b"XObject", xo);
            let _ = writeln!(
                c,
                "q 1 0 0 1 {} {} cm /PagebaraPg Do Q",
                num(-bbox[0] - w / 2.0),
                num(-bbox[1] - h / 2.0)
            );
            (w, h)
        }
    };
    let bbox = [-w / 2.0 - 1.0, -h / 2.0 - 1.0, w / 2.0 + 1.0, h / 2.0 + 1.0];
    let f = draw::form(Kind::Watermark, bbox, res, c.as_bytes(), Some(spec));
    Ok(Unit {
        form: doc.add(f),
        w,
        h,
    })
}

/// Page `index` of `src` as a form XObject in `doc`, shown as `src` shows it (its
/// `/Rotate` applied), with its box in the form's space.
pub fn page_as_form(
    doc: &mut Document,
    src: &Document,
    index: usize,
) -> Result<(ObjRef, [f64; 4])> {
    let tree = pagebara_pages::PageTree::load(src);
    let p = tree.page(index).ok_or_else(|| {
        Error::Source(format!(
            "it has no page {} ({} pages)",
            index + 1,
            tree.len()
        ))
    })?;
    let pd = src
        .get(p.r)
        .and_then(|o| o.as_dict().cloned())
        .ok_or_else(|| Error::Source("broken page".into()))?;
    let content = pagebara_content::page_contents(src, &pd);
    let mut cur = pd.clone();
    let mut res = Object::Dict(Dict::new());
    for _ in 0..64 {
        let v = src.get_in(&cur, b"Resources");
        if !v.is_null() {
            res = cur.get(b"Resources").cloned().unwrap_or_default();
            break;
        }
        let parent = src.get_in(&cur, b"Parent");
        match parent.as_dict() {
            Some(d) => cur = d.clone(),
            None => break,
        }
    }
    // Copy the resources through a holder object so references come along.
    let mut tmp = src.clone();
    let holder = tmp.add(res);
    let mut map = Default::default();
    let res_ref = doc.import(&tmp, holder, &mut map);
    let c = p.crop_box;
    let bbox = [c.x0, c.y0, c.x1, c.y1];
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"XObject"));
    d.set(b"Subtype", Object::name(b"Form"));
    d.set(b"BBox", draw::rect_object(bbox));
    d.set(b"Resources", res_ref);
    // Shown as the source shows it: its user space to the turned page's.
    let (inv, shown): (Option<[f64; 6]>, _) = match p.rotate {
        90 => (
            Some([0.0, -1.0, 1.0, 0.0, -c.y0, c.x1]),
            [0.0, 0.0, c.height(), c.width()],
        ),
        180 => (
            Some([-1.0, 0.0, 0.0, -1.0, c.x1, c.y1]),
            [0.0, 0.0, c.width(), c.height()],
        ),
        270 => (
            Some([0.0, 1.0, -1.0, 0.0, c.y1, -c.x0]),
            [0.0, 0.0, c.height(), c.width()],
        ),
        _ => (None, bbox),
    };
    if let Some(m) = inv {
        d.set(
            b"Matrix",
            Object::Array(m.iter().map(|&v| Object::Real(v)).collect()),
        );
    }
    let f = draw::flate(d, &content);
    Ok((doc.add(f), shown))
}

/// The placements (matrices from the unit to shown page space) on a page `pw` × `ph`.
fn placements(wm: &Watermark, u: &Unit, pw: f64, ph: f64) -> Vec<[f64; 6]> {
    let t = f64::from(wm.rotation).to_radians();
    let (s, c) = t.sin_cos();
    let rot_w = |w: f64, h: f64| (w * c.abs() + h * s.abs(), w * s.abs() + h * c.abs());
    let (bw, bh) = rot_w(u.w, u.h);
    let k = match wm.size {
        Size::Scale(f) => f64::from(f.max(0.01)),
        Size::Fit(frac) => {
            let frac = f64::from(frac.clamp(0.01, 1.0));
            if bw <= 0.0 || bh <= 0.0 {
                1.0
            } else {
                (frac * pw / bw).min(frac * ph / bh)
            }
        }
    };
    let (bw, bh) = (bw * k, bh * k);
    let base = [k * c, k * s, -k * s, k * c, 0.0, 0.0];
    let at = |x: f64, y: f64| {
        let mut m = base;
        m[4] = x;
        m[5] = y;
        m
    };
    let [dx, dy] = wm.offset.map(f64::from);
    if let Some(tl) = wm.tiling {
        let sx = bw + f64::from(tl.gap[0].max(0.0));
        let sy = bh + f64::from(tl.gap[1].max(0.0));
        if sx < 1.0 || sy < 1.0 {
            return vec![at(pw / 2.0, ph / 2.0)];
        }
        let mut out = Vec::new();
        let rows = (ph / sy).ceil() as i64 + 1;
        let cols = (pw / sx).ceil() as i64 + 1;
        if rows * cols > 2000 {
            return vec![at(pw / 2.0, ph / 2.0)];
        }
        for r in -1..=rows {
            let shift = if r % 2 == 0 { 0.0 } else { sx / 2.0 };
            for col in -1..=cols {
                let x = pw / 2.0 + dx + col as f64 * sx + shift - (cols / 2) as f64 * sx;
                let y = ph / 2.0 + dy + r as f64 * sy - (rows / 2) as f64 * sy;
                // Only copies that reach the page.
                if x + bw / 2.0 >= 0.0
                    && x - bw / 2.0 <= pw
                    && y + bh / 2.0 >= 0.0
                    && y - bh / 2.0 <= ph
                {
                    out.push(at(x, y));
                }
            }
        }
        return out;
    }
    let m = f64::from(wm.margin.max(0.0));
    let (col, row) = wm.anchor.grid();
    let x = match col {
        0 => m + bw / 2.0,
        1 => pw / 2.0,
        _ => pw - m - bw / 2.0,
    };
    let y = match row {
        0 => ph - m - bh / 2.0,
        1 => ph / 2.0,
        _ => m + bh / 2.0,
    };
    vec![at(x + dx, y + dy)]
}

/// Draws the watermark on its pages.
pub fn apply_watermark(doc: &mut Document, wm: &Watermark) -> Result<Report> {
    crate::check_editable(doc)?;
    let frames = draw::frames(doc);
    let pages = wm.pages.resolve(frames.len())?;
    let mut report = Report::default();
    if wm.replace {
        report.replaced = crate::remove(doc, &[Kind::Watermark], false)?.removed;
    }
    let spec = serde_json::to_string(wm).unwrap_or_default();
    let mut ts = Typesetter::new(doc, Some(pagebara_typeset::system_fonts()));
    let u = unit(doc, wm, &mut ts, &spec)?;
    report.missing = ts.finish(doc);
    let group = wm
        .group
        .as_deref()
        .filter(|g| !g.trim().is_empty())
        .map(|g| oc::group(doc, g.trim(), Kind::Watermark.marker(), wm.visibility));
    let name = format!("{}{}", Kind::Watermark.prefix(), u.form.num);
    let xobjects = [(name.clone(), u.form)];
    for &i in &pages {
        let f = &frames[i];
        let mut body = draw::artifact("Watermark");
        for m in placements(wm, &u, f.w, f.h) {
            let _ = writeln!(body, "q {} /{name} Do Q", draw::cm(concat(m, f.m)));
        }
        body.push_str("EMC\n");
        draw::add(
            doc,
            f.r,
            &PageStamp {
                kind: Kind::Watermark,
                behind: wm.layer == Layer::Behind,
                xobjects: &xobjects,
                group,
                body: &body,
            },
        );
    }
    report.pages = pages;
    Ok(report)
}
