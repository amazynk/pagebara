//! Images going in (embedding new pictures) and coming out (as PNG files).

use pagebara_content::{Matrix, Rect};
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::Error;

/// A picture to place.
#[derive(Clone, Copy, Debug)]
pub enum NewImage<'a> {
    /// 8-bit RGBA pixels, rows top to bottom, straight alpha.
    Rgba {
        width: u32,
        height: u32,
        pixels: &'a [u8],
    },
    /// A PNG file (decoded and stored losslessly).
    Png(&'a [u8]),
    /// A JPEG file, stored as it is.
    Jpeg(&'a [u8]),
}

/// How a replacement image takes the old one's place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageFit {
    /// Fills the old image's frame, stretched if the shapes differ.
    Stretch,
    /// Fits inside the old frame keeping its proportions, centred.
    #[default]
    Contain,
}

/// An image's size in pixels, read from its header.
pub fn image_size(img: &NewImage<'_>) -> Result<(u32, u32), Error> {
    let bad = |e: String| Error::Image(e);
    match *img {
        NewImage::Rgba { width, height, .. } => Ok((width, height)),
        NewImage::Png(d) => pagebara_png::read_info(d)
            .map(|i| (i.width, i.height))
            .map_err(|e| bad(e.to_string())),
        NewImage::Jpeg(d) => pagebara_jpeg::read_info(d)
            .map(|i| (i.width, i.height))
            .map_err(|e| bad(e.to_string())),
    }
}

/// Adds an image XObject; returns it with its size in pixels.
pub fn embed(doc: &mut Document, img: &NewImage<'_>) -> Result<(ObjRef, u32, u32), Error> {
    match *img {
        NewImage::Rgba {
            width,
            height,
            pixels,
        } => {
            let r = pagebara_annot::embed_image(
                doc,
                pagebara_annot::RgbaImage {
                    width,
                    height,
                    pixels,
                },
            )
            .map_err(|e| Error::Image(e.to_string()))?;
            Ok((r, width, height))
        }
        NewImage::Png(data) => {
            let opts = pagebara_png::DecodeOptions {
                output: pagebara_png::Output::Rgba8,
                ..Default::default()
            };
            let im = pagebara_png::decode(data, &opts).map_err(|e| Error::Image(e.to_string()))?;
            let rgba = im.to_rgba8();
            embed(
                doc,
                &NewImage::Rgba {
                    width: im.width,
                    height: im.height,
                    pixels: &rgba,
                },
            )
        }
        NewImage::Jpeg(data) => {
            let info = pagebara_jpeg::read_info(data).map_err(|e| Error::Image(e.to_string()))?;
            let (space, n): (&[u8], usize) = match info.components.len() {
                1 => (b"DeviceGray", 1),
                3 => (b"DeviceRGB", 3),
                4 => (b"DeviceCMYK", 4),
                _ => return Err(Error::Image("unsupported JPEG colour model".into())),
            };
            let mut d = Dict::new();
            d.set(b"Type", Object::name(b"XObject"));
            d.set(b"Subtype", Object::name(b"Image"));
            d.set(b"Width", info.width);
            d.set(b"Height", info.height);
            d.set(b"ColorSpace", Object::name(space));
            d.set(b"BitsPerComponent", 8);
            d.set(b"Filter", Object::name(b"DCTDecode"));
            if n == 4 && info.adobe_transform.is_some() {
                // Adobe CMYK JPEGs store inverted samples.
                d.set(
                    b"Decode",
                    Object::Array([1, 0, 1, 0, 1, 0, 1, 0].map(Object::Int).to_vec()),
                );
            }
            let r = doc.add(Stream::new(d, data));
            Ok((r, info.width, info.height))
        }
    }
}

