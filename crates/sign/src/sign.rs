//! Signing: a signature field with its widget, a reserved `/Contents`, an incremental
//! save, the byte range, and the CMS signature written into the reserved space.

use std::io::Write as _;

use pagebara_annot::{Glyph, Line, Shaper, TextFont};
use pagebara_forms::Form;
use pagebara_syntax::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::cms::Builder;
use crate::crypto::DigestAlgorithm;
use crate::identity::Identity;
use crate::time::Time;
use crate::tsp::{self, TimestampAuthority};
use crate::{Error, Result, oid};

/// The signature format (`/SubFilter`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SubFilter {
    /// PAdES baseline: CAdES with the signing-certificate-v2 attribute, no signing-time
    /// attribute (the time is `/M`, or a timestamp).
    #[default]
    CadesDetached,
    /// The classic PKCS #7 detached signature, readable by every PDF reader.
    Pkcs7Detached,
}

/// What a certified document still allows (DocMDP `/P`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdpPermissions {
    /// No changes at all (LTV data and document timestamps excepted).
    NoChanges = 1,
    /// Filling in forms and signing.
    FormFilling = 2,
    /// Also adding and editing comments.
    Annotations = 3,
}

/// A visible signature on a page. It is drawn upright as the page is shown: on pages with
/// a `/Rotate`, the appearance is turned against it and the widget's `/MK /R` says so.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    /// Page index (0-based).
    pub page: usize,
    /// `[x0, y0, x1, y1]` in page space (unrotated user space); ignored when signing an
    /// existing field that has a widget.
    pub rect: [f64; 4],
    /// An image XObject already in the document, e.g. the ink signature from
    /// `pagebara_signatures::embed_signature`; drawn on the left, or alone without text.
    pub image: Option<ObjRef>,
    /// Write who signed, when, and the reason and location.
    pub text: bool,
}

/// How to sign.
#[derive(Clone, Copy, Default)]
pub struct SignOptions<'a> {
    /// The signature field: an existing unsigned one, or the name of a new one
    /// (`Signature1`, `Signature2`, … when `None`).
    pub field: Option<&'a str>,
    /// Visible appearance; `None` for an invisible signature.
    pub appearance: Option<&'a Appearance>,
    /// The signer's name (`/Name`); the certificate's name when `None`.
    pub name: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub location: Option<&'a str>,
    pub contact_info: Option<&'a str>,
    pub sub_filter: SubFilter,
    /// The key's natural digest when `None` (SHA-256, SHA-384 for P-384).
    pub digest: Option<DigestAlgorithm>,
    /// RSA-PSS instead of PKCS #1 v1.5 for RSA keys.
    pub pss: bool,
    /// Certify the document (only as its first signature).
    pub certify: Option<MdpPermissions>,
    /// Lock the signed field and the listed others against later filling (FieldMDP).
    pub lock_fields: Option<&'a [&'a str]>,
    /// Timestamp the signature (PAdES B-T).
    pub timestamp: Option<&'a dyn TimestampAuthority>,
    /// The signing time written to `/M`; now when `None`.
    pub time: Option<Time>,
    /// Bytes to reserve for the signature; estimated when `None`.
    pub reserve: Option<usize>,
    /// The font of the appearance text. When `None`: Helvetica if it shows every
    /// character, else subsets of the system's fonts ([`pagebara_annot::default_font`]).
    pub font: Option<&'a dyn TextFont>,
}

impl std::fmt::Debug for SignOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignOptions")
            .field("field", &self.field)
            .field("sub_filter", &self.sub_filter)
            .field("certify", &self.certify)
            .field("timestamp", &self.timestamp.is_some())
            .finish_non_exhaustive()
    }
}

const RANGE_PLACEHOLDER: &str = "[0 1111111111 2222222222 3333333333]";

