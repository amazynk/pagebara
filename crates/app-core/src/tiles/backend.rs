//! How the view gets a page renderer: [`EngineBackend`] draws pages with `pagebara-render`.
//!
//! The app draws the annotations it edits over the pages itself (see
//! [`annots::drawn_by_app`]), so they can change without rendering the page again; the
//! renderer leaves those out and draws every other annotation from its appearance stream.
//! Shells without such an overlay use [`CompleteBackend`], which draws them all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use pagebara_render::{
    Annotations, DefaultGlyphs, Limits, PixelRect, RenderError, RenderRequest, Renderer,
};
use pagebara_syntax::{Document, ObjRef};

use super::{Job, PageRenderer};
use crate::annots;

/// What a renderer for one revision of a document's pages gets.
#[derive(Clone, Debug)]
pub struct RenderSource {
    pub doc: Document,
    /// The page objects, in order.
    pub refs: Vec<ObjRef>,
}

/// Makes page renderers; the app asks it again whenever the document changes.
pub trait RenderBackend: Send + Sync + std::fmt::Debug {
    fn renderer(&self, src: RenderSource) -> Arc<dyn PageRenderer>;
}

#[derive(Debug, Default)]
pub struct EngineBackend;

impl RenderBackend for EngineBackend {
    fn renderer(&self, src: RenderSource) -> Arc<dyn PageRenderer> {
        Arc::new(EngineRenderer::new(src))
    }
}

/// Draws every annotation into the pages, for shells that do not draw any themselves.
#[derive(Debug, Default)]
pub struct CompleteBackend;

impl RenderBackend for CompleteBackend {
    fn renderer(&self, src: RenderSource) -> Arc<dyn PageRenderer> {
        Arc::new(EngineRenderer::complete(src))
    }
}

/// Renders one revision of a document.
#[derive(Debug)]
pub struct EngineRenderer {
    renderer: Renderer,
    refs: Vec<ObjRef>,
    /// Per page, the annotations the app draws itself.
    own: Vec<OnceLock<Arc<Vec<ObjRef>>>>,
    /// Draws the annotations the app would draw too.
    complete: bool,
}

impl EngineRenderer {
    pub fn new(src: RenderSource) -> Self {
        EngineRenderer {
            complete: false,
            own: (0..src.refs.len()).map(|_| OnceLock::new()).collect(),
            renderer: Renderer::with_options(
                src.doc,
                Limits::for_this_machine(),
                Arc::new(DefaultGlyphs),
            ),
            refs: src.refs,
        }
    }

    /// A renderer that draws every annotation.
    pub fn complete(src: RenderSource) -> Self {
        EngineRenderer {
            complete: true,
            ..EngineRenderer::new(src)
        }
    }

    pub(crate) fn own(&self, page: usize) -> Arc<Vec<ObjRef>> {
        if self.complete {
            return Arc::default();
        }
        let Some(slot) = self.own.get(page) else {
            return Arc::default();
        };
        slot.get_or_init(|| {
            let doc = self.renderer.document();
            let list = self
                .refs
                .get(page)
                .map(|&r| annots::on_page(doc, r))
                .unwrap_or_default();
            Arc::new(
                list.iter()
                    .filter(|a| annots::drawn_by_app(doc, a))
                    .filter_map(|a| a.id)
                    .collect(),
            )
        })
        .clone()
    }
}

impl PageRenderer for EngineRenderer {
    fn render(&self, job: &Job, cancel: &AtomicBool, out: &mut [u8]) -> bool {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let req = RenderRequest {
            scale: job.scale as f32,
            rect: PixelRect {
                x: job.rect.x as i32,
                y: job.rect.y as i32,
                w: job.rect.w,
                h: job.rect.h,
            },
            rotation: 0,
            background: Some([255; 4]),
            annotations: Annotations::ViewExcept(self.own(job.page)),
            optional_content: None,
            cancel: Some(cancel),
            straight_alpha: false,
        };
        match self.renderer.render_into(job.page, &req, out) {
            Ok(()) => true,
            Err(RenderError::Cancelled) => false,
            Err(e) => {
                log::warn!("page {}: {e}", job.page + 1);
                out.fill(255);
                true
            }
        }
    }

    fn page_size(&self, page: usize) -> Option<(f64, f64)> {
        self.renderer
            .page_size(page)
            .map(|(w, h)| (f64::from(w), f64::from(h)))
    }
}
