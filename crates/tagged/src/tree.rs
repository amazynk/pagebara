//! Reading the structure tree (§14.7.2) into an arena of elements, with role mapping.

use std::collections::HashSet;

use pagebara_content::extract::text_string;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

/// Deeper nesting (of elements or `/K` arrays) is not read.
const MAX_DEPTH: u32 = 256;
/// Elements beyond this many are not read.
const MAX_ELEMENTS: usize = 1 << 20;
/// Role map chains longer than this end at [`Role::Group`].
const MAX_ROLE_CHAIN: usize = 16;

/// The standard structure type an element maps to (§14.8.4), as far as reading order cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Groups other elements: `Document`, `Div`, `NonStruct`, `Private`, `TOC`, `Index`,
    /// `Aside` and types neither standard nor role mapped to one.
    Group,
    /// `Part`, `Sect` and `Art`: groups that nest headings below them a level deeper.
    Section,
    /// `H1`–`H6`, or `H` (`None`) whose level follows its nesting in sections.
    Heading(Option<u8>),
    Paragraph,
    List,
    ListItem,
    /// `Lbl` and `LBody`.
    ListPart,
    /// `Table`, `TR`, `THead`, `TBody` and `TFoot`.
    Table,
    /// `TH` and `TD`.
    Cell,
    Figure,
    Formula,
    Caption,
    Link,
    Note,
    Quote,
    BlockQuote,
    Code,
    /// `Span` and other inline types (`Em`, `Strong`, `Sub`, `Reference`, `Ruby`, …).
    Inline,
    Artifact,
}

impl Role {
    fn standard(s: &[u8]) -> Option<Role> {
        Some(match s {
            b"Document" | b"DocumentFragment" | b"Div" | b"NonStruct" | b"Private" | b"TOC"
            | b"Index" | b"Aside" | b"Form" | b"Annot" => Role::Group,
            b"Part" | b"Sect" | b"Art" => Role::Section,
            b"H" => Role::Heading(None),
            b"Title" | b"H1" => Role::Heading(Some(1)),
            b"H2" => Role::Heading(Some(2)),
            b"H3" => Role::Heading(Some(3)),
            b"H4" => Role::Heading(Some(4)),
            b"H5" => Role::Heading(Some(5)),
            b"H6" => Role::Heading(Some(6)),
            b"P" | b"TOCI" | b"BibEntry" => Role::Paragraph,
            b"L" => Role::List,
            b"LI" => Role::ListItem,
            b"Lbl" | b"LBody" => Role::ListPart,
            b"Table" | b"TR" | b"THead" | b"TBody" | b"TFoot" => Role::Table,
            b"TH" | b"TD" => Role::Cell,
            b"Figure" => Role::Figure,
            b"Formula" => Role::Formula,
            b"Caption" => Role::Caption,
            b"Link" => Role::Link,
            b"Note" | b"FENote" => Role::Note,
            b"Quote" => Role::Quote,
            b"BlockQuote" => Role::BlockQuote,
            b"Code" => Role::Code,
            b"Span" | b"Em" | b"Strong" | b"Sub" | b"Reference" | b"Ruby" | b"RB" | b"RT"
            | b"RP" | b"Warichu" | b"WT" | b"WP" => Role::Inline,
            b"Artifact" => Role::Artifact,
            _ => return None,
        })
    }
}

/// A structure element.
#[derive(Clone, Debug)]
pub struct Element {
    /// `/S` as written.
    pub tag: Vec<u8>,
    pub role: Role,
    /// `/Pg`, or the nearest ancestor's.
    pub page: Option<ObjRef>,
    /// `/Lang` of the element itself.
    pub lang: Option<String>,
    pub alt: Option<String>,
    pub actual_text: Option<String>,
    /// `/E`: the expansion of an abbreviation.
    pub expansion: Option<String>,
    pub kids: Vec<Kid>,
    /// Pages its content is on, sorted.
    pub(crate) pages: Vec<ObjRef>,
}

/// A child of a structure element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kid {
    /// Index in [`StructTree::elements`].
    Element(usize),
    /// A marked-content sequence: `mcid` in the content of `page`, or of the form XObject
    /// `stream` drawn on it.
    Content {
        page: Option<ObjRef>,
        mcid: i64,
        stream: Option<ObjRef>,
    },
    /// An object such as an annotation (`/OBJR`).
    Object {
        page: Option<ObjRef>,
        object: ObjRef,
    },
}

impl Kid {
    fn page(&self, elems: &[Element]) -> Vec<ObjRef> {
        match self {
            Kid::Element(e) => elems[*e].pages.clone(),
            Kid::Content { page, .. } | Kid::Object { page, .. } => page.iter().copied().collect(),
        }
    }
}

/// The logical structure of a document (the catalog's `/StructTreeRoot`). Elements are
/// read once each: references back up the tree or to an element already read (cycles and
/// shared elements in damaged files) are skipped, as is nesting beyond 256 levels.
/// `/RoleMap` maps custom types to standard ones; PDF 2.0 namespaces are not followed.
#[derive(Clone, Debug)]
pub struct StructTree {
    elements: Vec<Element>,
    roots: Vec<usize>,
    /// The catalog's `/Lang`.
    lang: Option<String>,
    pages: HashSet<ObjRef>,
}

