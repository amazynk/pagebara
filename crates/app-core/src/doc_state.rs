//! What the app remembers per document without touching the file: layout, page theme,
//! the last page and the user's bookmarks. Kept in a JSON file next to the settings,
//! keyed by the file's path and identity (its `/ID`, else a hash of its bytes), so a
//! moved file keeps its bookmarks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pagebara_syntax::{Dict, Document, ObjRef, Object};
use serde::{Deserialize, Serialize};

use crate::page_theme::PageTheme;
use crate::view::LayoutMode;

/// Documents remembered; the least recently used are forgotten.
const MAX_DOCS: usize = 500;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    /// 0-based.
    pub page: usize,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocState {
    pub layout: LayoutMode,
    pub theme: PageTheme,
    /// 0-based.
    pub page: usize,
    pub bookmarks: Vec<Bookmark>,
}

impl DocState {
    /// Adds a bookmark for `page`, or removes the one there; returns whether one was added.
    pub fn toggle_bookmark(&mut self, page: usize, title: impl Into<String>) -> bool {
        if let Some(i) = self.bookmarks.iter().position(|b| b.page == page) {
            self.bookmarks.remove(i);
            return false;
        }
        let at = self.bookmarks.partition_point(|b| b.page < page);
        self.bookmarks.insert(
            at,
            Bookmark {
                page,
                title: title.into(),
            },
        );
        true
    }

    pub fn has_bookmark(&self, page: usize) -> bool {
        self.bookmarks.iter().any(|b| b.page == page)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    docs: BTreeMap<String, DocState>,
    /// Keys, most recently used last.
    order: Vec<String>,
}

/// The remembered documents.
#[derive(Debug, Default)]
pub struct DocStates {
    path: Option<PathBuf>,
    store: Store,
}

impl DocStates {
    /// The states stored at `path` (`None`: in memory only).
    pub fn load(path: Option<PathBuf>) -> Self {
        let store = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        DocStates { path, store }
    }

    /// The state of the document with `key` (see [`identity`]); a moved or renamed file is
    /// found by its identity alone.
    pub fn get(&self, key: &str) -> Option<&DocState> {
        if let Some(s) = self.store.docs.get(key) {
            return Some(s);
        }
        let id = id_part(key)?;
        self.store
            .order
            .iter()
            .rev()
            .filter(|k| id_part(k) == Some(id))
            .find_map(|k| self.store.docs.get(k))
    }

