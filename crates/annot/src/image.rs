//! Image stamps: RGBA pixels as an image XObject with a soft mask, placed as a Stamp
//! annotation or drawn into the page. This is how signatures are placed.

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::appearance::{Appearance, upright_frame};
use crate::content::Content;
use crate::font::Helvetica;
use crate::geom::Rect;
use crate::model::{Annotation, Flags, Kind};
use crate::{Error, Result, edit, page};

/// 8-bit RGBA pixels, rows top to bottom, not premultiplied.
#[derive(Clone, Copy, Debug)]
pub struct RgbaImage<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

/// Embeds `img` as a DeviceRGB image XObject (Flate-compressed) with a DeviceGray `/SMask`
/// when it has transparency. Several placements can share the returned reference.
pub fn embed_image(doc: &mut Document, img: RgbaImage<'_>) -> Result<ObjRef> {
    let n = (img.width as usize) * (img.height as usize);
    if n == 0 || img.pixels.len() != n * 4 {
        return Err(Error::Invalid("image size does not match its pixels"));
    }
    let mut rgb = Vec::with_capacity(n * 3);
    let mut alpha = Vec::with_capacity(n);
    for p in img.pixels.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[..3]);
        alpha.push(p[3]);
    }
    let base = |cs: &[u8]| {
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"XObject"));
        d.set(b"Subtype", Object::name(b"Image"));
        d.set(b"Width", img.width);
        d.set(b"Height", img.height);
        d.set(b"ColorSpace", Object::name(cs));
        d.set(b"BitsPerComponent", 8);
        d.set(b"Interpolate", true);
        d.set(b"Filter", Object::name(b"FlateDecode"));
        d
    };
    let mut d = base(b"DeviceRGB");
    if alpha.iter().any(|&a| a != 255) {
        let mask = doc.add(Stream::new(
            base(b"DeviceGray"),
            crate::deflate::zlib(&alpha),
        ));
        d.set(b"SMask", mask);
    }
    Ok(doc.add(Stream::new(d, crate::deflate::zlib(&rgb))))
}

/// Whether a placed image is an annotation (movable, removable) or part of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Placement {
    #[default]
    Annotation,
    Flatten,
}

/// Annotation fields for a placed image.
#[derive(Clone, Debug, Default)]
pub struct StampOptions {
    pub author: Option<String>,
    pub subject: Option<String>,
    pub contents: Option<String>,
    pub placement: Placement,
}

/// Draws the embedded image `image` into `rect` on `page`. Returns the Stamp annotation,
/// or `None` when flattened into the page content.
pub fn place_image(
    doc: &mut Document,
    page: ObjRef,
    image: ObjRef,
    rect: Rect,
    opts: &StampOptions,
) -> Result<Option<ObjRef>> {
    let pd = page::page_dict(doc, page)?;
    let rotation = page::page_rotation(doc, &pd);
    // Upright as the page is shown: on rotated pages the image is turned against `/Rotate`.
    let (frame, matrix) = upright_frame(rect, rotation);
    let mut res = Dict::new();
    let mut xo = Dict::new();
    xo.set(b"Im0", image);
    res.set(b"XObject", xo);
    let mut c = Content::new();
    c.save()
        .cm([frame.width(), 0.0, 0.0, frame.height(), frame.x0, frame.y0])
        .do_xobject("Im0")
        .restore();
    let form = Appearance {
        matrix,
        resources: res,
        content: c.0,
        ..Appearance::empty(frame)
    }
    .into_stream(doc, &Helvetica);
    let form = doc.add(form);
    if opts.placement == Placement::Flatten {
        let m = edit::form_matrix(doc, form, rect);
        page::draw_form(doc, page, form, m)?;
        return Ok(None);
    }
    let mut a = Annotation::new(Kind::Stamp { icon: None }, rect);
    a.author = opts.author.clone();
    a.subject = opts.subject.clone();
    a.contents = opts.contents.clone();
    a.flags = Flags::PRINT;
    a.rotate = Some(rotation);
    let r = edit::add(doc, page, &a)?;
    let mut d = doc
        .get(r)
        .and_then(|o| o.as_dict().cloned())
        .ok_or(Error::NotFound)?;
    let mut ap = Dict::new();
    ap.set(b"N", form);
    d.set(b"AP", ap);
    doc.set(r, d);
    Ok(Some(r))
}

/// [`embed_image`] then [`place_image`].
pub fn add_image_stamp(
    doc: &mut Document,
    page: ObjRef,
    img: RgbaImage<'_>,
    rect: Rect,
    opts: &StampOptions,
) -> Result<Option<ObjRef>> {
    let image = embed_image(doc, img)?;
    place_image(doc, page, image, rect, opts)
}
