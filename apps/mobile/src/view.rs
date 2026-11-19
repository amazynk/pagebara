//! The page view of the window: `pagebara_ui::ViewBridge` showing its slots through the
//! `ViewState` global, tuned for phones and tablets.

use pagebara_app_core::view::{Placement, Tuning};
use pagebara_ui::{Models, ViewSink};
use slint::{ComponentHandle, Image};

use crate::{MobileWindow, PageSlot, TileSlot, ViewState};

pub type ViewBridge = pagebara_ui::ViewBridge<MobileWindow>;

pub fn tuning() -> Tuning {
    Tuning::mobile(pagebara_ui::total_memory())
}

impl ViewSink for MobileWindow {
    type Page = PageSlot;
    type Tile = TileSlot;

    fn page_slot(p: &Placement, number: i32, preview: Image) -> PageSlot {
        PageSlot {
            di: p.di,
            by: p.by,
            bx: p.bx,
            w: p.w,
            h: p.h,
            visible: true,
            number,
            preview,
        }
    }

    fn tile_slot(p: &Placement) -> TileSlot {
        TileSlot {
            di: p.di,
            by: p.by,
            bx: p.bx,
            w: p.w,
            h: p.h,
            visible: true,
        }
    }

    fn scale_factor(&self) -> f64 {
        f64::from(self.window().scale_factor())
    }

    fn show_models(&self, m: Models<PageSlot, TileSlot>) {
        let vs = self.global::<ViewState>();
        vs.set_pages(m.pages);
        vs.set_page_covered(m.covered);
        vs.set_tiles(m.tiles);
        vs.set_tile_images(m.tile_images);
        vs.set_fallback(m.fallback);
        vs.set_fallback_images(m.fallback_images);
    }

    fn show_transform(&self, k: f32, cx: f32, oy: f32, dpr: f32) {
        let vs = self.global::<ViewState>();
        vs.set_k(k);
        vs.set_cx(cx);
        vs.set_oy(oy);
        vs.set_dpr(dpr);
    }

    // Phones scroll by touch; there are no scrollbars.
    fn show_scrollbars(&self, _: f32, _: f32, _: f32, _: f32) {}
}
