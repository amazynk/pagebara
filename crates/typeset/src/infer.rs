//! Recovering a paragraph's layout from its lines, so an edit reflows like the original.

use crate::layout::Align;

/// One existing line of a paragraph, in user space (y up).
#[derive(Clone, Debug, PartialEq)]
pub struct LineSample {
    /// Left and right edges of the line's glyphs (trailing spaces excluded).
    pub x0: f32,
    pub x1: f32,
    pub baseline: f32,
    /// Largest font size on the line.
    pub size: f32,
    /// Width of the line's first word plus a space before it: how much more room the
    /// previous line would have needed to take that word (0 if unknown).
    pub first_word: f32,
    /// Word spaces wider than the font's space (word spacing or `TJ` gaps): a hint that the
    /// line is justified.
    pub stretched: bool,
}

/// Paragraph properties inferred from its lines.
#[derive(Clone, Debug, PartialEq)]
pub struct InferredParagraph {
    /// Left edge of the box and its width.
    pub left: f32,
    pub width: f32,
    pub align: Align,
    /// Indent of the first line relative to the others.
    pub first_indent: f32,
    /// Baseline to baseline distance.
    pub line_height: f32,
    /// Baseline of the first line. [`layout`](crate::layout()) puts the first baseline one
    /// ascent below the box top, so the box top is this plus the first line's ascent.
    pub first_baseline: f32,
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// Infers alignment, box, first-line indent and line height from a paragraph's lines (top
/// to bottom). `None` for no lines.
pub fn infer_paragraph(lines: &[LineSample]) -> Option<InferredParagraph> {
    let first = lines.first()?;
    let n = lines.len();
    let size = median(lines.iter().map(|l| l.size).collect()).unwrap_or(12.0);
    let tol = (size * 0.05).max(0.5);
    let line_height = median(
        lines
            .windows(2)
            .map(|w| (w[0].baseline - w[1].baseline).abs())
            .collect(),
    )
    .unwrap_or(size * 1.2);
    if n == 1 {
        return Some(InferredParagraph {
            left: first.x0,
            width: first.x1 - first.x0,
            align: Align::Left,
            first_indent: 0.0,
            line_height,
            first_baseline: first.baseline,
        });
    }
    let near = |a: f32, b: f32| (a - b).abs() <= tol;
    let body = &lines[1..];
    let left = body.iter().map(|l| l.x0).fold(f32::INFINITY, f32::min);
    let lefts_equal = body.iter().all(|l| near(l.x0, left));
    let not_last = &lines[..n - 1];
    let right = not_last
        .iter()
        .map(|l| l.x1)
        .fold(f32::NEG_INFINITY, f32::max);
    let rights_equal = not_last.iter().all(|l| near(l.x1, right));
    let all_right = lines.iter().map(|l| l.x1).fold(f32::NEG_INFINITY, f32::max);
    let all_rights_equal = lines.iter().all(|l| near(l.x1, all_right));
    let centre = (first.x0 + first.x1) / 2.0;
    let centred = lines.iter().all(|l| near((l.x0 + l.x1) / 2.0, centre));
    let last = &lines[n - 1];
    let ragged_last = last.x1 < right - tol;
    let stretched = not_last.iter().any(|l| l.stretched);

    let align = if lefts_equal && rights_equal && (n >= 3 && ragged_last || stretched) {
        Align::Justify
    } else if lefts_equal && (near(first.x0, left) || first.x0 > left) && !centred {
        Align::Left
    } else if all_rights_equal && !lefts_equal {
        Align::Right
    } else if centred {
        Align::Center
    } else {
        Align::Left
    };
    // The box must hold every line but not the next line's first word.
    let bounds = |widths: &[f32]| -> f32 {
        let lower = widths.iter().copied().fold(0.0, f32::max);
        let upper = (0..n - 1)
            .filter(|&i| lines[i + 1].first_word > 0.0)
            .map(|i| widths[i] + lines[i + 1].first_word)
            .fold(f32::INFINITY, f32::min);
        if upper.is_finite() && upper > lower {
            (lower + upper) / 2.0
        } else {
            lower
        }
    };
    Some(match align {
        Align::Justify => InferredParagraph {
            left,
            width: right - left,
            align,
            first_indent: first.x0 - left,
            line_height,
            first_baseline: first.baseline,
        },
        Align::Left => {
            let widths: Vec<f32> = lines
                .iter()
                .enumerate()
                .map(|(i, l)| l.x1 - if i == 0 { first.x0 } else { left })
                .collect();
            let indent = first.x0 - left;
            // The first line's room is the box minus the indent.
            let w = bounds(
                &widths
                    .iter()
                    .enumerate()
                    .map(|(i, &w)| if i == 0 { w + indent } else { w })
                    .collect::<Vec<_>>(),
            );
            InferredParagraph {
                left,
                width: w,
                align,
                first_indent: indent,
                line_height,
                first_baseline: first.baseline,
            }
        }
        Align::Right => {
            let widths: Vec<f32> = lines.iter().map(|l| all_right - l.x0).collect();
            let w = bounds(&widths);
            InferredParagraph {
                left: all_right - w,
                width: w,
                align,
                first_indent: 0.0,
                line_height,
                first_baseline: first.baseline,
            }
        }
        Align::Center => {
            let widths: Vec<f32> = lines.iter().map(|l| l.x1 - l.x0).collect();
            let w = bounds(&widths);
            InferredParagraph {
                left: centre - w / 2.0,
                width: w,
                align,
                first_indent: 0.0,
                line_height,
                first_baseline: first.baseline,
            }
        }
    })
}
