//! Synthetic documents and images for UI tests and demos (`--make-samples DIR`): a lease, a
//! report with page labels and links, a form, protected copies, a photographed signature
//! and a picture, so tests never need anyone's real files.

use std::fmt::Write as _;
use std::path::Path;

use pagebara_annot::{Helvetica, TextFont};
use pagebara_syntax::{Document, Permissions};

mod edit;
mod live;
mod more;
mod printed;
mod tagged;

pub use more::{TestPki, pki, rotated, signed};
pub use printed::printed_form;
pub use tagged::tagged;

/// A classic PDF built object by object.
#[derive(Default)]
struct Pdf {
    objs: Vec<String>,
}

impl Pdf {
    fn reserve(&mut self) -> usize {
        self.objs.push(String::new());
        self.objs.len()
    }

    fn set(&mut self, n: usize, o: String) {
        self.objs[n - 1] = o;
    }

    fn add(&mut self, o: String) -> usize {
        self.objs.push(o);
        self.objs.len()
    }

    fn stream(&mut self, data: &str) -> usize {
        self.add(format!(
            "<</Length {}>>stream\n{data}\nendstream",
            data.len()
        ))
    }

    fn finish(self, root: usize, info: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in self.objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", self.objs.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n\r\n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<</Size {} /Root {root} 0 R /Info {info} 0 R>>\nstartxref\n{xref}\n%%EOF\n",
                self.objs.len() + 1
            )
            .bytes(),
        );
        out
    }
}

/// A PDF string literal in WinAnsi.
fn lit(s: &str) -> String {
    let mut out = String::from("(");
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            c => match Helvetica::code(c) {
                Some(b) => {
                    let _ = write!(out, "\\{b:03o}");
                }
                None => out.push('?'),
            },
        }
    }
    out.push(')');
    out
}

/// A text string for dictionaries (outline titles, info): UTF-16 when needed.
fn text(s: &str) -> String {
    if s.is_ascii() {
        return lit(s);
    }
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out.push('>');
    out
}

fn width(s: &str, size: f64) -> f64 {
    s.chars()
        .map(|c| Helvetica.glyph(c).map_or(0.5, |g| g.width))
        .sum::<f64>()
        * size
}

/// One page being laid out, top to bottom.
struct Page {
    w: f64,
    h: f64,
    y: f64,
    ops: String,
    annots: Vec<String>,
}

const MARGIN: f64 = 72.0;

impl Page {
    fn new(w: f64, h: f64) -> Page {
        Page {
            w,
            h,
            y: h - MARGIN,
            ops: String::new(),
            annots: Vec::new(),
        }
    }

    fn line_at(&mut self, font: &str, size: f64, x: f64, y: f64, s: &str) {
        let _ = writeln!(
            self.ops,
            "BT /{font} {size} Tf {x:.2} {y:.2} Td {} Tj ET",
            lit(s)
        );
    }

    fn heading(&mut self, s: &str, size: f64) -> f64 {
        self.y -= size * 0.4;
        let top = self.y;
        self.line_at("F2", size, MARGIN, self.y - size, s);
        self.y -= size * 1.8;
        top
    }

    /// A wrapped paragraph; returns the y of its first line's top.
    fn para(&mut self, s: &str, size: f64) -> f64 {
        let top = self.y;
        let max = self.w - 2.0 * MARGIN;
        let mut line = String::new();
        for word in s.split_whitespace() {
            let cand = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if width(&cand, size) > max && !line.is_empty() {
                let l = std::mem::take(&mut line);
                self.line_at("F1", size, MARGIN, self.y - size, &l);
                self.y -= size * 1.45;
                line = word.to_owned();
            } else {
                line = cand;
            }
        }
        if !line.is_empty() {
            self.line_at("F1", size, MARGIN, self.y - size, &line);
            self.y -= size * 1.45;
        }
        self.y -= size * 0.8;
        top
    }