/// The matrix in an image's unit square placing a `w`×`h` image in the frame `placement`
/// maps the unit square to.
pub fn fit(placement: &Matrix, w: u32, h: u32, how: ImageFit) -> Matrix {
    if how == ImageFit::Stretch || w == 0 || h == 0 {
        return Matrix::IDENTITY;
    }
    let fw = placement.a.hypot(placement.b);
    let fh = placement.c.hypot(placement.d);
    if fw <= 0.0 || fh <= 0.0 {
        return Matrix::IDENTITY;
    }
    let frame = fw / fh;
    let img = w as f32 / h as f32;
    if img > frame {
        let sy = frame / img;
        Matrix::new(1.0, 0.0, 0.0, sy, 0.0, (1.0 - sy) / 2.0)
    } else {
        let sx = img / frame;
        Matrix::new(sx, 0.0, 0.0, 1.0, (1.0 - sx) / 2.0, 0.0)
    }
}

/// The page-space matrix drawing a `w`×`h` image into `rect` keeping its proportions.
pub fn placement_in(rect: &Rect, w: u32, h: u32) -> Matrix {
    let m = Matrix::new(rect.width(), 0.0, 0.0, rect.height(), rect.x0, rect.y0);
    fit(&m, w, h, ImageFit::Contain).then(&m)
}

/// An image stream rendered at its own size, as a PNG file (RGBA, or RGB when opaque):
/// colour spaces, decode arrays, masks and soft masks applied.
pub fn to_png(doc: &Document, image: &Stream) -> Result<Vec<u8>, Error> {
    let int = |k: &[u8]| doc.get_in(&image.dict, k).as_int().unwrap_or(0);
    let (w, h) = (int(b"Width"), int(b"Height"));
    if w <= 0 || h <= 0 || w > 30_000 || h > 30_000 {
        return Err(Error::Image("unusable image size".into()));
    }
    let mut tmp = doc.clone();
    let img = tmp.add(image.clone());
    let content = format!("q {w} 0 0 {h} 0 0 cm /Im Do Q");
    let contents = tmp.add(Stream::new(Dict::new(), content.into_bytes()));
    let pages = tmp
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"Pages")?.as_objref())
        .ok_or(Error::NotAPage)?;
    let mut xo = Dict::new();
    xo.set(b"Im", Object::Ref(img));
    let mut res = Dict::new();
    res.set(b"XObject", Object::Dict(xo));
    let mut pd = Dict::new();
    pd.set(b"Type", Object::name(b"Page"));
    pd.set(b"Parent", Object::Ref(pages));
    pd.set(
        b"MediaBox",
        Object::Array(vec![0.into(), 0.into(), w.into(), h.into()]),
    );
    pd.set(b"Rotate", 0);
    pd.set(b"Resources", Object::Dict(res));
    pd.set(b"Contents", Object::Ref(contents));
    let page = tmp.add(pd);
    let mut root = tmp
        .get(pages)
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    root.set(b"Kids", Object::Array(vec![Object::Ref(page)]));
    root.set(b"Count", 1);
    for k in [&b"Resources"[..], b"MediaBox", b"CropBox", b"Rotate"] {
        root.remove(k);
    }
    tmp.set(pages, root);
    let r = Renderer::new(tmp);
    let mut req = RenderRequest::new(
        1.0,
        PixelRect {
            x: 0,
            y: 0,
            w: w as u32,
            h: h as u32,
        },
    );
    req.background = None;
    req.straight_alpha = true;
    req.annotations = Annotations::None;
    let out = r.render(0, &req).map_err(|e| Error::Image(e.to_string()))?;
    let opaque = out.data.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
    let png = if opaque {
        let rgb: Vec<u8> = out
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        pagebara_png::encode(out.width, out.height, pagebara_png::ColorType::Rgb, &rgb)
    } else {
        pagebara_png::encode(
            out.width,
            out.height,
            pagebara_png::ColorType::Rgba,
            &out.data,
        )
    };
    png.map_err(|e| Error::Image(e.to_string()))
}
