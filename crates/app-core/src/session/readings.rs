//! Pages in reading order, worked out on first use from any thread (screen reader text is
//! built on a worker while scrolling) and kept for the most recently used pages.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use pagebara_annot::DestPage;
use pagebara_syntax::{Document, ObjRef};
use pagebara_tagged::StructTree;
use parking_lot::Mutex;

use crate::reading_order::{self, PageReading};
use crate::text_index::TextIndex;

/// Pages whose reading is kept: a few screens' worth, for any document size.
const KEEP: usize = 64;

/// Page index to its reading and when it was last used, and the use clock.
type Kept = (HashMap<usize, (Arc<PageReading>, u64)>, u64);

/// The reading order of one document revision.
pub struct Readings {
    doc: Document,
    refs: Vec<ObjRef>,
    labels: Option<Vec<String>>,
    text: Arc<TextIndex>,
    structure: OnceLock<Option<Arc<StructTree>>>,
    pages: Mutex<Kept>,
}

impl std::fmt::Debug for Readings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Readings")
            .field("pages", &self.refs.len())
            .finish_non_exhaustive()
    }
}

impl Readings {
    pub(super) fn new(
        doc: Document,
        refs: Vec<ObjRef>,
        labels: Option<Vec<String>>,
        text: Arc<TextIndex>,
    ) -> Self {
        Readings {
            doc,
            refs,
            labels,
            text,
            structure: OnceLock::new(),
            pages: Mutex::default(),
        }
    }

    /// The document's logical structure (tagged PDF), read on first use.
    pub fn struct_tree(&self) -> Option<Arc<StructTree>> {
        self.structure
            .get_or_init(|| StructTree::load(&self.doc).map(Arc::new))
            .clone()
    }

    /// Page `i` when it has been worked out already.
    pub fn cached(&self, i: usize) -> Option<Arc<PageReading>> {
        let mut g = self.pages.lock();
        let (map, clock) = &mut *g;
        *clock += 1;
        map.get_mut(&i).map(|(r, used)| {
            *used = *clock;
            r.clone()
        })
    }

    /// Page `i` in reading order (see [`reading_order::page_reading`] for the cost).
    pub fn page(&self, i: usize) -> Arc<PageReading> {
        if let Some(r) = self.cached(i) {
            return r;
        }
        let r = Arc::new(match self.refs.get(i) {
            Some(&page) => reading_order::reading(
                &self.doc,
                self.struct_tree().as_deref(),
                page,
                &self.text.page(i),
                &|d| {
                    let i = match d {
                        DestPage::Ref(r) => self.refs.iter().position(|&p| p == r)?,
                        DestPage::Index(i) => i as usize,
                    };
                    (i < self.refs.len()).then(|| self.label(i))
                },
            ),
            None => PageReading::default(),
        });
        let mut g = self.pages.lock();
        let (map, clock) = &mut *g;
        *clock += 1;
        map.insert(i, (r.clone(), *clock));
        if map.len() > KEEP
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k)
        {
            map.remove(&oldest);
        }
        r
    }

    fn label(&self, i: usize) -> String {
        self.labels
            .as_ref()
            .and_then(|l| l.get(i).cloned())
            .unwrap_or_else(|| (i + 1).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    /// A document of `n` pages sharing one content stream.
    fn pages(n: usize) -> Vec<u8> {
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", i + 5)).collect();
        let mut objs = vec![
            "<</Type /Catalog /Pages 2 0 R>>".to_string(),
            format!("<</Type /Pages /Kids [{}] /Count {n}>>", kids.join(" ")),
            "<</Type /Font /Subtype /Type1 /BaseFont /Helvetica>>".into(),
            "<</Length 30>>stream\nBT /F1 12 Tf 72 700 Td (Hi) Tj ET\nendstream".into(),
        ];
        for _ in 0..n {
            objs.push(
                "<</Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
                 /Resources <</Font <</F1 3 0 R>>>> /Contents 4 0 R>>"
                    .into(),
            );
        }
        objs.push("<<>>".into());
        let objs: Vec<&str> = objs.iter().map(String::as_str).collect();
        crate::test_pdf::classic(&objs, 1)
    }

    #[test]
    fn keeps_the_recently_used_pages_only() {
        let s = Session::new(None, Document::open(pages(KEEP + 10)).unwrap());
        let r = s.readings();
        let first = r.page(0);
        assert_eq!(first.items[0].text, "Hi");
        for i in 1..KEEP + 10 {
            r.page(i);
            // Page 0 stays in use.
            assert!(r.cached(0).is_some());
        }
        assert_eq!(r.pages.lock().0.len(), KEEP);
        assert!(Arc::ptr_eq(&first, &r.page(0)));
        assert!(r.cached(1).is_none());
        assert!(r.cached(KEEP + 9).is_some());
    }
}
