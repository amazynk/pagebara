//! The graphics state (§8.4) as the interpreter tracks it.

use crate::display::{
    BlendMode, ColorSpace, Id, LineCap, LineJoin, NONE, RenderingIntent, TextMode,
};
use crate::geom::Matrix;

/// DeviceN allows at most this many components.
pub const MAX_COMPONENTS: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub space: ColorSpace,
    pub n: u8,
    pub comps: [f32; MAX_COMPONENTS],
    pub pattern: Id,
}

impl Color {
    pub fn new(space: ColorSpace, init: &[f32]) -> Self {
        let mut c = Color {
            space,
            n: init.len().min(MAX_COMPONENTS) as u8,
            comps: [0.0; MAX_COMPONENTS],
            pattern: NONE,
        };
        c.comps[..usize::from(c.n)].copy_from_slice(&init[..usize::from(c.n)]);
        c
    }

    pub fn components(&self) -> &[f32] {
        &self.comps[..usize::from(self.n)]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TextState {
    pub char_spacing: f32,
    pub word_spacing: f32,
    /// `Tz / 100`.
    pub hscale: f32,
    pub leading: f32,
    /// Entry of the list's fonts.
    pub font: Id,
    pub size: f32,
    pub mode: TextMode,
    pub rise: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct GState {
    pub ctm: Matrix,
    pub fill: Color,
    pub stroke: Color,
    pub line_width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    /// Dash lengths in the list's dash table.
    pub dash: (u32, u32),
    pub dash_phase: f32,
    pub stroke_adjust: bool,
    pub intent: RenderingIntent,
    pub flatness: f32,
    pub smoothness: f32,
    pub fill_alpha: f32,
    pub stroke_alpha: f32,
    pub blend: BlendMode,
    pub soft_mask: Id,
    pub alpha_is_shape: bool,
    pub text_knockout: bool,
    pub overprint_fill: bool,
    pub overprint_stroke: bool,
    pub overprint_mode: u8,
    pub text: TextState,
    /// Clips pushed since the page or form began.
    pub clips: u32,
    // Entries already in the list for the current values, or NONE.
    pub ctm_id: Id,
    pub fill_id: Id,
    pub stroke_id: Id,
    pub style_id: Id,
}

impl GState {
    pub fn new(ctm: Matrix) -> Self {
        let black = Color::new(ColorSpace::DeviceGray, &[0.0]);
        GState {
            ctm,
            fill: black,
            stroke: black,
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: (0, 0),
            dash_phase: 0.0,
            stroke_adjust: false,
            intent: RenderingIntent::default(),
            flatness: 1.0,
            smoothness: 0.0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend: BlendMode::Normal,
            soft_mask: NONE,
            alpha_is_shape: false,
            text_knockout: true,
            overprint_fill: false,
            overprint_stroke: false,
            overprint_mode: 0,
            text: TextState {
                char_spacing: 0.0,
                word_spacing: 0.0,
                hscale: 1.0,
                leading: 0.0,
                font: NONE,
                size: 0.0,
                mode: TextMode::Fill,
                rise: 0.0,
            },
            clips: 0,
            ctm_id: NONE,
            fill_id: NONE,
            stroke_id: NONE,
            style_id: NONE,
        }
    }

    /// Forgets the cached paint entries after a colour or compositing change.
    pub fn paints_changed(&mut self) {
        self.fill_id = NONE;
        self.stroke_id = NONE;
    }
}
