//! What changed after a signature: the objects of the signed revision against the
//! document's final state, sorted into the categories DocMDP permissions speak of
//! (ISO 32000-2 §12.8.2.2): form filling, signing, annotations, LTV data, and the rest.

use std::collections::{HashMap, HashSet, VecDeque};

use pagebara_syntax::{Dict, Document, ObjRef, Object};

/// What kind of change an incremental update made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChangeKind {
    /// Field values and their appearances.
    FormFill,
    /// Signing a signature field or adding one.
    Signing,
    /// Adding, editing or deleting comments (non-widget annotations).
    Annotation,
    /// Document Security Store and document timestamps (PAdES LTV).
    Ltv,
    /// Document information and metadata.
    Metadata,
    /// Page content, page tree, document structure: never allowed after signing.
    Other,
}

/// One change, described for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// Allowed by the document's certification and the signature's field locks.
    pub allowed: bool,
    pub description: String,
}

/// Which fields a signature locks (FieldMDP or the field's `/Lock`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Locks {
    #[default]
    None,
    All,
    Include(Vec<String>),
    Exclude(Vec<String>),
}

impl Locks {
    fn locks(&self, field: &str) -> bool {
        match self {
            Locks::None => false,
            Locks::All => true,
            Locks::Include(f) => f.iter().any(|n| n == field),
            Locks::Exclude(f) => !f.iter().any(|n| n == field),
        }
    }

    /// From a `/Lock` or FieldMDP transform parameters dictionary.
    pub fn from_dict(doc: &Document, d: &Dict) -> Locks {
        let fields = || -> Vec<String> {
            doc.get_in(d, b"Fields")
                .as_array()
                .unwrap_or_default()
                .iter()
                .filter_map(|o| doc.resolve(o).as_string().map(decode_text))
                .collect()
        };
        match d.name(b"Action") {
            Some(b"All") => Locks::All,
            Some(b"Include") => Locks::Include(fields()),
            Some(b"Exclude") => Locks::Exclude(fields()),
            _ => Locks::None,
        }
    }

    pub fn merge(self, other: Locks) -> Locks {
        match (self, other) {
            (Locks::None, o) | (o, Locks::None) => o,
            (Locks::All, _) | (_, Locks::All) => Locks::All,
            (Locks::Include(mut a), Locks::Include(b)) => {
                a.extend(b);
                Locks::Include(a)
            }
            // Mixed lists: lock everything rather than guess.
            _ => Locks::All,
        }
    }
}

pub(crate) fn decode_text(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let u: Vec<u16> = rest
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        return String::from_utf16_lossy(&u);
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| c as char).collect()
}

/// A field node: its full name and whether it is a signature field.
#[derive(Clone, Debug)]
pub(crate) struct FieldNode {
    pub name: String,
    pub is_signature: bool,
    /// A signature field holding a document timestamp.
    pub is_doc_timestamp: bool,
}

/// Every field node of the form (terminal or not, with merged widgets), by object number.
pub(crate) fn field_nodes(doc: &Document) -> (Option<ObjRef>, HashMap<u32, FieldNode>) {
    let mut out = HashMap::new();
    let cat = doc.catalog();
    let Some(cat) = cat.as_deref().and_then(Object::as_dict) else {
        return (None, out);
    };
    let af_ref = cat.get(b"AcroForm").and_then(Object::as_objref);
    let af = doc.get_in(cat, b"AcroForm");
    let Some(af) = af.as_dict() else {
        return (af_ref, out);
    };
    let fields = doc.get_in(af, b"Fields");
    let mut stack: Vec<(Object, String, bool)> = fields
        .as_array()
        .unwrap_or_default()
        .iter()
        .map(|o| (o.clone(), String::new(), false))
        .collect();
    let mut seen = HashSet::new();
    while let Some((o, parent, parent_sig)) = stack.pop() {
        let Some(r) = o.as_objref() else { continue };
        if !seen.insert(r.num) || seen.len() > 100_000 {
            continue;
        }
        let Some(node) = doc.get(r) else { continue };
        let Some(d) = node.as_dict() else { continue };
        let partial = doc.get_in(d, b"T").as_string().map(decode_text);
        let name = match (&partial, parent.is_empty()) {
            (Some(p), true) => p.clone(),
            (Some(p), false) => format!("{parent}.{p}"),
            (None, _) => parent.clone(),
        };
        let is_sig = d.name(b"FT").map_or(parent_sig, |t| t == b"Sig");
        let is_doc_timestamp = is_sig
            && doc.get_in(d, b"V").as_dict().and_then(|v| v.name(b"Type")) == Some(b"DocTimeStamp");
        out.insert(
            r.num,
            FieldNode {
                name: name.clone(),
                is_signature: is_sig,
                is_doc_timestamp,
            },
        );
        for k in doc.get_in(d, b"Kids").as_array().unwrap_or_default() {
            stack.push((k.clone(), name.clone(), is_sig));
        }
    }
    (af_ref, out)
}