/// Signs `doc` (with any unsaved edits, which become part of the signed revision) and
/// returns the new file: the original bytes followed by an incremental update.
pub fn sign(doc: &Document, identity: &Identity, opts: &SignOptions<'_>) -> Result<Vec<u8>> {
    let digest = opts.digest.unwrap_or_else(|| identity.key.default_digest());
    let mut reserve = opts.reserve.unwrap_or_else(|| {
        let certs: usize = identity.chain.iter().map(|c| c.der().len()).sum::<usize>()
            + identity.certificate.der().len();
        certs + 3000 + if opts.timestamp.is_some() { 8000 } else { 0 }
    });
    loop {
        let mut working = prepare(doc, identity, opts, reserve)?;
        let time = opts.time.unwrap_or_else(Time::now);
        let mut made = None;
        let out = finish(&mut working, |data| {
            let hash = digest.digest(data);
            let builder = Builder {
                key: &identity.key,
                cert: &identity.certificate,
                chain: &identity.chain,
                digest,
                pss: opts.pss,
                content_type: oid::DATA,
                content: None,
                content_hash: &hash,
                signing_time: (opts.sub_filter == SubFilter::Pkcs7Detached).then_some(time),
                signing_certificate_v2: opts.sub_filter == SubFilter::CadesDetached,
            };
            let cms = builder.build(&mut |signature| match opts.timestamp {
                Some(tsa) => {
                    let token = tsp::timestamp_data(tsa, digest, &[signature])?;
                    Ok(vec![crate::cms::attr(&oid::SIGNATURE_TIMESTAMP, &token)])
                }
                None => Ok(Vec::new()),
            })?;
            made = Some(cms.len());
            Ok(cms)
        });
        match out {
            Err(Error::Unsupported(ref m)) if m == TOO_SMALL => {
                reserve = made.unwrap_or(reserve * 2) + 2048;
            }
            other => return other,
        }
    }
}

pub(crate) const TOO_SMALL: &str = "signature larger than the reserved space";

