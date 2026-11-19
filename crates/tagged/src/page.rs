//! A page's content as block-level items in the order of the structure tree.

use std::collections::BTreeMap;
use std::ops::Range;

use pagebara_content::{DisplayList, Point, Rect, TextPage, extract_marked, marked_bounds};
use pagebara_syntax::{Document, ObjRef};

use crate::ItemKind;
use crate::tree::{Kid, Role, StructTree};

/// A block-level item: a heading, paragraph, list item, figure, …
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: ItemKind,
    /// `/Lang` of the element or its nearest ancestor with one.
    pub lang: Option<String>,
    /// The text of its content in tree order, `\n` between lines; an element's
    /// `/ActualText` replaces the text of its content, and a figure's text is its
    /// `/ActualText` or `/Alt`.
    pub text: String,
    /// Boxes in page space (y up) of the text's characters, with their byte ranges in
    /// `text`, in text order. Replacement text has one box around what it replaces.
    pub chars: Vec<(Range<u32>, [Point; 4])>,
}

/// The tagged content of one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageContent {
    pub blocks: Vec<Block>,
    /// Objects the tree refers to on the page (link annotations, form fields), in tree
    /// order.
    pub objects: Vec<ObjRef>,
}

impl StructTree {
    /// The content of `page` in tree order, `list` being its display list. Marked content
    /// in form XObjects (`/Stm` references) is not supported: it reads as the page's own.
    pub fn page_content(&self, doc: &Document, page: ObjRef, list: &DisplayList) -> PageContent {
        if !self.has_page(page) {
            return PageContent::default();
        }
        self.page_content_of(page, &extract_marked(doc, list), &marked_bounds(list))
    }

    /// Like [`StructTree::page_content`], given the text of each marked-content sequence
    /// of the page by MCID ([`pagebara_content::extract_marked`]) and the bounds of what else
    /// they draw ([`pagebara_content::marked_bounds`]).
    pub fn page_content_of(
        &self,
        page: ObjRef,
        marked: &BTreeMap<i64, TextPage>,
        drawn: &BTreeMap<i64, Rect>,
    ) -> PageContent {
        let mut w = Walk {
            tree: self,
            page,
            marked,
            drawn,
            out: PageContent::default(),
            cur: None,
        };
        let lang = self.lang();
        for &r in self.roots() {
            if self.touches(r, page) {
                w.element(r, lang, 0);
            }
        }
        w.flush();
        let mut seen = std::collections::HashSet::new();
        w.out.objects.retain(|o| seen.insert(*o));
        w.out
    }
}

struct Walk<'a> {
    tree: &'a StructTree,
    page: ObjRef,
    marked: &'a BTreeMap<i64, TextPage>,
    drawn: &'a BTreeMap<i64, Rect>,
    out: PageContent,
    /// The block being collected.
    cur: Option<Block>,
}

impl<'a> Walk<'a> {
    /// An element outside any block: groups are walked, anything else is a block.
    fn element(&mut self, e: usize, lang: Option<&'a str>, sections: u8) {
        let tree = self.tree;
        let el = &tree.elements()[e];
        let lang = el.lang.as_deref().or(lang);
        match el.role {
            Role::Artifact => {}
            Role::Group | Role::Section | Role::List | Role::Table => {
                self.flush();
                let sections = sections + u8::from(el.role == Role::Section);
                self.kids(e, lang, sections, true);
                self.flush();
            }
            role => {
                self.flush();
                self.open(kind(role, sections), lang);
                let figure = matches!(role, Role::Figure | Role::Formula);
                match el
                    .actual_text
                    .as_ref()
                    .or(el.alt.as_ref().filter(|_| figure))
                {
                    Some(t) => self.replace(e, t),
                    None => self.kids(e, lang, sections, false),
                }
                if self.cur.as_ref().is_some_and(|b| b.text.trim().is_empty())
                    && let Some(alt) = &el.alt
                {
                    self.replace(e, alt);
                }
                self.flush();
            }
        }
    }

    /// The kids of an element; `group` when outside any block, where direct content reads
    /// as paragraphs.
    fn kids(&mut self, e: usize, lang: Option<&'a str>, sections: u8, group: bool) {
        let tree = self.tree;
        for kid in &tree.elements()[e].kids {
            match *kid {
                Kid::Element(c) => {
                    if !tree.touches(c, self.page) {
                        continue;
                    }
                    let el = &tree.elements()[c];
                    let split = group
                        || matches!(el.role, Role::List | Role::Table)
                        || matches!(el.role, Role::Figure | Role::Formula)
                            && el.actual_text.is_none()
                            && el.alt.is_some();
                    if split {
                        // A block of its own; the enclosing block carries on after it.
                        let resume = self.cur.as_ref().map(|b| (b.kind, b.lang.clone()));
                        self.element(c, lang, sections);
                        if let Some((kind, lang)) = resume
                            && !group
                        {
                            self.cur = Some(Block {
                                kind,
                                lang,
                                text: String::new(),
                                chars: Vec::new(),
                            });
                        }
                    } else if el.role != Role::Artifact {
                        match &el.actual_text {
                            Some(t) => self.replace(c, t),
                            None => self.kids(c, el.lang.as_deref().or(lang), sections, false),
                        }
                    }
                }
                Kid::Content { page, mcid, .. } if page == Some(self.page) => {
                    if self.cur.is_none() {
                        self.open(ItemKind::Paragraph, lang);
                    }
                    if let Some(t) = self.marked.get(&mcid) {
                        self.append_text(t);
                    }
                }
                Kid::Object { page, object } if page == Some(self.page) => {
                    self.out.objects.push(object);
                }
                _ => {}
            }
        }
    }

