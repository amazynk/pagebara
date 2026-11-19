//! Dropping objects nothing refers to.
//!
//! Page operations leave the objects of removed pages (contents, fonts, images, interior
//! tree nodes) where they are. An incremental save keeps them by design: it only appends
//! to the original file, whose earlier revision (and any signature over it) stays intact.
//! A full save of a [`compact`]ed document leaves them out.

use std::collections::HashSet;

use pagebara_syntax::{Document, Object};

/// Deletes the objects nothing reaches any more, so the next full save leaves them out.
/// Returns how many were deleted. See [`remove_unreachable`].
pub fn compact(doc: &mut Document) -> usize {
    remove_unreachable(doc)
}

/// A full save without unreachable objects; `doc` itself is left as it is.
pub fn save_compact(doc: &Document) -> pagebara_syntax::Result<Vec<u8>> {
    let mut d = doc.clone();
    compact(&mut d);
    d.save_full()
}

/// Deletes every object not reachable from the trailer, such as the contents of deleted
/// pages, so a full save leaves them out. Object and cross-reference streams are kept:
/// the original file's index still points into them. Returns how many were deleted.
pub fn remove_unreachable(doc: &mut Document) -> usize {
    let mut seen = HashSet::new();
    let mut stack: Vec<Object> = doc.trailer().iter().map(|(_, v)| v.clone()).collect();
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => {
                if seen.insert(r.num)
                    && let Some(o) = doc.get(r)
                {
                    stack.push((*o).clone());
                }
            }
            Object::Array(a) => stack.extend(a),
            Object::Dict(d) => stack.extend(d.0.into_iter().map(|(_, v)| v)),
            Object::Stream(s) => stack.extend(s.dict.0.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
    let mut n = 0;
    for num in doc.object_numbers() {
        if seen.contains(&num) {
            continue;
        }
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let structural = doc.get(r).is_some_and(|o| {
            o.as_stream()
                .and_then(|s| s.dict.name(b"Type"))
                .is_some_and(|t| t == b"XRef" || t == b"ObjStm")
        });
        if !structural {
            doc.delete(r);
            n += 1;
        }
    }
    n
}