/// The document ready for saving: field, widget and signature dictionary with
/// placeholders.
fn prepare(
    doc: &Document,
    identity: &Identity,
    opts: &SignOptions<'_>,
    reserve: usize,
) -> Result<Document> {
    let mut doc = base_for_update(doc)?;
    let form = Form::load(&doc);
    let infos = form.signatures(&doc);
    let signed: Vec<_> = infos.iter().filter(|i| i.signed).collect();
    if signed.iter().any(|i| i.doc_mdp == Some(1)) {
        return Err(Error::NotPermitted);
    }
    if opts.certify.is_some() && signed.iter().any(|i| !i.timestamp) {
        return Err(Error::Unsupported(
            "only the first signature can certify a document".into(),
        ));
    }
    let time = opts.time.unwrap_or_else(Time::now);
    let name = opts
        .name
        .map(str::to_string)
        .unwrap_or_else(|| identity.certificate.display_name());
    let mut v = Dict::new();
    v.set(b"Type", Object::name(b"Sig"));
    v.set(b"Filter", Object::name(b"Adobe.PPKLite"));
    v.set(
        b"SubFilter",
        Object::name(match opts.sub_filter {
            SubFilter::CadesDetached => b"ETSI.CAdES.detached",
            SubFilter::Pkcs7Detached => b"adbe.pkcs7.detached",
        }),
    );
    v.set(b"Name", text(&name));
    v.set(b"M", Object::string(time.to_pdf().as_bytes()));
    for (k, val) in [
        (&b"Reason"[..], opts.reason),
        (b"Location", opts.location),
        (b"ContactInfo", opts.contact_info),
    ] {
        if let Some(t) = val {
            v.set(k, text(t));
        }
    }
    let mut refs = Vec::new();
    if let Some(p) = opts.certify {
        let mut params = Dict::new();
        params.set(b"Type", Object::name(b"TransformParams"));
        params.set(b"P", p as i64);
        params.set(b"V", Object::name(b"1.2"));
        let mut r = Dict::new();
        r.set(b"Type", Object::name(b"SigRef"));
        r.set(b"TransformMethod", Object::name(b"DocMDP"));
        r.set(b"TransformParams", params);
        refs.push(Object::Dict(r));
    }
    let field_name = choose_field_name(&form, &infos, opts.field)?;
    if let Some(locked) = opts.lock_fields {
        let mut params = Dict::new();
        params.set(b"Type", Object::name(b"TransformParams"));
        params.set(b"Action", Object::name(b"Include"));
        let mut names: Vec<Object> = locked.iter().map(|n| text(n)).collect();
        names.push(text(&field_name));
        params.set(b"Fields", names);
        params.set(b"V", Object::name(b"1.2"));
        let mut r = Dict::new();
        r.set(b"Type", Object::name(b"SigRef"));
        r.set(b"TransformMethod", Object::name(b"FieldMDP"));
        r.set(b"TransformParams", params);
        refs.push(Object::Dict(r));
    }
    if !refs.is_empty() {
        v.set(b"Reference", refs);
    }
    add_placeholders(&mut v, reserve);
    let sig = doc.add(v);
    if opts.certify.is_some() {
        let root = catalog_ref(&doc)?;
        let mut cat = dict_of(&doc, root)?;
        let mut perms = Dict::new();
        perms.set(b"DocMDP", sig);
        cat.set(b"Perms", perms);
        doc.set(root, cat);
    }
    let ap = |doc: &mut Document, rect: [f64; 4], rot: u16| -> Result<Option<ObjRef>> {
        match opts.appearance {
            Some(a) => Ok(Some(appearance_stream(
                doc, a, rect, rot, &name, time, opts,
            )?)),
            None => Ok(None),
        }
    };
    let existing = form.field(&field_name).cloned();
    match existing {
        Some(f) => {
            if f.kind != pagebara_forms::FieldKind::Signature {
                return Err(Error::NotASignatureField(field_name));
            }
            if infos.iter().any(|i| i.field == field_name && i.signed) {
                return Err(Error::AlreadySigned(field_name));
            }
            let mut fd = dict_of(&doc, f.id)?;
            fd.set(b"V", sig);
            if opts.lock_fields.is_some() {
                let ff = fd.int(b"Ff").unwrap_or(0);
                fd.set(b"Ff", ff | 1);
            }
            doc.set(f.id, fd);
            if let Some(w) = f.widgets.first() {
                let r = w.rect;
                let rect = [r.x0, r.y0, r.x1, r.y1];
                // The widget's own rotation wins over the page's.
                let rot = match w.mk.rotation {
                    0 => w.page_ref.map_or(0, |p| page_rotation(&doc, p)),
                    r => r,
                };
                if rect[2] > rect[0]
                    && rect[3] > rect[1]
                    && let Some(n) = ap(&mut doc, rect, rot)?
                {
                    let mut wd = dict_of(&doc, w.id)?;
                    let mut apd = Dict::new();
                    apd.set(b"N", n);
                    wd.set(b"AP", apd);
                    set_mk_rotation(&doc, &mut wd, rot);
                    doc.set(w.id, wd);
                }
            }
            set_sig_flags(&mut doc, None)?;
        }
        None => {
            let page = opts.appearance.map_or(0, |a| a.page);
            let rect = opts.appearance.map_or([0.0; 4], |a| a.rect);
            let visible = rect[2] > rect[0] && rect[3] > rect[1];
            let rot = match pagebara_pages::PageTree::load(&doc).page(page) {
                Some(p) if visible => page_rotation(&doc, p.r),
                _ => 0,
            };
            let n = if visible {
                ap(&mut doc, rect, rot)?
            } else {
                None
            };
            let w = add_field(
                &mut doc,
                &field_name,
                sig,
                page,
                rect,
                n,
                opts.lock_fields.is_some(),
            )?;
            if rot != 0 {
                let mut wd = dict_of(&doc, w)?;
                set_mk_rotation(&doc, &mut wd, rot);
                doc.set(w, wd);
            }
        }
    }
    Ok(doc)
}

/// Adds a signature field (merged with its widget) holding `sig` on page `page`, with
/// the appearance `ap` (an empty one when `None`).
pub(crate) fn add_field(
    doc: &mut Document,
    name: &str,
    sig: ObjRef,
    page: usize,
    rect: [f64; 4],
    ap: Option<ObjRef>,
    lock: bool,
) -> Result<ObjRef> {
    let tree = pagebara_pages::PageTree::load(doc);
    let page = tree.page(page).ok_or(Error::NoSuchPage(page))?.r;
    let mut w = Dict::new();
    w.set(b"Type", Object::name(b"Annot"));
    w.set(b"Subtype", Object::name(b"Widget"));
    w.set(b"FT", Object::name(b"Sig"));
    w.set(b"T", text(name));
    // Print and Locked.
    w.set(b"F", 132);
    w.set(
        b"Rect",
        rect.iter().map(|&x| Object::Real(x)).collect::<Vec<_>>(),
    );
    w.set(b"P", page);
    w.set(b"V", sig);
    if lock {
        w.set(b"Ff", 1);
    }
    let n = match ap {
        Some(n) => n,
        None => {
            // Invisible: an empty appearance keeps PDF/A and strict readers happy.
            let mut d = Dict::new();
            d.set(b"Type", Object::name(b"XObject"));
            d.set(b"Subtype", Object::name(b"Form"));
            d.set(b"BBox", vec![Object::Int(0); 4]);
            doc.add(Stream::new(d, Vec::new()))
        }
    };
    let mut apd = Dict::new();
    apd.set(b"N", n);
    w.set(b"AP", apd);
    let wr = doc.add(w);
    add_to_array(doc, page, b"Annots", wr)?;
    set_sig_flags(doc, Some(wr))?;
    Ok(wr)
}

