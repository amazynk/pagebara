//! Annotations in XFDF (ISO 19444-1 §6.4): the markup kinds of our annotation model as
//! XFDF elements, and back.

use pagebara_annot::{
    Align, Annotation, BorderStyle, Color, Flags, Kind, LineEnding, MarkupKind, Point, Quad, Rect,
};
use pagebara_syntax::Document;

use crate::xml::{Element, escape};

/// An annotation and the page (0-based) it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotData {
    pub page: usize,
    pub annot: Annotation,
}

/// The document's annotations that XFDF can carry.
pub(crate) fn collect(doc: &Document) -> Vec<AnnotData> {
    let tree = pagebara_pages::PageTree::load(doc);
    let mut out = Vec::new();
    for (page, p) in tree.pages().iter().enumerate() {
        let Some(d) = doc.get(p.r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        for a in pagebara_annot::annotations(doc, &d) {
            if element_name(&a.kind).is_some() {
                out.push(AnnotData { page, annot: a });
            }
        }
    }
    out
}

/// Adds the annotations to their pages; returns how many were added.
pub(crate) fn add_all(doc: &mut Document, list: &[AnnotData]) -> usize {
    let pages = pagebara_pages::PageTree::load(doc).refs();
    let mut n = 0;
    for a in list {
        let Some(&page) = pages.get(a.page) else {
            continue;
        };
        if pagebara_annot::add(doc, page, &a.annot).is_ok() {
            n += 1;
        }
    }
    n
}

fn element_name(k: &Kind) -> Option<&'static str> {
    Some(match k {
        Kind::Text { .. } => "text",
        Kind::FreeText { .. } => "freetext",
        Kind::Line { .. } => "line",
        Kind::Square { .. } => "square",
        Kind::Circle { .. } => "circle",
        Kind::Polygon { .. } => "polygon",
        Kind::PolyLine { .. } => "polyline",
        Kind::Markup { kind, .. } => match kind {
            MarkupKind::Highlight => "highlight",
            MarkupKind::Underline => "underline",
            MarkupKind::StrikeOut => "strikeout",
            MarkupKind::Squiggly => "squiggly",
        },
        Kind::Ink { .. } => "ink",
        Kind::Stamp { .. } => "stamp",
        _ => return None,
    })
}

fn num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == 0.0 { "0".into() } else { format!("{r}") }
}

fn color_attr(c: Color) -> String {
    let [r, g, b] = c
        .to_rgb()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn parse_color(s: &str) -> Option<Color> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let c = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16)
            .ok()
            .map(|v| f32::from(v) / 255.0)
    };
    Some(Color::Rgb(c(0)?, c(2)?, c(4)?))
}

fn points(ps: &[Point]) -> String {
    ps.iter()
        .map(|p| format!("{},{}", num(p.x), num(p.y)))
        .collect::<Vec<_>>()
        .join(";")
}

fn parse_numbers(s: &str) -> Vec<f64> {
    s.split([',', ';', ' ', '\n', '\r', '\t'])
        .filter_map(|v| v.trim().parse().ok())
        .collect()
}

fn parse_points(s: &str) -> Vec<Point> {
    parse_numbers(s)
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Point::new(c[0], c[1]))
        .collect()
}

const FLAG_NAMES: [(Flags, &str); 9] = [
    (Flags::INVISIBLE, "invisible"),
    (Flags::HIDDEN, "hidden"),
    (Flags::PRINT, "print"),
    (Flags::NO_ZOOM, "nozoom"),
    (Flags::NO_ROTATE, "norotate"),
    (Flags::NO_VIEW, "noview"),
    (Flags::READ_ONLY, "readonly"),
    (Flags::LOCKED, "locked"),
    (Flags::TOGGLE_NO_VIEW, "togglenoview"),
];

