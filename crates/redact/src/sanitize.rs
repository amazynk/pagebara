//! Removing what a document carries besides its visible pages: metadata, scripts,
//! attachments, hidden layers, comments, thumbnails, form data and unused resources.

use pagebara_content::oc::OptionalContent;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::apply::{inherited, remove_annots, remove_xfa, set_catalog_key};
use crate::fonts;
use crate::geom::Matrix;
use crate::mark::annots;
use crate::prune;
use crate::rewrite::{Res, Rewriter};

/// What [`sanitize`] removes. [`Default`] is the recommended set for sharing a redacted
/// document; it keeps comments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sanitize {
    /// The document information dictionary, XMP metadata of the document and of every
    /// object, and private application data (`/PieceInfo`).
    pub metadata: bool,
    /// JavaScript and launch actions, additional actions (`/AA`) and document scripts.
    pub javascript: bool,
    /// Attached files: the `/EmbeddedFiles` name tree, file attachment annotations and
    /// associated files (`/AF`).
    pub embedded_files: bool,
    /// Content in optional content groups the default configuration hides, and
    /// annotations in them.
    pub hidden_layers: bool,
    /// Comments: every markup annotation and pop-up (links and form fields stay).
    pub comments: bool,
    /// Page thumbnails.
    pub thumbnails: bool,
    /// XFA form data, which repeats field values.
    pub xfa: bool,
    /// Images and forms that resources list but no content draws.
    pub unused_resources: bool,
}

impl Default for Sanitize {
    fn default() -> Self {
        Sanitize {
            metadata: true,
            javascript: true,
            embedded_files: true,
            hidden_layers: true,
            comments: false,
            thumbnails: true,
            xfa: true,
            unused_resources: true,
        }
    }
}

impl Sanitize {
    /// Everything, comments included.
    pub fn all() -> Self {
        Sanitize {
            comments: true,
            ..Sanitize::default()
        }
    }

    pub fn none() -> Self {
        Sanitize {
            metadata: false,
            javascript: false,
            embedded_files: false,
            hidden_layers: false,
            comments: false,
            thumbnails: false,
            xfa: false,
            unused_resources: false,
        }
    }
}

/// What [`sanitize`] found and removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SanitizeReport {
    pub metadata: usize,
    pub scripts: usize,
    pub embedded_files: usize,
    pub hidden_content_pages: usize,
    /// Glyphs of hidden content that nothing shows any more, blanked in their fonts.
    pub glyphs_blanked: usize,
    pub annotations: usize,
    pub thumbnails: usize,
    pub xfa: bool,
    pub unused_resources: usize,
    pub warnings: Vec<String>,
}

const MARKUP: &[&[u8]] = &[
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Caret",
    b"Stamp",
    b"Ink",
    b"Popup",
    b"FileAttachment",
    b"Sound",
    b"Redact",
];

/// Removes what `opts` selects. Objects that become unreachable are left out by
/// [`save`](crate::save).
pub fn sanitize(doc: &mut Document, opts: &Sanitize) -> SanitizeReport {
    let mut rep = SanitizeReport::default();
    let removed = if opts.hidden_layers {
        hidden_layers(doc, &mut rep)
    } else {
        fonts::Removed::new()
    };
    if opts.metadata {
        let mut t = doc.trailer().clone();
        if t.remove(b"Info").is_some() {
            rep.metadata += 1;
            doc.set_trailer(t);
        }
    }
    if opts.embedded_files {
        set_catalog_key(doc, b"Collection", Object::Null);
    }
    // Per object.
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let mut o = (*o).clone();
        let before = o.clone();
        if let Some(d) = dict_mut(&mut o) {
            if opts.metadata {
                for k in [&b"Metadata"[..], b"PieceInfo"] {
                    if d.remove(k).is_some() {
                        rep.metadata += 1;
                    }
                }
            }
            if opts.javascript {
                rep.scripts += strip_scripts(doc, d);
            }
            if opts.embedded_files && d.remove(b"AF").is_some() {
                rep.embedded_files += 1;
            }
            if opts.thumbnails && d.name(b"Type") == Some(b"Page") && d.remove(b"Thumb").is_some() {
                rep.thumbnails += 1;
            }
            if d.contains(b"Names") && d.name(b"Type") == Some(b"Catalog") {
                let names = d.get(b"Names").cloned();
                if let Some(nd) = names.and_then(|n| doc.resolve(&n).as_dict().cloned()) {
                    let mut nd = nd;
                    let mut changed = false;
                    if opts.javascript && nd.remove(b"JavaScript").is_some() {
                        rep.scripts += 1;
                        changed = true;
                    }
                    if opts.embedded_files && nd.remove(b"EmbeddedFiles").is_some() {
                        rep.embedded_files += 1;
                        changed = true;
                    }
                    if changed {
                        match d.get(b"Names") {
                            Some(Object::Ref(nr)) => {
                                let nr = *nr;
                                doc.set(nr, nd);
                            }
                            _ => d.set(b"Names", nd),
                        }
                    }
                }
            }
        }
        if o != before {
            doc.set(r, o);
        }
    }
    // Per page annotations.
    let tree = pagebara_pages::PageTree::load(doc);
    let oc = OptionalContent::new(doc);
    for page in tree.pages() {
        let Some(po) = doc.get(page.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let mut gone = Vec::new();
        for a in annots(doc, pd) {
            let Some(ao) = doc.get(a) else { continue };
            let Some(ad) = ao.as_dict() else { continue };
            let sub = ad.name(b"Subtype").unwrap_or_default();
            let hidden = opts.hidden_layers && ad.get(b"OC").is_some_and(|o| !oc.visible(doc, o));
            if (opts.comments && MARKUP.contains(&sub))
                || (opts.embedded_files && sub == b"FileAttachment")
                || hidden
            {
                gone.push(a);
            }
        }
        if !gone.is_empty() {
            rep.annotations += remove_annots(doc, page.r, &gone);
        }
    }
    if opts.xfa {
        rep.xfa = remove_xfa(doc);
    }
    if opts.unused_resources {
        rep.unused_resources = prune::prune_xobjects(doc, None);
    }
    // Last, when what still shows the glyphs is known.
    rep.glyphs_blanked = fonts::prune(doc, &removed, &mut rep.warnings).glyphs;
    rep
}

