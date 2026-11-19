//! Putting a signature on a page, as an image Stamp annotation (or flattened into the page).
//!
//! Each signature image is embedded once per document and shared by every placement. The
//! image XObject carries a private `/PagebaraSignature` key (`<id>:<content hash>`) and the
//! catalog's `/PagebaraSignatureImages` dictionary maps those keys to the images, so the
//! sharing survives saving and reopening.

use pagebara_annot::{Placement, Point, Rect, StampOptions};
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::library::{Library, Signature};
use crate::raster::Rgba;
use crate::{Error, Result};

pub const POINTS_PER_MM: f64 = 72.0 / 25.4;

const IMAGE_KEY: &[u8] = b"PagebaraSignature";
const CATALOG_INDEX: &[u8] = b"PagebaraSignatureImages";

/// Where a placed copy goes, in page coordinates (points, origin bottom left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// Exactly this rectangle (the image is stretched to it).
    Rect(Rect),
    /// Centred on a point, `width` points wide as the page is shown (the signature's
    /// default width when `None`), with the image's aspect ratio.
    At { center: Point, width: Option<f64> },
}

/// Places `image` (the processed image of `sig`) on `page`, upright as the page is shown.
/// Returns the annotation, or `None` when flattened. Each placed copy is independent:
/// resize it by updating the annotation's rectangle.
pub fn place(
    doc: &mut Document,
    page: ObjRef,
    sig: &Signature,
    image: &Rgba,
    target: Target,
    placement: Placement,
) -> Result<Option<ObjRef>> {
    place_by(doc, page, sig, image, target, placement, None)
}

/// [`place`], with `author` as the annotation's author (`/T`), as the comments list
/// shows for every other annotation.
pub fn place_by(
    doc: &mut Document,
    page: ObjRef,
    sig: &Signature,
    image: &Rgba,
    target: Target,
    placement: Placement,
    author: Option<&str>,
) -> Result<Option<ObjRef>> {
    let rect = match target {
        Target::Rect(r) => r,
        Target::At { center, width } => {
            let w = width.unwrap_or(sig.default_width_mm * POINTS_PER_MM);
            if !(w.is_finite() && w > 0.0) {
                return Err(Error::Invalid("width must be positive"));
            }
            let h = w * f64::from(image.height.max(1)) / f64::from(image.width.max(1));
            let page_dict = doc
                .get(page)
                .and_then(|o| o.as_dict().cloned())
                .ok_or(pagebara_annot::Error::NotAPage)?;
            // On a quarter-turned page the shown width runs along the page's y axis.
            let (w, h) = match pagebara_annot::page_rotation(doc, &page_dict) {
                90 | 270 => (h, w),
                _ => (w, h),
            };
            Rect::from_size(center.x - w / 2.0, center.y - h / 2.0, w, h)
        }
    };
    let xobject = embed(doc, sig, image)?;
    let opts = StampOptions {
        author: author.filter(|a| !a.is_empty()).map(str::to_owned),
        subject: Some("Signature".into()),
        contents: Some(sig.name.clone()),
        placement,
    };
    Ok(pagebara_annot::place_image(
        doc, page, xobject, rect, &opts,
    )?)
}

/// The image XObject of `image` (the processed image of `sig`) in `doc`, embedded on first
/// use and shared after that.
pub fn embed(doc: &mut Document, sig: &Signature, image: &Rgba) -> Result<ObjRef> {
    let key = format!("{}:{:016x}", sig.id, content_hash(image));
    if let Some(r) = find(doc, &key) {
        return Ok(r);
    }
    let r = pagebara_annot::embed_image(doc, image.as_annot())?;
    let mut stream = doc
        .get(r)
        .and_then(|o| o.as_stream().cloned())
        .ok_or(Error::Invalid("embedded image missing"))?;
    stream.dict.set(IMAGE_KEY, Object::string(key.as_bytes()));
    doc.set(r, stream);
    if let Some(root) = doc.trailer().get(b"Root").and_then(Object::as_objref)
        && let Some(mut cat) = doc.get(root).and_then(|o| o.as_dict().cloned())
    {
        let mut index = doc
            .get_in(&cat, CATALOG_INDEX)
            .as_dict()
            .cloned()
            .unwrap_or_default();
        // Drop entries whose image is gone.
        index.0.retain(|(_, v)| {
            v.as_objref()
                .is_some_and(|r| doc.get(r).is_some_and(|o| o.as_stream().is_some()))
        });
        index.set(key.as_bytes(), r);
        cat.set(CATALOG_INDEX, index);
        doc.set(root, cat);
    }
    Ok(r)
}

/// An image embedded earlier under `key`, checked against the key it carries.
fn find(doc: &Document, key: &str) -> Option<ObjRef> {
    let cat = doc.catalog()?;
    let index = doc.get_in(cat.as_dict()?, CATALOG_INDEX);
    let r = index.as_dict()?.get(key.as_bytes())?.as_objref()?;
    let img = doc.get(r)?;
    let d: &Dict = &img.as_stream()?.dict;
    (doc.get_in(d, IMAGE_KEY).as_string() == Some(key.as_bytes())).then_some(r)
}

/// A stable 64-bit hash of the image's size and pixels.
fn content_hash(img: &Rgba) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mix = |h: u64, v: u64| {
        let h = (h ^ v).wrapping_mul(K);
        h ^ (h >> 29)
    };
    let mut h = mix(mix(0, u64::from(img.width)), u64::from(img.height));
    let (words, rest) = img.pixels.as_chunks::<8>();
    for w in words {
        h = mix(h, u64::from_le_bytes(*w));
    }
    for &b in rest {
        h = mix(h, u64::from(b));
    }
    mix(h, img.pixels.len() as u64)
}

impl Library {
    /// Places the signature `id` from this library.
    pub fn place(
        &self,
        doc: &mut Document,
        page: ObjRef,
        id: &str,
        target: Target,
        placement: Placement,
    ) -> Result<Option<ObjRef>> {
        let sig = self.get(id).ok_or(Error::NotFound)?;
        let image = self.image(id)?;
        place(doc, page, sig, &image, target, placement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_sees_every_pixel_and_the_shape() {
        let mut a = Rgba::new(5, 3);
        let base = content_hash(&a);
        assert_eq!(content_hash(&a.clone()), base);
        a.pixels[59] = 1;
        assert_ne!(content_hash(&a), base);
        let mut b = Rgba::new(3, 5);
        assert_ne!(content_hash(&b), base);
        b.pixels[0] = 0x80;
        assert_ne!(content_hash(&b), content_hash(&Rgba::new(3, 5)));
    }
}
