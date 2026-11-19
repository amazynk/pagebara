//! The comparison as a PDF: a summary with the list of changes, then each pair of pages
//! side by side (old left, new right) with the changes highlighted.

use std::fmt::Write as _;

use pagebara_stamp::Typesetter;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::{Change, ChangeKind, Comparison, shorten};

const W: f64 = 842.0;
const H: f64 = 595.0;
const MARGIN: f64 = 36.0;

#[derive(Clone, Debug)]
pub struct ReportOptions {
    pub old_name: String,
    pub new_name: String,
    /// Only page pairs with changes (else every pair).
    pub only_changed: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        ReportOptions {
            old_name: "Old".into(),
            new_name: "New".into(),
            only_changed: true,
        }
    }
}

/// Highlight colour per kind, for the old and the new side.
fn colour(kind: ChangeKind, new_side: bool) -> [f32; 3] {
    match kind {
        ChangeKind::Inserted | ChangeKind::PageInserted => [0.1, 0.7, 0.2],
        ChangeKind::Deleted | ChangeKind::PageDeleted => [0.9, 0.15, 0.15],
        ChangeKind::Replaced if new_side => [0.1, 0.7, 0.2],
        ChangeKind::Replaced => [0.9, 0.15, 0.15],
        ChangeKind::Moved => [0.2, 0.4, 0.95],
        ChangeKind::Visual => [1.0, 0.55, 0.0],
        ChangeKind::Annotation => [0.65, 0.2, 0.85],
        ChangeKind::Metadata | ChangeKind::Form => [0.4, 0.4, 0.4],
    }
}

fn n(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{r}")
}

struct Writer {
    doc: Document,
    ts: Typesetter,
    pages: Vec<ObjRef>,
    tree: ObjRef,
}

impl Writer {
    fn text(&mut self, c: &mut String, x: f64, y: f64, size: f64, bold: bool, s: &str) -> f64 {
        let font = if bold { "Helvetica-Bold" } else { "Helvetica" };
        let l = self.ts.line(&self.doc, font, size, s);
        let _ = writeln!(c, "BT 1 0 0 1 {} {} Tm {}ET", n(x), n(y), l.ops);
        l.width
    }

    /// `s` cut to fit `width` at `size`.
    fn fit(&mut self, s: &str, size: f64, width: f64) -> String {
        let mut t = s.to_owned();
        let mut chars = t.chars().count();
        while chars > 4 {
            let w = self.ts.line(&self.doc, "Helvetica", size, &t).width;
            if w <= width {
                return t;
            }
            chars = (chars as f64 * (width / w).min(0.95)) as usize;
            t = shorten(s, chars);
        }
        t
    }

    fn page(&mut self, content: String, xobjects: Dict, gs: bool) {
        let mut res = Dict::new();
        res.set(b"Font", self.ts.resources());
        res.set(b"XObject", xobjects);
        if gs {
            let mut g = Dict::new();
            g.set(b"Type", Object::name(b"ExtGState"));
            g.set(b"ca", 0.3);
            let mut d = Dict::new();
            d.set(b"Hl", g);
            res.set(b"ExtGState", d);
        }
        let mut sd = Dict::new();
        sd.set(b"Filter", Object::name(b"FlateDecode"));
        let s = self.doc.add(Stream::new(
            sd,
            pagebara_filters::encode_flate(content.as_bytes(), 6),
        ));
        let mut p = Dict::new();
        p.set(b"Type", Object::name(b"Page"));
        p.set(b"Parent", self.tree);
        p.set(
            b"MediaBox",
            Object::Array(vec![0.into(), 0.into(), Object::Real(W), Object::Real(H)]),
        );
        p.set(b"Resources", res);
        p.set(b"Contents", s);
        let r = self.doc.add(p);
        self.pages.push(r);
    }
}