/// The page's `/Rotate` (inherited) as 0, 90, 180 or 270.
fn page_rotation(doc: &Document, page: ObjRef) -> u16 {
    doc.get(page)
        .and_then(|p| p.as_dict().map(|d| pagebara_annot::page_rotation(doc, d)))
        .unwrap_or(0)
}

/// Records the counter-clockwise rotation of the widget's contents in `/MK /R`.
fn set_mk_rotation(doc: &Document, widget: &mut Dict, rot: u16) {
    let mut mk = doc
        .get_in(widget, b"MK")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    if rot == 0 && mk.get(b"R").is_none() {
        return;
    }
    mk.set(b"R", i64::from(rot));
    widget.set(b"MK", mk);
}

/// A field name not yet used: `{prefix}1`, `{prefix}2`, …
pub(crate) fn unused_name(form: &Form, prefix: &str) -> String {
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|n| form.field(n).is_none())
        .unwrap_or_default()
}

/// A document whose saving appends to the original file: repaired files are rewritten
/// first (possible only while unsigned).
pub(crate) fn base_for_update(doc: &Document) -> Result<Document> {
    match doc.save_incremental() {
        Ok(_) => Ok(doc.clone()),
        Err(pagebara_syntax::Error::NeedsFullSave) => {
            let signed = Form::load(doc).signatures(doc).iter().any(|i| i.signed);
            if signed || doc.is_encrypted() {
                return Err(Error::Unsupported(
                    "this file needs a full rewrite, which would break its signatures".into(),
                ));
            }
            Ok(Document::open(doc.save_full()?)?)
        }
        Err(e) => Err(e.into()),
    }
}

fn choose_field_name(
    form: &Form,
    infos: &[pagebara_forms::SignatureInfo],
    wanted: Option<&str>,
) -> Result<String> {
    if let Some(w) = wanted {
        return Ok(w.to_string());
    }
    // The first empty signature field with a widget, else a new name.
    let empty = form
        .signature_fields()
        .into_iter()
        .find(|f| !f.widgets.is_empty() && infos.iter().any(|i| i.field == f.name && !i.signed));
    match empty {
        Some(f) => Ok(f.name.clone()),
        None => Ok(unused_name(form, "Signature")),
    }
}

pub(crate) fn add_placeholders(v: &mut Dict, reserve: usize) {
    v.set(
        b"ByteRange",
        RANGE_PLACEHOLDER
            .trim_matches(['[', ']'])
            .split(' ')
            .map(|n| Object::Int(n.parse().unwrap_or(0)))
            .collect::<Vec<_>>(),
    );
    v.set(
        b"Contents",
        Object::String(PdfString {
            bytes: vec![0; reserve],
            hex: true,
        }),
    );
}