/// One annotation as an XFDF element.
pub(crate) fn write(out: &mut String, a: &AnnotData) {
    let an = &a.annot;
    let Some(tag) = element_name(&an.kind) else {
        return;
    };
    let mut attrs: Vec<(&str, String)> = vec![
        ("page", a.page.to_string()),
        (
            "rect",
            [an.rect.x0, an.rect.y0, an.rect.x1, an.rect.y1]
                .map(num)
                .join(","),
        ),
    ];
    if let Some(c) = an.color {
        attrs.push(("color", color_attr(c)));
    }
    let flags: Vec<&str> = FLAG_NAMES
        .iter()
        .filter(|(f, _)| an.flags.contains(*f))
        .map(|(_, n)| *n)
        .collect();
    if !flags.is_empty() {
        attrs.push(("flags", flags.join(",")));
    }
    for (k, v) in [
        ("name", &an.name),
        ("title", &an.author),
        ("subject", &an.subject),
        ("date", &an.modified),
        ("creationdate", &an.created),
    ] {
        if let Some(v) = v {
            attrs.push((k, v.clone()));
        }
    }
    if an.opacity < 1.0 {
        attrs.push(("opacity", num(an.opacity)));
    }
    attrs.push(("width", num(an.border.width)));
    if let BorderStyle::Dashed(d) = &an.border.style {
        attrs.push(("style", "dash".into()));
        attrs.push((
            "dashes",
            d.iter().map(|v| num(*v)).collect::<Vec<_>>().join(","),
        ));
    }
    let mut inner = String::new();
    match &an.kind {
        Kind::Text { icon, .. } => attrs.push(("icon", icon.clone())),
        Kind::Stamp { icon: Some(i) } => attrs.push(("icon", i.clone())),
        Kind::FreeText {
            font_size,
            text_color,
            align,
        } => {
            let [r, g, b] = text_color.to_rgb();
            inner.push_str(&format!(
                "<defaultappearance>/Helv {} Tf {} {} {} rg</defaultappearance>",
                num(*font_size),
                num(f64::from(r)),
                num(f64::from(g)),
                num(f64::from(b))
            ));
            let j = match align {
                Align::Left => "left",
                Align::Center => "centered",
                Align::Right => "right",
            };
            attrs.push(("justification", j.into()));
        }
        Kind::Line {
            start,
            end,
            endings,
            interior,
        } => {
            attrs.push(("start", format!("{},{}", num(start.x), num(start.y))));
            attrs.push(("end", format!("{},{}", num(end.x), num(end.y))));
            attrs.push((
                "head",
                String::from_utf8_lossy(endings[0].name()).into_owned(),
            ));
            attrs.push((
                "tail",
                String::from_utf8_lossy(endings[1].name()).into_owned(),
            ));
            if let Some(c) = interior {
                attrs.push(("interior-color", color_attr(*c)));
            }
        }
        Kind::Square { interior } | Kind::Circle { interior } => {
            if let Some(c) = interior {
                attrs.push(("interior-color", color_attr(*c)));
            }
        }
        Kind::Polygon { vertices, interior } => {
            if let Some(c) = interior {
                attrs.push(("interior-color", color_attr(*c)));
            }
            inner.push_str(&format!("<vertices>{}</vertices>", points(vertices)));
        }
        Kind::PolyLine {
            vertices,
            endings,
            interior,
        } => {
            attrs.push((
                "head",
                String::from_utf8_lossy(endings[0].name()).into_owned(),
            ));
            attrs.push((
                "tail",
                String::from_utf8_lossy(endings[1].name()).into_owned(),
            ));
            if let Some(c) = interior {
                attrs.push(("interior-color", color_attr(*c)));
            }
            inner.push_str(&format!("<vertices>{}</vertices>", points(vertices)));
        }
        Kind::Markup { quads, .. } => {
            let c: Vec<String> = quads
                .iter()
                .flat_map(|q| q.0)
                .flat_map(|p| [num(p.x), num(p.y)])
                .collect();
            attrs.push(("coords", c.join(",")));
        }
        Kind::Ink { strokes } => {
            inner.push_str("<inklist>");
            for s in strokes {
                inner.push_str(&format!("<gesture>{}</gesture>", points(s)));
            }
            inner.push_str("</inklist>");
        }
        _ => {}
    }
    if let Some(c) = &an.contents {
        inner.push_str(&format!("<contents>{}</contents>", escape(c)));
    }
    out.push('<');
    out.push_str(tag);
    for (k, v) in attrs {
        out.push_str(&format!(" {k}=\"{}\"", escape(&v)));
    }
    if inner.is_empty() {
        out.push_str("/>\n");
    } else {
        out.push('>');
        out.push_str(&inner);
        out.push_str(&format!("</{tag}>\n"));
    }
}

fn endings(e: &Element) -> [LineEnding; 2] {
    let n = |k: &str| LineEnding::from_name(e.attr(k).unwrap_or("None").as_bytes());
    [n("head"), n("tail")]
}

