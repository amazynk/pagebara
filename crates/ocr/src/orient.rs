//! Page orientation: which way a scan has to turn (0, 90, 180 or 270 degrees) for its text
//! to read upright.
//!
//! Two PP-LCNet classifiers decide it, with the detected text lines as a check: the
//! document classifier (PP-LCNet_x1_0_doc_ori) looks at the whole page, the text line
//! classifier (PP-LCNet_x1_0_textline_ori) tells upright lines from upside-down ones. When
//! the document classifier is unsure, the lines' shape gives the axis (lines run along the
//! longer side of their boxes) and the line classifier the direction along it.
//!
//! Tall lines are either a page turned a quarter or vertical writing (Chinese, Japanese,
//! Korean columns). The recogniser reads the longest ones downwards and upwards, and the
//! more confident reading wins. CJK reads downwards both as vertical writing and as
//! horizontal lines turned clockwise, so then single characters decide (the top of each
//! column, one-character lines): read as they stand and turned a quarter, upright ones mean
//! vertical writing, read column by column where it is; characters on their sides mean a
//! turned page. An upright page with tall lines is checked the same way (see
//! `Engine::orient`).

use rten::Model;
use rten_tensor::NdTensor;
use rten_tensor::prelude::*;

use crate::Error;
use crate::image::{RgbImage, RotRect, warp};

/// ImageNet normalisation the classifiers were trained with (RGB).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// Lines classified for the direction check.
const SAMPLE_LINES: usize = 24;

/// The input tensor of an RGB image, normalised.
fn tensor(img: &RgbImage) -> NdTensor<f32, 4> {
    let (w, h) = (img.width, img.height);
    let mut input = vec![0.0f32; 3 * w * h];
    for (i, p) in img.data.as_chunks::<3>().0.iter().enumerate() {
        for c in 0..3 {
            input[c * w * h + i] = (f32::from(p[c]) / 255.0 - MEAN[c]) / STD[c];
        }
    }
    NdTensor::from_data([1, 3, h, w], input)
}

/// Class probabilities of each batch row (the models end in a softmax; normalised again in
/// case one does not).
fn probabilities(out: NdTensor<f32, 2>) -> Vec<Vec<f32>> {
    let [rows, classes] = out.shape();
    let v = out.to_vec();
    (0..rows)
        .map(|r| {
            let row = &v[r * classes..(r + 1) * classes];
            if row.iter().all(|p| (0.0..=1.0).contains(p))
                && (row.iter().sum::<f32>() - 1.0).abs() < 1e-3
            {
                return row.to_vec();
            }
            let m = row.iter().copied().fold(f32::MIN, f32::max);
            let e: Vec<f32> = row.iter().map(|x| (x - m).exp()).collect();
            let s: f32 = e.iter().sum();
            e.iter().map(|x| x / s).collect()
        })
        .collect()
}

/// Probabilities that the page shows its content turned by 0, 90, 180 and 270 degrees
/// clockwise.
pub(crate) fn page_classes(
    model: &Model,
    img: &RgbImage,
    run: impl Fn(&Model, NdTensor<f32, 4>) -> Result<NdTensor<f32, 2>, Error>,
) -> Result<[f32; 4], Error> {
    // Shorter side to 256, then the central 224 × 224 (as the model was trained).
    let (w, h) = (img.width.max(1), img.height.max(1));
    let s = 256.0 / w.min(h) as f32;
    let (rw, rh) = (
        ((w as f32 * s).round() as usize).max(224),
        ((h as f32 * s).round() as usize).max(224),
    );
    let small = img.resize(rw, rh);
    let (x0, y0) = ((rw - 224) / 2, (rh - 224) / 2);
    let mut crop = Vec::with_capacity(224 * 224 * 3);
    for y in y0..y0 + 224 {
        crop.extend_from_slice(&small.data[(y * rw + x0) * 3..(y * rw + x0 + 224) * 3]);
    }
    let crop = RgbImage {
        width: 224,
        height: 224,
        data: crop,
    };
    let p = probabilities(run(model, tensor(&crop))?);
    let p = p
        .first()
        .ok_or(Error::Inference("orientation output".into()))?;
    if p.len() != 4 {
        return Err(Error::BadModel(format!("{} orientation classes", p.len())));
    }
    Ok([p[0], p[1], p[2], p[3]])
}

/// The mean probability that `lines` (read along their `u` axis) are upside down.
pub(crate) fn upside_down(
    model: &Model,
    img: &RgbImage,
    lines: &[RotRect],
    run: impl Fn(&Model, NdTensor<f32, 4>) -> Result<NdTensor<f32, 2>, Error>,
) -> Result<Option<f32>, Error> {
    // The longest lines say the most.
    let mut order: Vec<&RotRect> = lines
        .iter()
        .filter(|r| r.width() > r.height() * 1.5)
        .collect();
    order.sort_by(|a, b| b.width().total_cmp(&a.width()));
    order.truncate(SAMPLE_LINES);
    if order.is_empty() {
        return Ok(None);
    }
    let (w, h) = (160, 80);
    let mut input = vec![0.0f32; order.len() * 3 * w * h];
    for (b, r) in order.iter().enumerate() {
        let crop = warp(img, r, w, h);
        let t = tensor(&crop).to_vec();
        input[b * 3 * w * h..(b + 1) * 3 * w * h].copy_from_slice(&t);
    }
    let p = probabilities(run(
        model,
        NdTensor::from_data([order.len(), 3, h, w], input),
    )?);
    // Weighted by length.
    let (mut sum, mut weight) = (0.0, 0.0);
    for (r, p) in order.iter().zip(&p) {
        let Some(&flip) = p.get(1) else {
            return Err(Error::BadModel("text line orientation classes".into()));
        };
        sum += flip * r.width();
        weight += r.width();
    }
    Ok(Some(sum / weight.max(1e-6)))
}

/// Whether most of the text (by length) runs along the image's vertical axis.
pub(crate) fn mostly_vertical(lines: &[RotRect]) -> bool {
    let (mut along, mut across) = (0.0, 0.0);
    for r in lines {
        let (w, h) = (r.width(), r.height());
        if w > h * 1.5 {
            along += w;
        } else if h > w * 1.5 {
            across += h;
        }
    }
    across > along
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_shapes_give_the_axis() {
        let r = |w: f32, h: f32| RotRect {
            origin: [0.0, 0.0],
            u: [w, 0.0],
            v: [0.0, h],
        };
        assert!(!mostly_vertical(&[r(100.0, 10.0), r(5.0, 30.0)]));
        assert!(mostly_vertical(&[
            r(10.0, 100.0),
            r(12.0, 120.0),
            r(50.0, 10.0)
        ]));
        assert!(!mostly_vertical(&[]));
    }

    #[test]
    fn probabilities_are_normalised() {
        let p = probabilities(NdTensor::from_data([1, 2], vec![2.0, 0.0]));
        assert!((p[0][0] - 0.8808).abs() < 1e-3);
        let p = probabilities(NdTensor::from_data([1, 2], vec![0.25, 0.75]));
        assert_eq!(p[0], [0.25, 0.75]);
    }
}