/// Writes the report.
pub fn report(
    a: &Document,
    b: &Document,
    cmp: &Comparison,
    opts: &ReportOptions,
) -> Result<Vec<u8>, String> {
    let mut doc = pagebara_pages::blank((1, 7));
    // A fresh page tree replaces the blank document's.
    let tree = doc.add(Dict::new());
    let ts = Typesetter::new(&mut doc, Some(pagebara_typeset::system_fonts()));
    let mut w = Writer {
        doc,
        ts,
        pages: Vec::new(),
        tree,
    };
    summary(&mut w, cmp, opts);
    pairs(&mut w, a, b, cmp, opts)?;
    let Writer {
        mut doc,
        ts,
        pages,
        tree,
    } = w;
    ts.finish(&mut doc);
    let mut t = Dict::new();
    t.set(b"Type", Object::name(b"Pages"));
    t.set(b"Count", pages.len() as i64);
    t.set(
        b"Kids",
        pages.into_iter().map(Object::from).collect::<Vec<_>>(),
    );
    doc.set(tree, t);
    let root = doc
        .trailer()
        .get(b"Root")
        .and_then(Object::as_objref)
        .ok_or("no catalog")?;
    let mut cat = Dict::new();
    cat.set(b"Type", Object::name(b"Catalog"));
    cat.set(b"Pages", tree);
    doc.set(root, cat);
    let mut info = Dict::new();
    info.set(
        b"Title",
        Object::string(format!("Comparison of {} and {}", opts.old_name, opts.new_name).as_bytes()),
    );
    info.set(b"Producer", Object::string(b"pagebara"));
    let info = doc.add(info);
    let mut trailer = doc.trailer().clone();
    trailer.set(b"Info", info);
    doc.set_trailer(trailer);
    pagebara_pages::save_compact(&doc).map_err(|e| e.to_string())
}

fn summary(w: &mut Writer, cmp: &Comparison, opts: &ReportOptions) {
    let s = &cmp.summary;
    let mut lines: Vec<(f64, bool, String)> = vec![
        (20.0, true, "Comparison".into()),
        (
            11.0,
            false,
            format!("Old: {} ({} pages)", opts.old_name, cmp.pages_a),
        ),
        (
            11.0,
            false,
            format!("New: {} ({} pages)", opts.new_name, cmp.pages_b),
        ),
        (6.0, false, String::new()),
        (
            11.0,
            true,
            format!(
                "{} change{}",
                s.changes,
                if s.changes == 1 { "" } else { "s" }
            ),
        ),
        (
            10.0,
            false,
            format!(
                "Words: {} inserted, {} deleted, {} moved. Pages: {} inserted, {} deleted, {} looking different.",
                s.words_inserted,
                s.words_deleted,
                s.words_moved,
                s.pages_inserted,
                s.pages_deleted,
                s.pages_looking_different
            ),
        ),
        (
            10.0,
            false,
            format!(
                "Annotations: {}. Properties: {}. Form fields: {}.",
                s.annotations, s.metadata, s.forms
            ),
        ),
        (10.0, false, String::new()),
    ];
    for c in &cmp.changes {
        let at = match (c.page_a, c.page_b) {
            (Some(x), Some(y)) => format!("p. {} / {}", x + 1, y + 1),
            (Some(x), None) => format!("p. {} / –", x + 1),
            (None, Some(y)) => format!("p. – / {}", y + 1),
            _ => "document".into(),
        };
        lines.push((
            9.0,
            false,
            format!("{at}\u{1f}{}\u{1f}{}", c.kind.label(), c.description),
        ));
    }
    let mut c = String::new();
    let mut y = H - MARGIN;
    for (size, bold, text) in lines {
        let lh = size * 1.35;
        if y - lh < MARGIN {
            let content = std::mem::take(&mut c);
            w.page(content, Dict::new(), false);
            y = H - MARGIN;
        }
        y -= lh;
        if text.is_empty() {
            continue;
        }
        if let Some((at, rest)) = text.split_once('\u{1f}') {
            let (kind, what) = rest.split_once('\u{1f}').unwrap_or((rest, ""));
            w.text(&mut c, MARGIN, y, size, false, at);
            w.text(&mut c, MARGIN + 80.0, y, size, true, kind);
            let what = w.fit(what, size, W - 2.0 * MARGIN - 180.0);
            w.text(&mut c, MARGIN + 180.0, y, size, false, &what);
        } else {
            let t = w.fit(&text, size, W - 2.0 * MARGIN);
            w.text(&mut c, MARGIN, y, size, bold, &t);
        }
    }
    w.page(c, Dict::new(), false);
}