    /// A one-line link: GoTo a page object or open a URI.
    fn link(&mut self, s: &str, size: f64, target: &str) {
        let y = self.y - size;
        let _ = writeln!(self.ops, "0.15 0.39 0.92 rg");
        self.line_at("F1", size, MARGIN, y, s);
        let _ = writeln!(self.ops, "0 g");
        let w = width(s, size);
        let _ = writeln!(
            self.ops,
            "0.15 0.39 0.92 RG 0.6 w {MARGIN} {:.2} m {:.2} {:.2} l S 0 G",
            y - 2.0,
            MARGIN + w,
            y - 2.0
        );
        self.annots.push(format!(
            "<</Type/Annot/Subtype/Link/Rect[{MARGIN} {:.2} {:.2} {:.2}]/Border[0 0 0]{target}>>",
            y - 3.0,
            MARGIN + w,
            y + size
        ));
        self.y -= size * 2.2;
    }

    fn rule(&mut self, x0: f64, x1: f64, y: f64) {
        let _ = writeln!(self.ops, "0.6 w {x0:.2} {y:.2} m {x1:.2} {y:.2} l S");
    }

    fn footer(&mut self, s: &str) {
        let x = (self.w - width(s, 9.0)) / 2.0;
        let _ = writeln!(self.ops, "0.45 g");
        self.line_at("F1", 9.0, x, 36.0, s);
        let _ = writeln!(self.ops, "0 g");
    }
}

/// Writes pages and the catalog; `extra` adds catalog entries (outlines, names, labels,
/// forms) given the page object numbers.
fn build(
    pages: Vec<Page>,
    info: &[(&str, &str)],
    extra: impl FnOnce(&mut Pdf, &[usize], usize) -> String,
) -> Vec<u8> {
    let mut pdf = Pdf::default();
    let catalog = pdf.reserve();
    let tree = pdf.reserve();
    let fonts = pdf.add(
        "<</F1<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>\
         /F2<</Type/Font/Subtype/Type1/BaseFont/Helvetica-Bold/Encoding/WinAnsiEncoding>>>>"
            .into(),
    );
    let refs: Vec<usize> = pages.iter().map(|_| pdf.reserve()).collect();
    for (p, &r) in pages.iter().zip(&refs) {
        let contents = pdf.stream(&p.ops);
        let annots: Vec<String> = p
            .annots
            .iter()
            .map(|a| {
                // Page targets are written as "@N" (0-based page index).
                let a = resolve_targets(a, &refs);
                format!("{} 0 R", pdf.add(a))
            })
            .collect();
        let annots = if annots.is_empty() {
            String::new()
        } else {
            format!("/Annots[{}]", annots.join(" "))
        };
        pdf.set(
            r,
            format!(
                "<</Type/Page/Parent {tree} 0 R/MediaBox[0 0 {} {}]/Resources<</Font {fonts} 0 R>>/Contents {contents} 0 R{annots}>>",
                p.w, p.h
            ),
        );
    }
    let kids: Vec<String> = refs.iter().map(|r| format!("{r} 0 R")).collect();
    pdf.set(
        tree,
        format!(
            "<</Type/Pages/Kids[{}]/Count {}>>",
            kids.join(" "),
            refs.len()
        ),
    );
    let more = extra(&mut pdf, &refs, catalog);
    let more = resolve_targets(&more, &refs);
    pdf.set(catalog, format!("<</Type/Catalog/Pages {tree} 0 R{more}>>"));
    let fields: Vec<String> = info
        .iter()
        .map(|(k, v)| format!("/{k} {}", text(v)))
        .collect();
    let info = pdf.add(format!(
        "<<{} /Producer (pagebara samples) /CreationDate (D:20261105093000Z)>>",
        fields.join(" ")
    ));
    pdf.finish(catalog, info)
}