/// An annotation from its XFDF element; `None` for kinds we do not take.
pub(crate) fn read(e: &Element) -> Option<AnnotData> {
    let page: usize = e.attr("page")?.trim().parse().ok()?;
    let r = parse_numbers(e.attr("rect").unwrap_or_default());
    let rect = match r[..] {
        [x0, y0, x1, y1] => Rect::new(x0, y0, x1, y1),
        _ => Rect::default(),
    };
    let interior = e.attr("interior-color").and_then(parse_color);
    let child = |n: &str| e.elements().find(|c| c.name == n);
    let kind = match e.name.as_str() {
        "text" => Kind::Text {
            icon: e.attr("icon").unwrap_or("Note").to_owned(),
            open: false,
        },
        "stamp" => Kind::Stamp {
            icon: e.attr("icon").map(str::to_owned),
        },
        "freetext" => {
            let da = child("defaultappearance")
                .map(Element::text)
                .unwrap_or_default();
            let da = pagebara_forms::DefaultAppearance::parse(da.as_bytes());
            Kind::FreeText {
                font_size: if da.size > 0.0 { da.size } else { 12.0 },
                text_color: da.color,
                align: match e.attr("justification") {
                    Some("centered") => Align::Center,
                    Some("right") => Align::Right,
                    _ => Align::Left,
                },
            }
        }
        "line" => {
            let p = |k: &str| parse_points(e.attr(k).unwrap_or_default()).first().copied();
            Kind::Line {
                start: p("start")?,
                end: p("end")?,
                endings: endings(e),
                interior,
            }
        }
        "square" => Kind::Square { interior },
        "circle" => Kind::Circle { interior },
        "polygon" | "polyline" => {
            let vertices = parse_points(&child("vertices").map(Element::text).unwrap_or_default());
            if vertices.len() < 2 {
                return None;
            }
            if e.name == "polygon" {
                Kind::Polygon { vertices, interior }
            } else {
                Kind::PolyLine {
                    vertices,
                    endings: endings(e),
                    interior,
                }
            }
        }
        "highlight" | "underline" | "strikeout" | "squiggly" => {
            let quads: Vec<Quad> = parse_numbers(e.attr("coords").unwrap_or_default())
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| {
                    Quad([
                        Point::new(c[0], c[1]),
                        Point::new(c[2], c[3]),
                        Point::new(c[4], c[5]),
                        Point::new(c[6], c[7]),
                    ])
                })
                .collect();
            if quads.is_empty() {
                return None;
            }
            let kind = match e.name.as_str() {
                "highlight" => MarkupKind::Highlight,
                "underline" => MarkupKind::Underline,
                "strikeout" => MarkupKind::StrikeOut,
                _ => MarkupKind::Squiggly,
            };
            Kind::Markup { kind, quads }
        }
        "ink" => {
            let strokes: Vec<Vec<Point>> = child("inklist")
                .map(|l| {
                    l.elements()
                        .filter(|g| g.name == "gesture")
                        .map(|g| parse_points(&g.text()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if strokes.is_empty() {
                return None;
            }
            Kind::Ink { strokes }
        }
        _ => return None,
    };
    let mut a = Annotation::new(kind, rect);
    if let Some(c) = e.attr("color") {
        a.color = parse_color(c);
    }
    if let Some(f) = e.attr("flags") {
        let mut flags = Flags(0);
        for name in f.split(',').map(str::trim) {
            if let Some((fl, _)) = FLAG_NAMES.iter().find(|(_, n)| *n == name) {
                flags = flags | *fl;
            }
        }
        a.flags = flags;
    }
    let text = |k: &str| e.attr(k).map(str::to_owned);
    a.name = text("name");
    a.author = text("title");
    a.subject = text("subject");
    a.modified = text("date");
    a.created = text("creationdate");
    if let Some(o) = e.attr("opacity").and_then(|o| o.trim().parse::<f64>().ok()) {
        a.opacity = o.clamp(0.0, 1.0);
    }
    if let Some(w) = e.attr("width").and_then(|w| w.trim().parse::<f64>().ok()) {
        a.border.width = w.max(0.0);
    }
    if e.attr("style") == Some("dash") {
        let d = parse_numbers(e.attr("dashes").unwrap_or("3"));
        a.border.style = BorderStyle::Dashed(if d.is_empty() { vec![3.0] } else { d });
    }
    a.contents = child("contents").map(Element::text);
    Some(AnnotData { page, annot: a })
}