impl StructTree {
    /// The document's structure tree, or `None` when it has none or it is empty.
    pub fn load(doc: &Document) -> Option<StructTree> {
        let cat = doc.catalog()?;
        let cat = cat.as_dict()?;
        let root = doc.get_in(cat, b"StructTreeRoot");
        let root = root.as_dict()?;
        let mut l = Loader {
            doc,
            role_map: doc
                .get_in(root, b"RoleMap")
                .as_dict()
                .cloned()
                .unwrap_or_default(),
            elements: Vec::new(),
            seen: HashSet::new(),
        };
        let mut kids = Vec::new();
        if let Some(k) = root.get(b"K") {
            l.kids(k, None, 0, &mut kids);
        }
        let roots: Vec<usize> = kids
            .into_iter()
            .filter_map(|k| match k {
                Kid::Element(e) => Some(e),
                _ => None,
            })
            .collect();
        if roots.is_empty() {
            return None;
        }
        let pages = roots
            .iter()
            .flat_map(|&r| l.elements[r].pages.iter().copied())
            .collect();
        Some(StructTree {
            elements: l.elements,
            roots,
            lang: string(doc, cat, b"Lang"),
            pages,
        })
    }

    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    /// The top-level elements, in order.
    pub fn roots(&self) -> &[usize] {
        &self.roots
    }

    /// The document's natural language (the catalog's `/Lang`).
    pub fn lang(&self) -> Option<&str> {
        self.lang.as_deref()
    }

    /// Whether any element has content on `page`.
    pub fn has_page(&self, page: ObjRef) -> bool {
        self.pages.contains(&page)
    }

    pub(crate) fn touches(&self, e: usize, page: ObjRef) -> bool {
        self.elements[e].pages.binary_search(&page).is_ok()
    }
}

struct Loader<'a> {
    doc: &'a Document,
    role_map: Dict,
    elements: Vec<Element>,
    seen: HashSet<ObjRef>,
}

impl Loader<'_> {
    /// Reads a `/K` entry (or one item of its array) into `out`.
    fn kids(&mut self, k: &Object, page: Option<ObjRef>, depth: u32, out: &mut Vec<Kid>) {
        if depth > MAX_DEPTH {
            return;
        }
        let r = k.as_objref();
        let o = self.doc.resolve(k);
        match &*o {
            Object::Int(mcid) => out.push(Kid::Content {
                page,
                mcid: *mcid,
                stream: None,
            }),
            Object::Array(a) => {
                for k in a {
                    self.kids(k, page, depth + 1, out);
                }
            }
            Object::Dict(d) => {
                let pg = d.get(b"Pg").and_then(Object::as_objref).or(page);
                let ty = d.name(b"Type");
                let mcid = self.doc.get_in(d, b"MCID").as_int();
                let obj = d.get(b"Obj").and_then(Object::as_objref);
                if ty == Some(b"MCR") || !d.contains(b"S") && mcid.is_some() {
                    if let Some(mcid) = mcid {
                        out.push(Kid::Content {
                            page: pg,
                            mcid,
                            stream: d.get(b"Stm").and_then(Object::as_objref),
                        });
                    }
                } else if ty == Some(b"OBJR") || !d.contains(b"S") && obj.is_some() {
                    if let Some(object) = obj {
                        out.push(Kid::Object { page: pg, object });
                    }
                } else if let Some(e) = self.element(r, d, page, depth + 1) {
                    out.push(Kid::Element(e));
                }
            }
            _ => {}
        }
    }

    fn element(
        &mut self,
        r: Option<ObjRef>,
        d: &Dict,
        page: Option<ObjRef>,
        depth: u32,
    ) -> Option<usize> {
        if depth > MAX_DEPTH || self.elements.len() >= MAX_ELEMENTS {
            return None;
        }
        if let Some(r) = r
            && !self.seen.insert(r)
        {
            return None;
        }
        let doc = self.doc;
        let tag = doc.get_in(d, b"S").as_name()?.to_vec();
        let page = d.get(b"Pg").and_then(Object::as_objref).or(page);
        let i = self.elements.len();
        self.elements.push(Element {
            role: self.role(&tag),
            tag,
            page,
            lang: string(doc, d, b"Lang"),
            alt: string(doc, d, b"Alt"),
            actual_text: string(doc, d, b"ActualText"),
            expansion: string(doc, d, b"E"),
            kids: Vec::new(),
            pages: Vec::new(),
        });
        let mut kids = Vec::new();
        if let Some(k) = d.get(b"K") {
            self.kids(k, page, depth, &mut kids);
        }
        let mut pages: Vec<ObjRef> = kids.iter().flat_map(|k| k.page(&self.elements)).collect();
        if pages.is_empty() {
            pages.extend(page);
        }
        pages.sort_unstable();
        pages.dedup();
        let e = &mut self.elements[i];
        e.kids = kids;
        e.pages = pages;
        Some(i)
    }

    /// The standard role of a type, through the role map.
    fn role(&self, tag: &[u8]) -> Role {
        let mut t = tag.to_vec();
        for _ in 0..MAX_ROLE_CHAIN {
            if let Some(r) = Role::standard(&t) {
                return r;
            }
            match self.doc.get_in(&self.role_map, &t).as_name() {
                Some(n) if n != t.as_slice() => t = n.to_vec(),
                _ => break,
            }
        }
        Role::Group
    }
}

/// A text string entry, `None` when missing or empty.
fn string(doc: &Document, d: &Dict, key: &[u8]) -> Option<String> {
    let o = doc.get_in(d, key);
    o.as_string()?;
    let s = text_string(&o);
    (!s.trim().is_empty()).then_some(s)
}