/// What each object of a revision is for.
#[derive(Default)]
struct Roles {
    catalog: Option<u32>,
    acroform: Option<u32>,
    /// `/Fields` and `/Annots` arrays kept as their own objects.
    fields_array: Option<u32>,
    annots_arrays: HashSet<u32>,
    page_nodes: HashSet<u32>,
    /// Reached from page content and resources.
    content: HashSet<u32>,
    fields: HashMap<u32, FieldNode>,
    /// Non-widget annotations.
    annots: HashSet<u32>,
    /// Reached from appearances: true for widgets.
    appearance: HashMap<u32, bool>,
    signatures: HashSet<u32>,
    dss: HashSet<u32>,
    info: HashSet<u32>,
    reachable: HashSet<u32>,
}

/// Objects reachable from `roots`, not following `skip` keys nor (unless `pages`) entering
/// page objects.
fn closure(doc: &Document, roots: &[Object], skip: &[&[u8]]) -> HashSet<u32> {
    closure_with(doc, roots, skip, false)
}

fn closure_with(doc: &Document, roots: &[Object], skip: &[&[u8]], pages: bool) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<Object> = roots.iter().cloned().collect();
    while let Some(o) = queue.pop_front() {
        match o {
            Object::Ref(r) => {
                if seen.len() > 1_000_000 || seen.contains(&r.num) {
                    continue;
                }
                let Some(obj) = doc.get(r) else { continue };
                if !pages
                    && let Some(d) = obj.as_dict()
                    && matches!(d.name(b"Type"), Some(b"Page" | b"Pages"))
                {
                    continue;
                }
                seen.insert(r.num);
                queue.push_back((*obj).clone());
            }
            Object::Array(a) => queue.extend(a),
            Object::Dict(d) => queue.extend(
                d.0.into_iter()
                    .filter(|(k, _)| !skip.contains(&k.as_bytes()))
                    .map(|(_, v)| v),
            ),
            Object::Stream(s) => queue.extend(
                s.dict
                    .0
                    .into_iter()
                    .filter(|(k, _)| !skip.contains(&k.as_bytes()))
                    .map(|(_, v)| v),
            ),
            _ => {}
        }
    }
    seen
}