fn pairs(
    w: &mut Writer,
    a: &Document,
    b: &Document,
    cmp: &Comparison,
    opts: &ReportOptions,
) -> Result<(), String> {
    let half = (W - 3.0 * MARGIN / 2.0) / 2.0;
    let top = H - MARGIN - 18.0;
    let bottom = MARGIN / 2.0;
    for &(x, y) in &cmp.pairs {
        let here: Vec<&Change> = cmp
            .changes
            .iter()
            .filter(|c| {
                c.rects_a.iter().any(|r| Some(r.0) == x)
                    || c.rects_b.iter().any(|r| Some(r.0) == y)
                    || (x.is_none() && c.page_b == y && c.kind == ChangeKind::PageInserted)
                    || (y.is_none() && c.page_a == x && c.kind == ChangeKind::PageDeleted)
            })
            .collect();
        if opts.only_changed && here.is_empty() {
            continue;
        }
        let mut c = String::new();
        let mut xo = Dict::new();
        let label = |p: Option<usize>, gone: &str| match p {
            Some(p) => format!("page {}", p + 1),
            None => gone.to_owned(),
        };
        let t = format!("Old {}", label(x, "(no page)"));
        w.text(&mut c, MARGIN / 2.0, H - MARGIN, 11.0, true, &t);
        let t = format!("New {}", label(y, "(no page)"));
        w.text(&mut c, MARGIN + half, H - MARGIN, 11.0, true, &t);
        for (side, (src, p)) in [(a, x), (b, y)].into_iter().enumerate() {
            let ax = MARGIN / 2.0 + side as f64 * (half + MARGIN / 2.0);
            let Some(p) = p else {
                let _ = writeln!(
                    c,
                    "0.85 g {} {} {} {} re f 0 g",
                    n(ax),
                    n(bottom),
                    n(half),
                    n(top - bottom)
                );
                continue;
            };
            let (form, bb) =
                pagebara_stamp::page_as_form(&mut w.doc, src, p).map_err(|e| e.to_string())?;
            let (pw, ph) = (bb[2] - bb[0], bb[3] - bb[1]);
            let s = (half / pw).min((top - bottom) / ph);
            let ox = ax + (half - pw * s) / 2.0;
            let oy = top - ph * s;
            let name = format!("P{side}");
            xo.set(name.as_bytes(), form);
            let _ = writeln!(
                c,
                "q {} 0 0 {} {} {} cm /{name} Do Q",
                n(s),
                n(s),
                n(ox - bb[0] * s),
                n(oy - bb[1] * s)
            );
            let _ = writeln!(
                c,
                "q 0.6 G 0.5 w {} {} {} {} re S Q",
                n(ox),
                n(oy),
                n(pw * s),
                n(ph * s)
            );
            for ch in &here {
                let rects = if side == 0 { &ch.rects_a } else { &ch.rects_b };
                let [r, g, bl] = colour(ch.kind, side == 1);
                for (_, q) in rects.iter().filter(|q| q.0 == p) {
                    let x0 = ox + f64::from(q[0]) * s;
                    let y0 = oy + (ph - f64::from(q[3])) * s;
                    let (rw, rh) = (f64::from(q[2] - q[0]) * s, f64::from(q[3] - q[1]) * s);
                    let _ = writeln!(
                        c,
                        "q /Hl gs {r} {g} {bl} rg {r} {g} {bl} RG 0.6 w {} {} {} {} re b Q",
                        n(x0 - 1.0),
                        n(y0 - 1.0),
                        n(rw + 2.0),
                        n(rh + 2.0)
                    );
                }
            }
        }
        w.page(c, xo, true);
    }
    Ok(())
}