    pub fn set(&mut self, key: &str, state: DocState) {
        self.store.order.retain(|k| k != key);
        self.store.order.push(key.to_owned());
        self.store.docs.insert(key.to_owned(), state);
        while self.store.order.len() > MAX_DOCS {
            let old = self.store.order.remove(0);
            self.store.docs.remove(&old);
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(&self.store).map_err(std::io::Error::other)?;
        crate::session::write_atomic(path, &json)
    }
}

fn id_part(key: &str) -> Option<&str> {
    key.rsplit_once('|')
        .map(|(_, id)| id)
        .filter(|s| !s.is_empty())
}

/// The key of a document: its absolute path and `/ID` (or, without one, a hash of its
/// bytes), as `path|id`.
pub fn identity(path: &Path, doc: &Document) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let id = doc
        .trailer()
        .get(b"ID")
        .and_then(Object::as_array)
        .and_then(|a| a.first())
        .and_then(Object::as_string)
        .filter(|s| !s.is_empty())
        .map(hex)
        .unwrap_or_else(|| format!("h{:016x}", fnv1a(doc.source())));
    format!("{}|{id}", abs.to_string_lossy())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// FNV-1a, 64 bit: enough to tell files apart, not a security hash.
pub fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &c| {
        (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A PDF text string: PDFDocEncoding-compatible ASCII as is, else UTF-16BE with a BOM.
fn text_string(s: &str) -> Object {
    if s.chars().all(|c| (' '..='~').contains(&c)) {
        return Object::string(s.as_bytes());
    }
    let mut b = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        b.extend(u.to_be_bytes());
    }
    Object::string(&b)
}

/// Appends `bookmarks` to the document outline (creating one if needed), each pointing
/// at the top of its page.
pub fn add_to_outline(
    doc: &mut Document,
    pages: &[ObjRef],
    bookmarks: &[Bookmark],
) -> Result<(), String> {
    let cat_ref = doc
        .trailer()
        .get(b"Root")
        .and_then(Object::as_objref)
        .ok_or("the document has no catalog")?;
    let mut cat = doc
        .get(cat_ref)
        .and_then(|o| o.as_dict().cloned())
        .ok_or("the catalog is not a dictionary")?;
    // The outline root, made indirect if it was not.
    let root_ref = match cat.get(b"Outlines") {
        Some(Object::Ref(r)) if doc.get(*r).is_some_and(|o| o.as_dict().is_some()) => *r,
        other => {
            let mut d = other.and_then(|o| o.as_dict().cloned()).unwrap_or_default();
            d.set(b"Type", Object::name(b"Outlines"));
            let r = doc.add(Object::Dict(d));
            cat.set(b"Outlines", Object::Ref(r));
            doc.set(cat_ref, Object::Dict(cat));
            r
        }
    };
    let mut root: Dict = doc
        .get(root_ref)
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    let mut last = root.get(b"Last").and_then(Object::as_objref);
    let mut count = root.int(b"Count").unwrap_or(0);
    for b in bookmarks {
        let page = *pages
            .get(b.page)
            .ok_or_else(|| format!("there is no page {}", b.page + 1))?;
        let mut item = Dict::new();
        item.set(b"Title", text_string(&b.title));
        item.set(b"Parent", Object::Ref(root_ref));
        item.set(
            b"Dest",
            Object::Array(vec![
                Object::Ref(page),
                Object::name(b"XYZ"),
                Object::Null,
                Object::Null,
                Object::Null,
            ]),
        );
        if let Some(prev) = last {
            item.set(b"Prev", Object::Ref(prev));
        }
        let r = doc.add(Object::Dict(item));
        match last.and_then(|p| doc.get(p).and_then(|o| o.as_dict().cloned())) {
            Some(mut prev) => {
                prev.set(b"Next", Object::Ref(r));
                doc.set(last.expect("matched"), Object::Dict(prev));
            }
            None => root.set(b"First", Object::Ref(r)),
        }
        last = Some(r);
        // A negative count means a closed outline; open items add to its magnitude.
        count += if count < 0 { -1 } else { 1 };
    }
    if let Some(l) = last {
        root.set(b"Last", Object::Ref(l));
    }
    root.set(b"Count", Object::Int(count));
    doc.set(root_ref, Object::Dict(root));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_toggle_in_page_order() {
        let mut s = DocState::default();
        assert!(s.toggle_bookmark(5, "Five"));
        assert!(s.toggle_bookmark(1, "One"));
        assert!(s.toggle_bookmark(3, "Three"));
        let pages: Vec<usize> = s.bookmarks.iter().map(|b| b.page).collect();
        assert_eq!(pages, [1, 3, 5]);
        assert!(!s.toggle_bookmark(3, ""));
        assert!(!s.has_bookmark(3));
        assert!(s.has_bookmark(5));
    }

    #[test]
    fn states_round_trip_and_follow_moved_files() {
        let dir = std::env::temp_dir().join(format!("pagebara-docstate-{}", std::process::id()));
        let path = dir.join("documents.json");
        let mut st = DocStates::load(Some(path.clone()));
        let state = DocState {
            layout: LayoutMode::Book,
            theme: PageTheme::Sepia,
            page: 7,
            bookmarks: vec![Bookmark {
                page: 2,
                title: "Résumé".into(),
            }],
        };
        st.set("/a/x.pdf|abcd", state.clone());
        st.save().unwrap();
        let st = DocStates::load(Some(path));
        assert_eq!(st.get("/a/x.pdf|abcd"), Some(&state));
        assert_eq!(st.get("/b/moved.pdf|abcd"), Some(&state));
        assert_eq!(st.get("/a/x.pdf|other"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identity_uses_the_id_or_a_hash() {
        let doc = Document::open(crate::test_pdf::text(&["Hello"])).unwrap();
        let k = identity(Path::new("/tmp/a.pdf"), &doc);
        assert!(k.starts_with("/tmp/a.pdf|h"), "{k}");
        assert_eq!(k, identity(Path::new("/tmp/a.pdf"), &doc));
    }

    #[test]
    fn bookmarks_go_into_the_outline() {
        let mut s = crate::session::Session::new(
            None,
            Document::open(crate::test_pdf::text(&["Hello"])).unwrap(),
        );
        let refs = s.tree.refs();
        let marks = [
            Bookmark {
                page: 0,
                title: "Start".into(),
            },
            Bookmark {
                page: 0,
                title: "Again ✓".into(),
            },
        ];
        s.edit("Bookmarks", |d| add_to_outline(d, &refs, &marks))
            .unwrap();
        let titles: Vec<String> = s.outline.iter().map(|o| o.title.clone()).collect();
        assert_eq!(titles, ["Start", "Again ✓"]);
        assert!(s.outline.iter().all(|o| o.page == Some(0)));
        assert!(
            add_to_outline(
                &mut s.doc().clone(),
                &refs,
                &[Bookmark {
                    page: 9,
                    title: "x".into()
                }]
            )
            .is_err()
        );
    }
}