impl Roles {
    fn of(doc: &Document) -> Roles {
        let mut r = Roles::default();
        let trailer = doc.trailer();
        r.catalog = trailer
            .get(b"Root")
            .and_then(Object::as_objref)
            .map(|x| x.num);
        if let Some(i) = trailer.get(b"Info") {
            r.info = closure(doc, std::slice::from_ref(i), &[]);
        }
        r.reachable = closure_with(
            doc,
            &[Object::Dict(trailer.clone())],
            &[b"Prev", b"Encrypt"],
            true,
        );
        let (af, fields) = field_nodes(doc);
        r.acroform = af.map(|x| x.num);
        r.fields = fields;
        let cat = doc.catalog();
        let cat = cat.as_deref().and_then(Object::as_dict);
        if let Some(cat) = cat {
            if let Some(m) = cat.get(b"Metadata") {
                r.info.extend(closure(doc, std::slice::from_ref(m), &[]));
            }
            if let Some(dss) = cat.get(b"DSS") {
                r.dss = closure(doc, std::slice::from_ref(dss), &[]);
                if let Some(x) = dss.as_objref() {
                    r.dss.insert(x.num);
                }
            }
            if let Some(af) = doc.get_in(cat, b"AcroForm").as_dict() {
                r.fields_array = af.get(b"Fields").and_then(Object::as_objref).map(|x| x.num);
            }
        }
        // Pages: walk the tree, collecting nodes, content and annotations.
        let tree = pagebara_pages::PageTree::load(doc);
        let mut content_roots = Vec::new();
        let mut annots = Vec::new();
        for p in tree.refs() {
            r.page_nodes.insert(p.num);
            let mut node = doc.get(p);
            let mut depth = 0;
            let mut first = true;
            while let Some(n) = node {
                let Some(d) = n.as_dict() else { break };
                for k in [&b"Contents"[..], b"Resources", b"Group"] {
                    if let Some(v) = d.get(k) {
                        content_roots.push(v.clone());
                    }
                }
                if first {
                    if let Some(a) = d.get(b"Annots") {
                        if let Some(x) = a.as_objref() {
                            r.annots_arrays.insert(x.num);
                        }
                        annots.extend(
                            doc.resolve(a)
                                .as_array()
                                .unwrap_or_default()
                                .iter()
                                .cloned(),
                        );
                    }
                    first = false;
                }
                let parent = d.get(b"Parent").and_then(Object::as_objref);
                depth += 1;
                if depth > 64 {
                    break;
                }
                match parent {
                    Some(pr) => {
                        r.page_nodes.insert(pr.num);
                        node = doc.get(pr);
                    }
                    None => break,
                }
            }
        }
        r.content = closure(doc, &content_roots, &[]);
        for a in annots {
            let Some(ar) = a.as_objref() else { continue };
            let Some(ad) = doc.get(ar) else { continue };
            let Some(d) = ad.as_dict() else { continue };
            let widget = d.name(b"Subtype") == Some(b"Widget");
            if !widget {
                r.annots.insert(ar.num);
            }
            let mut roots: Vec<Object> = Vec::new();
            roots.extend(d.get(b"AP").cloned());
            roots.extend(d.get(b"MK").cloned());
            if !widget {
                roots.extend(d.get(b"Popup").cloned());
                roots.extend(d.get(b"RC").cloned());
            }
            for n in closure(doc, &roots, &[b"Parent", b"P"]) {
                r.appearance.entry(n).or_insert(widget);
            }
        }
        // Signature dictionaries: field values of signature fields.
        for (&num, f) in &r.fields {
            if !f.is_signature {
                continue;
            }
            if let Some(node) = doc.get(ObjRef::new(num, 0))
                && let Some(v) = node.as_dict().and_then(|d| d.get(b"V"))
            {
                if let Some(x) = v.as_objref() {
                    r.signatures.insert(x.num);
                }
                if let Some(vd) = doc.resolve(v).as_dict() {
                    for x in closure(doc, &[Object::Dict(vd.clone())], &[b"Data"]) {
                        r.signatures.insert(x);
                    }
                }
            }
        }
        r
    }
}

/// Objects equal across two documents (streams by their encoded bytes).
fn same(a: &Document, x: &Object, b: &Document, y: &Object) -> bool {
    match (x, y) {
        (Object::Stream(s), Object::Stream(t)) => {
            same_dict(a, &s.dict, b, &t.dict, &[b"Length"]) && a.raw(s) == b.raw(t)
        }
        (Object::Dict(d), Object::Dict(e)) => same_dict(a, d, b, e, &[]),
        (Object::Array(p), Object::Array(q)) => {
            p.len() == q.len() && p.iter().zip(q).all(|(i, j)| same(a, i, b, j))
        }
        (Object::Real(p), Object::Int(q)) | (Object::Int(q), Object::Real(p)) => *p == *q as f64,
        _ => x == y,
    }
}

fn same_dict(a: &Document, d: &Dict, b: &Document, e: &Dict, ignore: &[&[u8]]) -> bool {
    changed_keys(a, d, b, e)
        .iter()
        .all(|k| ignore.contains(&k.as_slice()))
}

/// Keys whose values differ (or exist on one side only).
fn changed_keys(a: &Document, d: &Dict, b: &Document, e: &Dict) -> Vec<Vec<u8>> {
    let mut keys: Vec<Vec<u8>> = Vec::new();
    for (k, v) in d.iter() {
        match e.get(k.as_bytes()) {
            Some(w) if same(a, v, b, w) => {}
            _ => keys.push(k.as_bytes().to_vec()),
        }
    }
    for (k, _) in e.iter() {
        if d.get(k.as_bytes()).is_none() {
            keys.push(k.as_bytes().to_vec());
        }
    }
    keys
}