/// Replaces "@N" page markers with references to page N.
fn resolve_targets(s: &str, refs: &[usize]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('@') {
        out.push_str(&rest[..i]);
        let digits: String = rest[i + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        match digits.parse::<usize>().ok().and_then(|n| refs.get(n)) {
            Some(r) => {
                let _ = write!(out, "{r} 0 R");
            }
            None => out.push('@'),
        }
        rest = &rest[i + 1 + digits.len()..];
    }
    out.push_str(rest);
    out
}

/// Outline items: (title, depth, page index, top y); written as a linked tree.
fn outline(
    pdf: &mut Pdf,
    items: &[(&str, usize, usize, f64)],
    root_children: &mut Vec<usize>,
) -> usize {
    let root = pdf.reserve();
    let nums: Vec<usize> = items.iter().map(|_| pdf.reserve()).collect();
    let parent_of = |i: usize| -> Option<usize> {
        let d = items[i].1;
        (0..i).rev().find(|&j| items[j].1 < d)
    };
    let children = |p: Option<usize>| -> Vec<usize> {
        (0..items.len())
            .filter(|&i| parent_of(i) == p && (p.is_some() || items[i].1 == 0))
            .collect()
    };
    for (i, (title, _, page, top)) in items.iter().enumerate() {
        let parent = parent_of(i).map_or(root, |p| nums[p]);
        let sibs = children(parent_of(i));
        let pos = sibs.iter().position(|&s| s == i).unwrap_or(0);
        let mut d = format!(
            "<</Title {}/Parent {parent} 0 R/Dest[@{page}/XYZ 0 {top:.0} null]",
            text(title)
        );
        if pos > 0 {
            let _ = write!(d, "/Prev {} 0 R", nums[sibs[pos - 1]]);
        }
        if pos + 1 < sibs.len() {
            let _ = write!(d, "/Next {} 0 R", nums[sibs[pos + 1]]);
        }
        let kids = children(Some(i));
        if let (Some(f), Some(l)) = (kids.first(), kids.last()) {
            let _ = write!(
                d,
                "/First {} 0 R/Last {} 0 R/Count {}",
                nums[*f],
                nums[*l],
                kids.len()
            );
        }
        d.push_str(">>");
        pdf.set(nums[i], d);
    }
    let top = children(None);
    *root_children = top.iter().map(|&i| nums[i]).collect();
    pdf.set(
        root,
        format!(
            "<</Type/Outlines/First {} 0 R/Last {} 0 R/Count {}>>",
            nums[top[0]],
            nums[*top.last().unwrap_or(&0)],
            top.len()
        ),
    );
    root
}

fn fix_outline_refs(pdf: &mut Pdf, refs: &[usize]) {
    for o in &mut pdf.objs {
        if o.contains('@') && o.starts_with("<</Title") {
            *o = resolve_targets(o, refs);
        }
    }
}

pub fn lease() -> Vec<u8> {
    let (w, h) = (595.0, 842.0);
    let mut tops = Vec::new();
    let mut p1 = Page::new(w, h);
    p1.heading("RESIDENTIAL LEASE AGREEMENT", 18.0);
    p1.para("This agreement is made on 1 November 2026 between the parties named below. It sets out the terms on which the premises are let.", 11.0);
    tops.push(("1. Parties", 0, 0, p1.heading("1. Parties", 13.0)));
    p1.para("Landlord: Example Lettings Ltd, 12 Harbour Street, Portsmouth. Tenant: Alex Morgan, currently residing at 4 Elm Close, Winchester.", 11.0);
    tops.push(("2. Term", 0, 0, p1.heading("2. Term", 13.0)));
    p1.para("The tenancy begins on 1 December 2026 and runs for an initial term of twelve months. After the initial term it continues from month to month until either party ends it.", 11.0);
    tops.push(("3. Rent", 0, 0, p1.heading("3. Rent", 13.0)));
    p1.para("The tenant agrees to pay the monthly rent of 1,250 euros on the first day of each month by bank transfer to the account named in Schedule A. Rent paid more than five days late incurs a fee of 5% of the monthly amount.", 11.0);
    p1.link(
        "See Schedule A for the bank details.",
        11.0,
        "/Dest[@3/XYZ 0 770 null]",
    );
    p1.link(
        "Guidance for tenants: https://example.org/renting-guide",
        11.0,
        "/A<</S/URI/URI(https://example.org/renting-guide)>>",
    );
    p1.footer("Page 1 of 4");

    let mut p2 = Page::new(w, h);
    tops.push(("4. Deposit", 0, 1, p2.heading("4. Deposit", 13.0)));
    p2.para("A security deposit equal to one month of rent is due on signing. It is returned within thirty days of the end of the tenancy, less any amount needed to repair damage beyond normal wear.", 11.0);
    tops.push((
        "5. Use of the premises",
        0,
        1,
        p2.heading("5. Use of the premises", 13.0),
    ));
    p2.para("The premises are to be used as a private residence only. The tenant shall not sublet any part of the premises without the landlord's written consent.", 11.0);
    tops.push(("6. Maintenance", 0, 1, p2.heading("6. Maintenance", 13.0)));
    p2.para("The landlord is responsible for structural repairs, heating and plumbing. The tenant keeps the premises clean and reports any damage promptly. Small repairs below 100 euros are made by the tenant.", 11.0);
    p2.para("The landlord may inspect the premises twice a year after giving at least two days of notice. Urgent repairs may be carried out without notice.", 11.0);
    p2.footer("Page 2 of 4");

    let mut p3 = Page::new(w, h);
    tops.push(("7. Notice", 0, 2, p3.heading("7. Notice", 13.0)));
    p3.para("Either party may end this agreement after the initial term by giving two full months of written notice.", 11.0);
    tops.push(("8. Keys", 0, 2, p3.heading("8. Keys", 13.0)));
    p3.para(
        "The tenant receives two sets of keys and returns all keys at the end of the tenancy.",
        11.0,
    );
    tops.push((
        "9. Entire agreement",
        0,
        2,
        p3.heading("9. Entire agreement", 13.0),
    ));
    p3.para(
        "This document and its schedules are the whole agreement between the parties.",
        11.0,
    );
    p3.y -= 40.0;
    tops.push(("Signatures", 0, 2, p3.heading("Signatures", 13.0)));
    p3.y -= 60.0;
    let y = p3.y;
    p3.rule(MARGIN, MARGIN + 190.0, y);
    p3.rule(w - MARGIN - 190.0, w - MARGIN, y);
    p3.line_at("F1", 10.0, MARGIN, y - 14.0, "Tenant: Alex Morgan");
    p3.line_at(
        "F1",
        10.0,
        w - MARGIN - 190.0,
        y - 14.0,
        "Landlord: Example Lettings Ltd",
    );
    p3.footer("Page 3 of 4");

    let mut p4 = Page::new(w, h);
    tops.push((
        "Schedule A",
        0,
        3,
        p4.heading("Schedule A: payment details", 13.0),
    ));
    p4.para("Account holder: Example Lettings Ltd. IBAN: XX00 0000 0000 0000 0000 00. Reference: the tenant's name and the month paid.", 11.0);
    p4.heading("Inventory", 13.0);
    for item in [
        "Sofa, grey fabric, three seats",
        "Dining table with four chairs",
        "Double bed with mattress",
        "Washing machine and fridge freezer",
        "Curtains in every room",
    ] {
        p4.para(&format!("- {item}"), 11.0);
    }
    p4.footer("Page 4 of 4");

    let items: Vec<(&str, usize, usize, f64)> =
        tops.iter().map(|(t, d, p, y)| (*t, *d, *p, *y)).collect();
    build(
        vec![p1, p2, p3, p4],
        &[
            ("Title", "Residential lease agreement"),
            ("Author", "Example Lettings Ltd"),
            ("Subject", "Lease for 4 Elm Close"),
        ],
        move |pdf, refs, _| {
            let mut top = Vec::new();
            let root = outline(pdf, &items, &mut top);
            fix_outline_refs(pdf, refs);
            format!("/Outlines {root} 0 R/PageMode/UseOutlines")
        },
    )
}

pub fn report() -> Vec<u8> {
    let (w, h) = (612.0, 792.0);
    let mut pages = Vec::new();
    let mut cover = Page::new(w, h);
    cover.y -= 180.0;
    cover.heading("Quarterly report", 28.0);
    cover.para("Third quarter 2026 · Northwind Garden Supplies", 14.0);
    cover.para("Prepared for the board. Figures are unaudited.", 11.0);
    pages.push(cover);
    let mut contents = Page::new(w, h);
    contents.heading("Contents", 18.0);
    let sections = [
        ("Summary", 2),
        ("Revenue", 3),
        ("Costs", 5),
        ("Customers", 7),
        ("Outlook", 9),
        ("Appendix: tables", 11),
    ];
    for (name, page) in sections {
        contents.link(
            &format!("{name} ........ {}", page - 1),
            12.0,
            &format!(
                "/Dest/{}",
                name.split(':').next().unwrap_or(name).replace(' ', "")
            ),
        );
    }
    pages.push(contents);
    let topics = [
        "Revenue grew in every region this quarter, led by online orders of seeds and tools. Revenue from garden furniture was flat, as expected for the season.",
        "Costs rose with shipping prices. We renegotiated two carrier contracts, which lowers costs from the next quarter on. Staff costs stayed within the budget.",
        "Customer numbers reached a new high. Returning customers bought more often, and the share of revenue from subscriptions doubled.",
        "The outlook for the fourth quarter is cautious: winter is our quiet season, so revenue will fall, but the margin should improve with lower costs.",
    ];
    let mut tops = Vec::new();
    for i in 0..10 {
        let mut p = Page::new(w, h);
        let section = match i {
            0 | 1 => "Summary",
            2 | 3 => "Revenue",
            4 | 5 => "Costs",
            6 | 7 => "Customers",
            8 => "Outlook",
            _ => "Appendix: tables",
        };
        let top = p.heading(
            &format!("{section}{}", if i % 2 == 1 { " (continued)" } else { "" }),
            16.0,
        );
        if i % 2 == 0 || i == 9 {
            tops.push((section, i + 2, top));
        }
        for k in 0..5 {
            p.para(topics[(i + k) % topics.len()], 11.0);
        }
        p.footer(&format!("{}", i + 1));
        pages.push(p);
    }
    let dests: Vec<String> = sections
        .iter()
        .map(|(name, page)| {
            let key = name.split(':').next().unwrap_or(name).replace(' ', "");
            let top = tops.iter().find(|t| t.0 == *name).map_or(720.0, |t| t.2);
            format!("/{key}[@{page}/XYZ 0 {top:.0} null]")
        })
        .collect();
    let mut items: Vec<(&str, usize, usize, f64)> = Vec::new();
    for (name, page, top) in &tops {
        let depth = usize::from(matches!(*name, "Revenue" | "Costs"));
        if depth == 1 && !items.iter().any(|i| i.0 == "Results") {
            items.push(("Results", 0, *page, *top));
        }
        items.push((name, depth, *page, *top));
    }
    build(
        pages,
        &[
            ("Title", "Quarterly report Q3 2026"),
            ("Author", "Finance team"),
            ("Keywords", "revenue, costs, outlook"),
        ],
        move |pdf, refs, _| {
            let mut top = Vec::new();
            let root = outline(pdf, &items, &mut top);
            fix_outline_refs(pdf, refs);
            format!(
                "/Outlines {root} 0 R/Dests<<{}>>/PageLabels<</Nums[0<</S/r>>2<</S/D>>]>>",
                dests.join("")
            )
        },
    )
}

pub fn form() -> Vec<u8> {
    let (w, h) = (595.0, 842.0);
    let mut p = Page::new(w, h);
    p.heading("Membership application", 20.0);
    p.para("Please fill in your details. We use them only to manage your membership of the community garden.", 11.0);
    let (x0, x1) = (MARGIN + 150.0, w - MARGIN);
    let widget = |rest: &str, r: [f64; 4]| {
        // A grey border unless the widget brings its own /MK.
        let mk = if rest.contains("/MK") {
            ""
        } else {
            "/MK<</BC[0.6 0.6 0.6]>>"
        };
        format!(
            "<</Type/Annot/Subtype/Widget/F 4/Rect[{:.1} {:.1} {:.1} {:.1}]{mk}{rest}>>",
            r[0], r[1], r[2], r[3]
        )
    };
    // Rows of a label and a field; `extra` is the field's own entries.
    let row = |p: &mut Page, label: &str, height: f64, width: f64, extra: &str| {
        let y = p.y - 18.0;
        p.line_at("F1", 11.0, MARGIN, y, label);
        let top = y + 14.0;
        p.annots.push(widget(
            &format!("{extra}/DA(/Helv 11 Tf 0 g)"),
            [x0, top - height, x0 + width, top],
        ));
        p.y -= height + 14.0;
    };
    row(
        &mut p,
        "Full name",
        20.0,
        x1 - x0,
        "/FT/Tx/T(name)/TU(Full name)",
    );
    row(
        &mut p,
        "Email address",
        20.0,
        x1 - x0,
        "/FT/Tx/T(email)/AA<</F<</S/JavaScript/JS(event.value = event.value.toLowerCase();)>>>>",
    );
    row(&mut p, "Date of birth", 20.0, x1 - x0, "/FT/Tx/T(birth)");
    row(
        &mut p,
        "Plot number wanted",
        20.0,
        x1 - x0,
        "/FT/Tx/T(plot)",
    );
    row(
        &mut p,
        "Postcode",
        20.0,
        132.0,
        "/FT/Tx/T(postcode)/Ff 16777216/MaxLen 6",
    );
    // A radio group: the widgets are kids of one field, added after the page.
    let y = p.y - 18.0;
    p.line_at("F1", 11.0, MARGIN, y, "Plot size");
    for (i, size) in ["Small", "Medium", "Large"].iter().enumerate() {
        let x = x0 + i as f64 * 100.0;
        p.annots.push(widget(
            &format!("/Parent PARENT 0 R/AS/Off/MK<</BC[0.6 0.6 0.6]/CA(l)>>/AP<</N<</{size} DOT 0 R/Off BLANK 0 R>>>>"),
            [x, y - 3.0, x + 14.0, y + 11.0],
        ));
        p.line_at("F1", 11.0, x + 20.0, y, size);
    }
    p.y -= 34.0;
    row(
        &mut p,
        "Preferred time",
        20.0,
        200.0,
        "/FT/Ch/T(time)/Ff 131072/Opt[(Weekday mornings)(Weekday evenings)(Weekends)]",
    );
    row(
        &mut p,
        "Interests",
        66.0,
        200.0,
        "/FT/Ch/T(interests)/Ff 2097152/Opt[[(veg)(Vegetables)][(fruit)(Fruit trees)][(herbs)(Herbs)][(bees)(Beekeeping)]]",
    );
    row(
        &mut p,
        "Anything else",
        54.0,
        x1 - x0,
        "/FT/Tx/T(notes)/Ff 4096",
    );
    let y = p.y - 18.0;
    p.line_at(
        "F1",
        11.0,
        MARGIN + 22.0,
        y,
        "Send me the monthly newsletter",
    );
    p.annots.push(widget(
        "/FT/Btn/T(newsletter)/V/Off/AS/Off",
        [MARGIN, y - 3.0, MARGIN + 14.0, y + 11.0],
    ));
    p.y -= 76.0;
    let y = p.y;
    p.rule(MARGIN, MARGIN + 220.0, y);
    p.line_at("F1", 10.0, MARGIN, y - 14.0, "Signature of the applicant");
    p.annots.push(widget(
        "/FT/Sig/T(applicant)",
        [MARGIN, y, MARGIN + 220.0, y + 40.0],
    ));
    p.footer("Community garden association");
    // Widgets are numbered right after the page and its contents; then the radio group
    // and the radio buttons' off and on appearances.
    let n = p.annots.len();
    let first = 6;
    let (parent, blank, dot) = (first + n, first + n + 1, first + n + 2);
    for a in &mut p.annots {
        *a = a
            .replace("PARENT", &parent.to_string())
            .replace("BLANK", &blank.to_string())
            .replace("DOT", &dot.to_string());
    }
    let kids: Vec<usize> = (0..n)
        .filter(|&i| p.annots[i].contains("/Parent"))
        .map(|i| first + i)
        .collect();
    build(
        vec![p],
        &[("Title", "Membership application")],
        move |pdf, refs, _| {
            assert_eq!(refs[0] + 2, first);
            let list: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
            let group = pdf.add(format!(
                "<</FT/Btn/T(size)/Ff 49152/V/Off/Kids[{}]>>",
                list.join(" ")
            ));
            // Both states draw the ring (in the /MK border colour), so the buttons show
            // before one is chosen; the chosen one adds a dot.
            let ring = format!("0.6 0.6 0.6 RG 1 w {} s", circle(7.0, 7.0, 6.0));
            let mark = format!("{ring} 0.15 0.15 0.18 rg {} f", circle(7.0, 7.0, 3.0));
            let appearance = |content: &str| {
                format!(
                    "<</Type/XObject/Subtype/Form/BBox[0 0 14 14]/Length {}>>stream\n{content}\nendstream",
                    content.len()
                )
            };
            let empty = pdf.add(appearance(&ring));
            let on = pdf.add(appearance(&mark));
            assert_eq!((group, empty, on), (parent, blank, dot));
            let fields: Vec<String> = (first..first + n)
                .filter(|i| !kids.contains(i))
                .chain([parent])
                .map(|i| format!("{i} 0 R"))
                .collect();
            format!(
                "/Lang(en-US)/AcroForm<</Fields[{}]/NeedAppearances true/DA(/Helv 11 Tf 0 g)/DR<</Font<</Helv<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>/ZaDb<</Type/Font/Subtype/Type1/BaseFont/ZapfDingbats>>>>>>>>",
                fields.join(" ")
            )
        },
    )
}

/// A closed circle path (four Bézier quarters) for a content stream.
fn circle(cx: f64, cy: f64, r: f64) -> String {
    let k = r * 0.552_284_75;
    let pt = |(x, y): (f64, f64)| format!("{} {}", cx + x, cy + y);
    let mut path = format!("{} m", pt((r, 0.0)));
    let quarters = [
        [(r, k), (k, r), (0.0, r)],
        [(-k, r), (-r, k), (-r, 0.0)],
        [(-r, -k), (-k, -r), (0.0, -r)],
        [(k, -r), (r, -k), (r, 0.0)],
    ];
    for [a, b, c] in quarters {
        path += &format!(" {} {} {} c", pt(a), pt(b), pt(c));
    }
    path
}

fn protect(bytes: Vec<u8>, user: &str, owner: &str, p: Permissions) -> Vec<u8> {
    let mut d = Document::open(bytes).expect("sample parses");
    d.set_password(user, owner, p).expect("protect");
    d.save_full().expect("save")
}

/// A photographed signature: dark blue ink on uneven beige paper.
pub fn signature_png() -> Vec<u8> {
    let (w, h) = (900u32, 320u32);
    let mut px = vec![0u8; (w * h * 3) as usize];
    let mut seed = 0x2545_F491u32;
    for y in 0..h {
        for x in 0..w {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed % 9) as f64 - 4.0;
            let shade =
                1.0 - 0.10 * (f64::from(x) / f64::from(w)) - 0.06 * (f64::from(y) / f64::from(h));
            let i = ((y * w + x) * 3) as usize;
            px[i] = (239.0 * shade + noise) as u8;
            px[i + 1] = (232.0 * shade + noise) as u8;
            px[i + 2] = (216.0 * shade + noise) as u8;
        }
    }
    // A cursive scribble: loops riding on a gentle wave.
    let mut pts = Vec::new();
    for k in 0..=1400 {
        let t = f64::from(k) / 1400.0;
        let x = 90.0 + 700.0 * t + 28.0 * (t * 38.0).sin();
        let y =
            175.0 - 70.0 * (t * 9.0).sin() * (1.0 - t * 0.6) - 30.0 * (t * 38.0).cos() + 20.0 * t;
        pts.push((x, y));
    }
    for (x, y) in pts {
        let r = 3.6;
        for dy in -5..=5 {
            for dx in -5..=5 {
                let (fx, fy) = (x + f64::from(dx), y + f64::from(dy));
                let d = ((fx - x).powi(2) + (fy - y).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                if a <= 0.0 || fx < 0.0 || fy < 0.0 || fx >= f64::from(w) || fy >= f64::from(h) {
                    continue;
                }
                let i = ((fy as u32 * w + fx as u32) * 3) as usize;
                for (c, ink) in [22.0, 40.0, 110.0].iter().enumerate() {
                    let v = f64::from(px[i + c]);
                    px[i + c] = (v + (ink - v) * a * 0.9) as u8;
                }
            }
        }
    }
    encode_png(w, h, &px)
}

/// A simple picture (a bar chart) to insert as a page.
pub fn picture_png() -> Vec<u8> {
    let (w, h) = (640u32, 420u32);
    let mut px = vec![255u8; (w * h * 3) as usize];
    let bars = [
        (0x60, 0xA5, 0xFA, 0.55),
        (0x4A, 0xDE, 0x80, 0.8),
        (0xFB, 0x92, 0x3C, 0.4),
        (0xF4, 0x72, 0xB6, 0.7),
    ];
    for (i, (r, g, b, v)) in bars.iter().enumerate() {
        let x0 = 80 + i as u32 * 130;
        let top = (f64::from(h - 60) * (1.0 - v)) as u32 + 30;
        for y in top..h - 30 {
            for x in x0..x0 + 90 {
                let k = ((y * w + x) * 3) as usize;
                px[k..k + 3].copy_from_slice(&[*r, *g, *b]);
            }
        }
    }
    for x in 50..w - 30 {
        let k = (((h - 30) * w + x) * 3) as usize;
        px[k..k + 3].copy_from_slice(&[40, 40, 46]);
    }
    encode_png(w, h, &px)
}

fn encode_png(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
    let opts = pagebara_png::EncodeOptions {
        level: Some(6),
        ..Default::default()
    };
    pagebara_png::encode_with(w, h, pagebara_png::ColorType::Rgb, px, &opts).expect("png encode")
}

pub fn write_all(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let lease = lease();
    let mut no_print = Permissions::ALL;
    no_print.print = false;
    no_print.print_high_quality = false;
    let mut restricted = Permissions::ALL;
    restricted.assemble = false;
    restricted.annotate = false;
    restricted.modify = false;
    let mut files: Vec<(&str, Vec<u8>)> = vec![
        ("lease.pdf", lease.clone()),
        ("report.pdf", report()),
        ("form.pdf", form()),
        (
            "lease-protected.pdf",
            protect(lease.clone(), "user", "owner", no_print),
        ),
        (
            "lease-unicode.pdf",
            protect(lease.clone(), "pässwörd 密码 🔐", "owner", Permissions::ALL),
        ),
        (
            "lease-restricted.pdf",
            protect(lease, "", "owner", restricted),
        ),
        ("signature-photo.png", signature_png()),
        ("chart.png", picture_png()),
    ];
    files.extend(more::files());
    files.push(("live-text.pdf", live::live_text()));
    // The same marked as Greek: live text offers the Greek models.
    files.push(("live-text-greek.pdf", live::live_text_in(Some("el"))));
    files.push(("tagged.pdf", tagged::tagged()));
    files.push(("flyer.pdf", edit::flyer()));
    files.push(("printed-form.pdf", printed::printed_form()));
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_open_with_text_outline_and_links() {
        let d = Document::open(lease()).unwrap();
        let s = pagebara_app_core::session::Session::new(None, d);
        assert_eq!(s.page_count(), 4);
        assert_eq!(s.outline.len(), 11);
        assert!(s.outline.iter().all(|o| o.page.is_some()));
        assert!(s.text.page(0).text.contains("RESIDENTIAL LEASE AGREEMENT"));
        let links = pagebara_app_core::annots::on_page(s.doc(), s.page_ref(0).unwrap());
        assert_eq!(links.len(), 2);

        let r = pagebara_app_core::session::Session::new(None, Document::open(report()).unwrap());
        assert_eq!(r.page_count(), 12);
        assert_eq!(r.page_label(0), "i");
        assert_eq!(r.page_label(2), "1");
        assert!(r.outline.len() >= 6);

        let f = pagebara_app_core::session::Session::new(None, Document::open(form()).unwrap());
        let form = pagebara_app_core::forms::FormBackend::load(
            &pagebara_app_core::forms::EngineForms,
            f.doc(),
        );
        let kinds: Vec<_> = form.fields().iter().map(|f| f.kind).collect();
        use pagebara_app_core::forms::FieldKind as K;
        assert_eq!(
            kinds,
            [
                K::Text,
                K::Text,
                K::Text,
                K::Text,
                K::Text,
                K::ComboBox,
                K::ListBox,
                K::Text,
                K::CheckBox,
                K::Signature,
                K::Radio
            ]
        );
        assert!(form.can_fill(f.doc()));
        assert_eq!(form.script_warnings().len(), 1);
        assert!(matches!(
            Document::open(protect(lease(), "user", "owner", Permissions::ALL)),
            Err(pagebara_syntax::Error::PasswordRequired(_))
        ));
        assert!(pagebara_signatures::decode_image(&signature_png()).is_ok());
    }
}