fn dict_mut(o: &mut Object) -> Option<&mut Dict> {
    match o {
        Object::Dict(d) => Some(d),
        Object::Stream(s) => Some(&mut s.dict),
        _ => None,
    }
}

/// Whether an action (or a chain of them) runs a script or launches something.
fn is_script(doc: &Document, a: &Object) -> bool {
    let a = doc.resolve(a);
    let Some(d) = a.as_dict() else { return false };
    matches!(
        d.name(b"S"),
        Some(b"JavaScript" | b"Launch" | b"ImportData" | b"RichMediaExecute")
    ) || d.contains(b"JS")
}

fn strip_scripts(doc: &Document, d: &mut Dict) -> usize {
    let mut n = 0;
    if d.remove(b"AA").is_some() {
        n += 1;
    }
    for k in [&b"A"[..], b"OpenAction"] {
        if d.get(k).is_some_and(|a| is_script(doc, a)) {
            d.remove(k);
            n += 1;
        }
    }
    match d.get(b"Next").cloned() {
        Some(Object::Array(a)) => {
            let kept: Vec<Object> = a.iter().filter(|x| !is_script(doc, x)).cloned().collect();
            if kept.len() != a.len() {
                n += a.len() - kept.len();
                d.set(b"Next", Object::Array(kept));
            }
        }
        Some(x) if is_script(doc, &x) => {
            d.remove(b"Next");
            n += 1;
        }
        _ => {}
    }
    // An action that is itself a script (reachable from places not covered above).
    if matches!(d.name(b"S"), Some(b"JavaScript")) || d.contains(b"JS") {
        d.remove(b"JS");
        d.set(b"S", Object::name(b"JavaScript"));
        d.set(b"JS", Object::string(b""));
    }
    n
}

/// Rewrites every page without the content hidden layers hold. Returns the glyphs removed.
fn hidden_layers(doc: &mut Document, rep: &mut SanitizeReport) -> fonts::Removed {
    let oc = OptionalContent::new(doc);
    let tree = pagebara_pages::PageTree::load(doc);
    let mut removed = fonts::Removed::new();
    let mut tainted = std::collections::HashSet::new();
    for page in tree.pages() {
        let Some(po) = doc.get(page.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let mut pd = pd.clone();
        let content = pagebara_content::page_contents(doc, &pd);
        // Cheap test before rewriting: hidden content is marked /OC or drawn by XObjects.
        if !content.windows(3).any(|w| w == b"/OC" || w == b" Do") {
            continue;
        }
        let res_obj = inherited(doc, &pd, b"Resources");
        let mut res = Res::new(doc, res_obj.as_ref());
        let sp = doc.get_in(&pd, b"StructParents").as_int();
        let mut rw = Rewriter::new(doc, Vec::new(), crate::apply::GLYPH_OVERLAP);
        rw.hidden_oc = Some(oc.clone());
        let (new, changed) = rw.rewrite(&content, &mut res, Matrix::IDENTITY, sp);
        fonts::merge(&mut removed, std::mem::take(&mut rw.removed));
        tainted.extend(rw.tainted.drain());
        rep.warnings.append(&mut rw.warnings);
        drop(rw);
        if !changed {
            continue;
        }
        let mut sd = Dict::new();
        sd.set(b"Filter", Object::name(b"FlateDecode"));
        let r: ObjRef = doc.add(Stream::new(sd, pagebara_filters::encode_flate(&new, 6)));
        pd.set(b"Contents", Object::Ref(r));
        pd.set(b"Resources", Object::Dict(res.dict));
        doc.set(page.r, pd);
        rep.hidden_content_pages += 1;
    }
    // Hidden XObjects no longer drawn leave the resources.
    tainted.remove(&0);
    if !tainted.is_empty() {
        rep.unused_resources += prune::prune_xobjects(doc, Some(&tainted));
    }
    removed
}