/// Saves `doc` incrementally, fills in the byte range, and writes what `make` returns for
/// the signed data into `/Contents`.
pub(crate) fn finish(
    doc: &mut Document,
    make: impl FnOnce(&[&[u8]]) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let base = doc.source().len();
    let mut out = doc.save_incremental()?;
    let find = |hay: &[u8], needle: &[u8], from: usize| {
        hay[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    let br_at = find(&out, RANGE_PLACEHOLDER.as_bytes(), base)
        .ok_or(Error::Malformed("signature placeholder"))?;
    let c_key =
        find(&out, b"/Contents <00", base).ok_or(Error::Malformed("signature placeholder"))?;
    let a = c_key + b"/Contents ".len();
    let c = a
        + out[a..]
            .iter()
            .position(|&b| b == b'>')
            .ok_or(Error::Malformed("signature placeholder"))?
        + 1;
    let capacity = (c - a - 2) / 2;
    let mut range = Vec::new();
    let _ = write!(range, "[0 {a} {c} {}", out.len() - c);
    if range.len() + 1 > RANGE_PLACEHOLDER.len() {
        return Err(Error::Unsupported("file too large to sign".into()));
    }
    range.resize(RANGE_PLACEHOLDER.len() - 1, b' ');
    range.push(b']');
    out[br_at..br_at + range.len()].copy_from_slice(&range);
    let signature = make(&[&out[..a], &out[c..]])?;
    if signature.len() > capacity {
        return Err(Error::Unsupported(TOO_SMALL.into()));
    }
    let mut hex = Vec::with_capacity(signature.len() * 2);
    for b in &signature {
        let _ = write!(hex, "{b:02X}");
    }
    out[a + 1..a + 1 + hex.len()].copy_from_slice(&hex);
    Ok(out)
}

/// Text as a PDF text string: PDFDocEncoding-compatible ASCII, else UTF-16BE.
pub(crate) fn text(s: &str) -> Object {
    if s.chars().all(|c| (' '..='~').contains(&c)) {
        Object::string(s.as_bytes())
    } else {
        let mut b = vec![0xFE, 0xFF];
        b.extend(s.encode_utf16().flat_map(u16::to_be_bytes));
        Object::String(PdfString {
            bytes: b,
            hex: true,
        })
    }
}

pub(crate) fn catalog_ref(doc: &Document) -> Result<ObjRef> {
    doc.trailer()
        .get(b"Root")
        .and_then(Object::as_objref)
        .ok_or(Error::Malformed("document catalog"))
}

pub(crate) fn dict_of(doc: &Document, r: ObjRef) -> Result<Dict> {
    doc.get(r)
        .and_then(|o| o.as_dict().cloned())
        .ok_or(Error::Malformed("dictionary"))
}

/// Appends `item` to the array `key` of object `owner`, wherever the array lives.
fn add_to_array(doc: &mut Document, owner: ObjRef, key: &[u8], item: ObjRef) -> Result<()> {
    let mut d = dict_of(doc, owner)?;
    match d.get(key).cloned() {
        Some(Object::Ref(r)) => {
            let mut a = doc
                .get(r)
                .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                .unwrap_or_default();
            a.push(Object::Ref(item));
            doc.set(r, a);
        }
        Some(Object::Array(mut a)) => {
            a.push(Object::Ref(item));
            d.set(key, a);
            doc.set(owner, d);
        }
        _ => {
            d.set(key, vec![Object::Ref(item)]);
            doc.set(owner, d);
        }
    }
    Ok(())
}

/// Sets `/SigFlags 3` on the form, creating it if needed, and adds `new_field` to it.
fn set_sig_flags(doc: &mut Document, new_field: Option<ObjRef>) -> Result<()> {
    let root = catalog_ref(doc)?;
    let mut cat = dict_of(doc, root)?;
    let (mut af, af_ref) = match cat.get(b"AcroForm").cloned() {
        Some(Object::Ref(r)) => (dict_of(doc, r).unwrap_or_default(), Some(r)),
        Some(Object::Dict(d)) => (d, None),
        _ => (Dict::new(), None),
    };
    af.set(b"SigFlags", 3);
    if let Some(f) = new_field {
        match af.get(b"Fields").cloned() {
            Some(Object::Ref(r)) => {
                let mut a = doc
                    .get(r)
                    .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                    .unwrap_or_default();
                a.push(Object::Ref(f));
                doc.set(r, a);
            }
            Some(Object::Array(mut a)) => {
                a.push(Object::Ref(f));
                af.set(b"Fields", a);
            }
            _ => af.set(b"Fields", vec![Object::Ref(f)]),
        }
    }
    match af_ref {
        Some(r) => doc.set(r, af),
        None => {
            let r = doc.add(af);
            cat.set(b"AcroForm", r);
            doc.set(root, cat);
        }
    }
    Ok(())
}

/// The visible appearance: the ink image on the left, the text on the right, upright on a
/// page rotated by `rot`.
fn appearance_stream(
    doc: &mut Document,
    a: &Appearance,
    rect: [f64; 4],
    rot: u16,
    name: &str,
    time: Time,
    opts: &SignOptions<'_>,
) -> Result<ObjRef> {
    let (rw, rh) = ((rect[2] - rect[0]).abs(), (rect[3] - rect[1]).abs());
    // The box as seen on the rotated page, and the matrix turning it back to the page.
    let (w, h, matrix) = match rot {
        90 => (rh, rw, Some([0.0, 1.0, -1.0, 0.0, rw, 0.0])),
        180 => (rw, rh, Some([-1.0, 0.0, 0.0, -1.0, rw, rh])),
        270 => (rh, rw, Some([0.0, -1.0, 1.0, 0.0, 0.0, rh])),
        _ => (rw, rh, None),
    };
    let mut content = String::new();
    let mut resources = Dict::new();
    let mut text_x = 2.0;
    if let Some(img) = a.image {
        let d = dict_of(doc, img)?;
        let iw = d.int(b"Width").unwrap_or(1).max(1) as f64;
        let ih = d.int(b"Height").unwrap_or(1).max(1) as f64;
        let area = if a.text { w * 0.45 } else { w };
        let scale = (area / iw).min(h / ih);
        let (dw, dh) = (iw * scale, ih * scale);
        let (x, y) = ((area - dw) / 2.0, (h - dh) / 2.0);
        content.push_str(&format!(
            "q {} 0 0 {} {} {} cm /Img Do Q\n",
            num(dw),
            num(dh),
            num(x),
            num(y)
        ));
        let mut xo = Dict::new();
        xo.set(b"Img", img);
        resources.set(b"XObject", xo);
        text_x = area + 4.0;
    }
    if a.text {
        let mut lines = vec![format!("Digitally signed by {name}")];
        lines.push(format!("Date: {}", time.to_iso().replace('T', " ")));
        if let Some(r) = opts.reason {
            lines.push(format!("Reason: {r}"));
        }
        if let Some(l) = opts.location {
            lines.push(format!("Location: {l}"));
        }
        let font = opts
            .font
            .unwrap_or_else(|| pagebara_annot::default_font(&lines.join("\n")));
        let mut shaper = Shaper::default();
        let laid: Vec<Line> = lines.iter().map(|l| shaper.line(font, l)).collect();
        let avail = (w - text_x - 2.0).max(1.0);
        let longest = laid.iter().map(Line::width).fold(0.1, f64::max);
        let size = (h / (lines.len() as f64 * 1.2))
            .min(avail / longest)
            .clamp(2.0, 12.0);
        let mut y = h - size * 1.05;
        let mut used: Vec<(char, Glyph)> = Vec::new();
        content.push_str("BT\n");
        for l in &laid {
            content.push_str(&format!("1 0 0 1 {} {} Tm", num(text_x), num(y)));
            // One-byte codes (Helvetica) as readable literal strings, others as hex.
            let simple = l.0.iter().all(|(_, g)| g.len == 1);
            for (face, bytes) in l.runs() {
                content.push_str(&format!(" /{} {} Tf ", font.face_name(face), num(size)));
                content.push_str(&string_operand(&bytes, simple));
                content.push_str(" Tj");
            }
            content.push('\n');
            for g in &l.0 {
                if !used.contains(g) {
                    used.push(*g);
                }
            }
            y -= size * 1.2;
        }
        content.push_str("ET\n");
        let mut fonts = Dict::new();
        for (n, f) in font.resources(doc, &used) {
            fonts.set(n.as_bytes(), f);
        }
        resources.set(b"Font", fonts);
    }
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"XObject"));
    d.set(b"Subtype", Object::name(b"Form"));
    d.set(
        b"BBox",
        vec![
            Object::Int(0),
            Object::Int(0),
            Object::Real(w),
            Object::Real(h),
        ],
    );
    if let Some(m) = matrix {
        d.set(
            b"Matrix",
            m.iter().map(|&x| Object::Real(x)).collect::<Vec<_>>(),
        );
    }
    d.set(b"Resources", resources);
    Ok(doc.add(Stream::new(d, content.into_bytes())))
}

fn string_operand(bytes: &[u8], literal: bool) -> String {
    let mut out = String::new();
    if literal {
        out.push('(');
        for &b in bytes {
            match b {
                b'(' | b')' | b'\\' => {
                    out.push('\\');
                    out.push(b as char);
                }
                0x20..0x7F => out.push(b as char),
                b => out.push_str(&format!("\\{b:03o}")),
            }
        }
        out.push(')');
    } else {
        out.push('<');
        for b in bytes {
            out.push_str(&format!("{b:02X}"));
        }
        out.push('>');
    }
    out
}

fn num(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}