fn key_list(keys: &[Vec<u8>]) -> String {
    keys.iter()
        .map(|k| format!("/{}", String::from_utf8_lossy(k)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compares the signed revision `old` with `new`. `p` is the DocMDP permission level in
/// force (1 none, 2 form filling and signing, 3 also annotations); approval signatures
/// without certification pass 3.
pub(crate) fn diff(old: &Document, new: &Document, p: u8, locks: &Locks) -> Vec<Change> {
    let ro = Roles::of(old);
    let rn = Roles::of(new);
    let mut out: Vec<Change> = Vec::new();
    let mut push = |kind: ChangeKind, description: String| {
        let allowed = match kind {
            ChangeKind::Ltv => true,
            ChangeKind::Metadata => p >= 2,
            ChangeKind::FormFill | ChangeKind::Signing => p >= 2,
            ChangeKind::Annotation => p >= 3,
            ChangeKind::Other => false,
        };
        if !out
            .iter()
            .any(|c| c.kind == kind && c.description == description)
        {
            out.push(Change {
                kind,
                allowed,
                description,
            });
        }
    };

    let old_nums: HashSet<u32> = old.object_numbers().into_iter().collect();
    let new_nums: HashSet<u32> = new.object_numbers().into_iter().collect();
    let mut nums: Vec<u32> = old_nums.union(&new_nums).copied().collect();
    nums.sort_unstable();
    for num in nums {
        let r = ObjRef::new(num, 0);
        let (a, b) = (old.get(r), new.get(r));
        match (&a, &b) {
            (Some(x), Some(y)) if same(old, x, new, y) => continue,
            (None, None) => continue,
            _ => {}
        }
        let in_old = ro.reachable.contains(&num);
        let in_new = rn.reachable.contains(&num);
        if !in_old && !in_new {
            continue;
        }
        let Some(b) = b else {
            // Deleted (or made unreachable).
            if ro.annots.contains(&num) {
                push(ChangeKind::Annotation, "a comment was deleted".into());
            } else if ro.content.contains(&num) || ro.page_nodes.contains(&num) {
                push(ChangeKind::Other, "page content was removed".into());
            } else if ro.fields.contains_key(&num) || ro.signatures.contains(&num) {
                push(ChangeKind::Other, "a form field was removed".into());
            }
            continue;
        };
        let Some(a) = a else {
            // New objects matter through the objects that reference them, except these.
            if rn.signatures.contains(&num) && b.as_dict().is_some_and(|d| d.contains(b"ByteRange"))
            {
                if b.as_dict().and_then(|d| d.name(b"Type")) == Some(b"DocTimeStamp") {
                    push(ChangeKind::Ltv, "a document timestamp was added".into());
                } else {
                    push(ChangeKind::Signing, "a signature was added".into());
                }
            }
            continue;
        };
        let (da, db) = (a.as_dict(), b.as_dict());
        if ro.content.contains(&num) || rn.content.contains(&num) {
            push(ChangeKind::Other, "page content was changed".into());
        } else if Some(num) == ro.catalog || Some(num) == rn.catalog {
            let (Some(da), Some(db)) = (da, db) else {
                push(
                    ChangeKind::Other,
                    "the document catalog was replaced".into(),
                );
                continue;
            };
            for k in changed_keys(old, da, new, db) {
                match k.as_slice() {
                    b"DSS" | b"Extensions" => push(ChangeKind::Ltv, "LTV data was added".into()),
                    b"AcroForm" => {
                        let (x, y) = (old.get_in(da, b"AcroForm"), new.get_in(db, b"AcroForm"));
                        match (x.as_dict(), y.as_dict()) {
                            (Some(x), Some(y)) => {
                                acroform_changes(old, x, &ro, new, y, &rn, &mut push)
                            }
                            (None, Some(_)) => {
                                push(ChangeKind::Signing, "a form was added for signing".into())
                            }
                            _ => push(ChangeKind::Other, "the form was removed".into()),
                        }
                    }
                    b"Metadata" => {
                        push(ChangeKind::Metadata, "the document metadata changed".into())
                    }
                    b"Perms" => push(ChangeKind::Other, "document permissions changed".into()),
                    k => push(
                        ChangeKind::Other,
                        format!("the document catalog changed ({})", key_list(&[k.to_vec()])),
                    ),
                }
            }
        } else if ro.page_nodes.contains(&num) || rn.page_nodes.contains(&num) {
            let (Some(da), Some(db)) = (da, db) else {
                push(ChangeKind::Other, "a page was replaced".into());
                continue;
            };
            let keys = changed_keys(old, da, new, db);
            for k in &keys {
                if k == b"Annots" && rn.page_nodes.contains(&num) {
                    let x = old.get_in(da, b"Annots");
                    let y = new.get_in(db, b"Annots");
                    annots_changes(
                        x.as_array().unwrap_or_default(),
                        &ro,
                        y.as_array().unwrap_or_default(),
                        &rn,
                        new,
                        &mut push,
                    );
                } else {
                    push(ChangeKind::Other, "pages were changed".into());
                }
            }
        } else if ro.annots_arrays.contains(&num) || rn.annots_arrays.contains(&num) {
            annots_changes(
                a.as_array().unwrap_or_default(),
                &ro,
                b.as_array().unwrap_or_default(),
                &rn,
                new,
                &mut push,
            );
        } else if Some(num) == ro.acroform || Some(num) == rn.acroform {
            match (da, db) {
                (Some(x), Some(y)) => acroform_changes(old, x, &ro, new, y, &rn, &mut push),
                _ => push(ChangeKind::Other, "the form was replaced".into()),
            }
        } else if Some(num) == ro.fields_array || Some(num) == rn.fields_array {
            fields_list_changes(
                a.as_array().unwrap_or_default(),
                &ro,
                b.as_array().unwrap_or_default(),
                &rn,
                &mut push,
            );
        } else if let Some(f) = ro.fields.get(&num).or_else(|| rn.fields.get(&num)) {
            let (Some(da), Some(db)) = (da, db) else {
                push(
                    ChangeKind::Other,
                    format!("field \"{}\" was replaced", f.name),
                );
                continue;
            };
            let keys = changed_keys(old, da, new, db);
            let fill_keys: [&[u8]; 6] = [b"V", b"AS", b"AP", b"M", b"MK", b"Ff"];
            let other: Vec<Vec<u8>> = keys
                .iter()
                .filter(|k| !fill_keys.contains(&k.as_slice()))
                .cloned()
                .collect();
            if !other.is_empty() {
                push(
                    ChangeKind::Other,
                    format!(
                        "field \"{}\" properties changed ({})",
                        f.name,
                        key_list(&other)
                    ),
                );
            }
            let ff_ok = |o: Option<i64>, n: Option<i64>| {
                let (o, n) = (o.unwrap_or(0), n.unwrap_or(0));
                // Only the read-only bit may be added (locking on signing).
                (o | 1) == (n | 1) && n & 1 >= o & 1
            };
            if keys.iter().any(|k| k == b"Ff")
                && !ff_ok(
                    old.get_in(da, b"Ff").as_int(),
                    new.get_in(db, b"Ff").as_int(),
                )
            {
                push(
                    ChangeKind::Other,
                    format!("field \"{}\" flags changed", f.name),
                );
            }
            if keys.iter().any(|k| k == b"V") {
                if f.is_signature {
                    if da.get(b"V").is_some() {
                        push(
                            ChangeKind::Other,
                            format!("the signature in \"{}\" was replaced", f.name),
                        );
                    } else {
                        push(
                            ChangeKind::Signing,
                            format!("field \"{}\" was signed", f.name),
                        );
                    }
                } else if locks.locks(&f.name) {
                    push(
                        ChangeKind::Other,
                        format!("locked field \"{}\" was filled in", f.name),
                    );
                } else {
                    push(
                        ChangeKind::FormFill,
                        format!("field \"{}\" was filled in", f.name),
                    );
                }
            } else if keys.iter().any(|k| fill_keys.contains(&k.as_slice())) {
                let kind = if f.is_signature {
                    ChangeKind::Signing
                } else {
                    ChangeKind::FormFill
                };
                push(kind, format!("the appearance of \"{}\" changed", f.name));
            }
        } else if ro.annots.contains(&num) || rn.annots.contains(&num) {
            push(ChangeKind::Annotation, "a comment was changed".into());
        } else if let Some(&widget) = ro.appearance.get(&num).or_else(|| rn.appearance.get(&num)) {
            if widget {
                push(
                    ChangeKind::FormFill,
                    "a field appearance was updated".into(),
                );
            } else {
                push(
                    ChangeKind::Annotation,
                    "a comment's appearance was changed".into(),
                );
            }
        } else if ro.signatures.contains(&num) {
            push(ChangeKind::Other, "an earlier signature was altered".into());
        } else if ro.dss.contains(&num) || rn.dss.contains(&num) {
            push(ChangeKind::Ltv, "LTV data was updated".into());
        } else if ro.info.contains(&num) || rn.info.contains(&num) {
            push(
                ChangeKind::Metadata,
                "the document information changed".into(),
            );
        } else {
            push(ChangeKind::Other, format!("object {num} was changed"));
        }
    }
    if p == 1 {
        for c in &mut out {
            c.allowed = c.kind == ChangeKind::Ltv;
        }
    }
    out.sort_by_key(|c| (c.allowed, c.kind));
    out
}

type Push<'a> = dyn FnMut(ChangeKind, String) + 'a;

fn acroform_changes(
    old: &Document,
    x: &Dict,
    ro: &Roles,
    new: &Document,
    y: &Dict,
    rn: &Roles,
    push: &mut Push<'_>,
) {
    for k in changed_keys(old, x, new, y) {
        match k.as_slice() {
            b"Fields" => {
                let (a, b) = (old.get_in(x, b"Fields"), new.get_in(y, b"Fields"));
                fields_list_changes(
                    a.as_array().unwrap_or_default(),
                    ro,
                    b.as_array().unwrap_or_default(),
                    rn,
                    push,
                );
            }
            b"SigFlags" => push(ChangeKind::Signing, "the form was marked as signed".into()),
            b"DR" | b"DA" | b"NeedAppearances" => {
                push(ChangeKind::FormFill, "form resources were updated".into())
            }
            k => push(
                ChangeKind::Other,
                format!("form settings changed ({})", key_list(&[k.to_vec()])),
            ),
        }
    }
}

fn refs(a: &[Object]) -> HashSet<u32> {
    a.iter()
        .filter_map(Object::as_objref)
        .map(|r| r.num)
        .collect()
}

fn fields_list_changes(a: &[Object], ro: &Roles, b: &[Object], rn: &Roles, push: &mut Push<'_>) {
    let (x, y) = (refs(a), refs(b));
    for n in y.difference(&x) {
        match rn.fields.get(n) {
            Some(f) if f.is_doc_timestamp => {
                push(ChangeKind::Ltv, "a document timestamp was added".into())
            }
            Some(f) if f.is_signature => push(
                ChangeKind::Signing,
                format!("signature field \"{}\" was added", f.name),
            ),
            Some(f) => push(ChangeKind::Other, format!("field \"{}\" was added", f.name)),
            None => push(ChangeKind::Other, "a form field was added".into()),
        }
    }
    for n in x.difference(&y) {
        let name = ro
            .fields
            .get(n)
            .map_or_else(String::new, |f| f.name.clone());
        push(ChangeKind::Other, format!("field \"{name}\" was removed"));
    }
}

fn annots_changes(
    a: &[Object],
    ro: &Roles,
    b: &[Object],
    rn: &Roles,
    new: &Document,
    push: &mut Push<'_>,
) {
    let (x, y) = (refs(a), refs(b));
    for n in y.difference(&x) {
        if let Some(f) = rn.fields.get(n) {
            if f.is_doc_timestamp {
                push(ChangeKind::Ltv, "a document timestamp was added".into());
            } else if f.is_signature {
                push(
                    ChangeKind::Signing,
                    format!("signature field \"{}\" was added", f.name),
                );
            } else {
                push(ChangeKind::Other, format!("field \"{}\" was added", f.name));
            }
        } else {
            let widget = new
                .get(ObjRef::new(*n, 0))
                .and_then(|o| o.as_dict().map(|d| d.name(b"Subtype") == Some(b"Widget")))
                .unwrap_or(false);
            if widget {
                push(ChangeKind::Other, "a form field was added".into());
            } else {
                push(ChangeKind::Annotation, "a comment was added".into());
            }
        }
    }
    for n in x.difference(&y) {
        if ro.annots.contains(n) {
            push(ChangeKind::Annotation, "a comment was deleted".into());
        } else {
            push(
                ChangeKind::Other,
                "a form field was removed from a page".into(),
            );
        }
    }
}