    fn open(&mut self, kind: ItemKind, lang: Option<&str>) {
        self.cur = Some(Block {
            kind,
            lang: lang.map(str::to_owned),
            text: String::new(),
            chars: Vec::new(),
        });
    }

    /// Ends the current block, keeping it if it has text or is a figure.
    fn flush(&mut self) {
        if let Some(b) = self.cur.take()
            && (b.kind == ItemKind::Figure || !b.text.trim().is_empty())
        {
            self.out.blocks.push(b);
        }
    }

    fn append_text(&mut self, t: &TextPage) {
        let Some(b) = self.cur.as_mut() else {
            return;
        };
        let first = t.chars.iter().find(|c| c.text.0 < c.text.1).map(|c| c.quad);
        separate(b, &t.text, first);
        let off = b.text.len() as u32;
        b.text.push_str(&t.text);
        b.chars.extend(
            t.chars
                .iter()
                .filter(|c| c.text.0 < c.text.1)
                .map(|c| (c.text.0 + off..c.text.1 + off, c.quad)),
        );
    }

    /// Appends `text` for the content of element `e` (its `/ActualText` or `/Alt`), with one
    /// box around that content, and collects the objects in it.
    fn replace(&mut self, e: usize, text: &str) {
        let mut bounds: Option<Rect> = None;
        self.content_bounds(e, &mut bounds, 0);
        let Some(b) = self.cur.as_mut() else {
            return;
        };
        let quad = bounds.map(|r| r.corners());
        separate(b, text, quad);
        let start = b.text.len() as u32;
        b.text.push_str(text);
        if let Some(q) = quad {
            b.chars.push((start..b.text.len() as u32, q));
        }
    }

    /// The bounds of the content of `e` on the page (its text, and its drawing for figures);
    /// also collects the objects in it.
    fn content_bounds(&mut self, e: usize, out: &mut Option<Rect>, depth: u32) {
        let tree = self.tree;
        if depth > 256 {
            return;
        }
        for kid in &tree.elements()[e].kids {
            match *kid {
                Kid::Element(c) if tree.touches(c, self.page) => {
                    self.content_bounds(c, out, depth + 1);
                }
                Kid::Content { page, mcid, .. } if page == Some(self.page) => {
                    let text = self.marked.get(&mcid).into_iter().flat_map(|t| &t.chars);
                    for r in text.map(|c| c.bbox()).chain(self.drawn.get(&mcid).copied()) {
                        *out = Some(out.map_or(r, |o| o.union(&r)));
                    }
                }
                Kid::Object { page, object } if page == Some(self.page) => {
                    self.out.objects.push(object);
                }
                _ => {}
            }
        }
    }
}

/// What separates the text of a block from the next piece, which starts with a character
/// in box `next`: nothing within a word, a space between words on a line, else a new line.
fn separate(b: &mut Block, next_text: &str, next: Option<[Point; 4]>) {
    if b.text.is_empty()
        || b.text.ends_with(char::is_whitespace)
        || next_text.starts_with(char::is_whitespace)
    {
        return;
    }
    let sep = match (b.chars.last().map(|c| c.1), next) {
        (Some(a), Some(n)) => {
            let h = (a[3].y - a[0].y).abs().max(1e-3);
            if (a[0].y - n[0].y).abs() < 0.5 * h && n[0].x >= a[0].x {
                if n[0].x - a[1].x > 0.2 * h { " " } else { "" }
            } else {
                "\n"
            }
        }
        _ => "\n",
    };
    b.text.push_str(sep);
}

fn kind(role: Role, sections: u8) -> ItemKind {
    match role {
        Role::Heading(Some(l)) => ItemKind::Heading(l.clamp(1, 6)),
        Role::Heading(None) => ItemKind::Heading(sections.clamp(1, 6)),
        Role::ListItem => ItemKind::ListItem,
        Role::Cell => ItemKind::Cell,
        Role::Figure | Role::Formula => ItemKind::Figure,
        Role::Caption => ItemKind::Caption,
        Role::Quote | Role::BlockQuote => ItemKind::Quote,
        Role::Note => ItemKind::Note,
        Role::Code => ItemKind::Code,
        _ => ItemKind::Paragraph,
    }
}
