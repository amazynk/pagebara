//! Where the bytes of a file go: stream data per category, and the rest (objects,
//! cross-reference) as structure.

use std::collections::HashMap;

use pagebara_syntax::{Document, Object};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Images,
    Fonts,
    /// Page contents, forms and Type 3 glyphs.
    Content,
    /// XMP metadata.
    Metadata,
    /// Attached files.
    Attachments,
    /// Other streams: colour profiles, thumbnails, scripts, ….
    OtherStreams,
    /// Objects outside streams, object streams, the cross-reference.
    Structure,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Images,
        Category::Fonts,
        Category::Content,
        Category::Metadata,
        Category::Attachments,
        Category::OtherStreams,
        Category::Structure,
    ];

    pub fn title(self) -> String {
        use pagebara_i18n::tr;
        match self {
            Category::Images => tr!("Images"),
            Category::Fonts => tr!("Fonts"),
            Category::Content => tr!("Page content"),
            Category::Metadata => tr!("Metadata"),
            Category::Attachments => tr!("Attachments"),
            Category::OtherStreams => tr!("Other data"),
            // TRANSLATORS: the part of a PDF file that is its structure (object tables)
            Category::Structure => tr!("Structure"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategorySize {
    pub category: Category,
    pub before: u64,
    pub after: u64,
}

/// Stream bytes per category.
pub(crate) fn sizes(doc: &Document) -> HashMap<Category, u64> {
    // Roles given by the objects that point at streams.
    let mut role: HashMap<u32, Category> = HashMap::new();
    let nums = doc.object_numbers();
    for &num in &nums {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let d = match &*o {
            Object::Dict(d) => d,
            Object::Stream(s) => &s.dict,
            _ => continue,
        };
        let mut mark = |o: Option<&Object>, c: Category| {
            if let Some(r) = o.and_then(Object::as_objref) {
                role.insert(r.num, c);
            }
        };
        for k in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
            mark(d.get(k), Category::Fonts);
        }
        match d.name(b"Type") {
            Some(b"Page") => match doc
                .resolve(d.get(b"Contents").unwrap_or(&Object::Null))
                .as_array()
            {
                Some(a) => a.iter().for_each(|c| mark(Some(c), Category::Content)),
                None => mark(d.get(b"Contents"), Category::Content),
            },
            Some(b"Filespec") => {
                if let Some(ef) = doc.get_in(d, b"EF").as_dict() {
                    ef.iter()
                        .for_each(|(_, v)| mark(Some(v), Category::Attachments));
                }
            }
            _ => {}
        }
        if let Some(cp) = doc.get_in(d, b"CharProcs").as_dict() {
            cp.iter()
                .for_each(|(_, v)| mark(Some(v), Category::Content));
        }
    }
    let mut out = HashMap::new();
    for num in nums {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let Some(s) = o.as_stream() else { continue };
        let c = match (
            role.get(&num),
            s.dict.name(b"Subtype"),
            s.dict.name(b"Type"),
        ) {
            (Some(&c), _, _) => c,
            (_, _, Some(b"XRef" | b"ObjStm")) => continue,
            (_, Some(b"Image"), _) => Category::Images,
            (_, Some(b"Form"), _) => Category::Content,
            (_, _, Some(b"Metadata")) => Category::Metadata,
            (_, _, Some(b"EmbeddedFile")) => Category::Attachments,
            _ => Category::OtherStreams,
        };
        *out.entry(c).or_insert(0) += doc.raw(s).len() as u64;
    }
    out
}

/// Per category, with structure as the rest of each file.
pub(crate) fn compare(
    before: &HashMap<Category, u64>,
    total_before: u64,
    after: &HashMap<Category, u64>,
    total_after: u64,
) -> Vec<CategorySize> {
    let rest = |m: &HashMap<Category, u64>, total: u64| total.saturating_sub(m.values().sum());
    Category::ALL
        .into_iter()
        .map(|c| {
            let (b, a) = if c == Category::Structure {
                (rest(before, total_before), rest(after, total_after))
            } else {
                (
                    before.get(&c).copied().unwrap_or(0),
                    after.get(&c).copied().unwrap_or(0),
                )
            };
            CategorySize {
                category: c,
                before: b,
                after: a,
            }
        })
        .filter(|c| c.before > 0 || c.after > 0)
        .collect()
}
