//! Renders wanted tiles on a background pool. Jobs that are no longer wanted when a worker
//! picks them up are skipped, and running ones are cancelled, so fast scrolling and zooming
//! do not pile up stale work.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rayon::ThreadPool;

use super::{Key, PixelRect};

/// Pixel storage for a finished tile: premultiplied RGBA8, rows of `w * 4` bytes. The UI
/// provides its own type so tiles go to the screen without a copy.
pub trait TileBuffer: Send + 'static {
    fn new(w: u32, h: u32) -> Self;
    fn pixels_mut(&mut self) -> &mut [u8];
}

#[derive(Clone, Copy, Debug)]
pub struct Job {
    pub key: Key,
    pub page: usize,
    /// The page's displayed size in points.
    pub page_size: (f64, f64),
    /// Device pixels per point.
    pub scale: f64,
    /// The pixels to render, in the page's device-pixel space at `scale`.
    pub rect: PixelRect,
}

/// Draws pages.
pub trait PageRenderer: Send + Sync + 'static {
    /// Fills `out` (premultiplied RGBA8, `rect.w * rect.h * 4` bytes) with `job.rect` of the
    /// page. Returns false when it stopped early because `cancel` was set.
    fn render(&self, job: &Job, cancel: &AtomicBool, out: &mut [u8]) -> bool;

    /// The displayed size of a page in points, when the renderer knows it.
    fn page_size(&self, _page: usize) -> Option<(f64, f64)> {
        None
    }
}

/// The pool shared by all views, leaving one core for the UI thread.
pub fn render_pool() -> Arc<ThreadPool> {
    static POOL: OnceLock<Arc<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(2, |n| n.get());
        let pool = pagebara_thread::rayon_pool("ren-tile")
            .num_threads(n.saturating_sub(1).clamp(1, 12))
            .build()
            .expect("tile render pool");
        Arc::new(pool)
    })
    .clone()
}

struct Done<B> {
    key: Key,
    /// The renderer generation that made it.
    generation: u64,
    /// `None` for a skipped job.
    buf: Option<B>,
    took: Duration,
}

pub struct Scheduler<B> {
    pool: Arc<ThreadPool>,
    renderer: Arc<dyn PageRenderer>,
    /// Bumped when the renderer is replaced; older results are dropped.
    generation: u64,
    wanted: Arc<Mutex<HashSet<Key>>>,
    /// Running jobs and their cancel flags.
    in_flight: HashMap<Key, Arc<AtomicBool>>,
    tx: Sender<Done<B>>,
    rx: Receiver<Done<B>>,
    /// Set by the first finished job after a drain; wakes the UI once per batch.
    signalled: Arc<AtomicBool>,
    notify: Arc<dyn Fn() + Send + Sync>,
    rendered: u64,
    render_time: Duration,
}

impl<B> std::fmt::Debug for Scheduler<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("in_flight", &self.in_flight.len())
            .field("rendered", &self.rendered)
            .finish_non_exhaustive()
    }
}

impl<B: TileBuffer> Scheduler<B> {
    /// `notify` is called from a worker when finished tiles are waiting in
    /// [`Scheduler::drain`].
    pub fn new(
        pool: Arc<ThreadPool>,
        renderer: Arc<dyn PageRenderer>,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        Scheduler {
            pool,
            renderer,
            generation: 0,
            wanted: Arc::default(),
            in_flight: HashMap::new(),
            tx,
            rx,
            signalled: Arc::default(),
            notify: Arc::new(notify),
            rendered: 0,
            render_time: Duration::ZERO,
        }
    }

    /// Makes `jobs` (most urgent first) the wanted set; starts those not yet running and
    /// cancels running ones no longer wanted. `have` says which keys are already cached.
    pub fn want(&mut self, jobs: &[Job], have: impl Fn(&Key) -> bool) {
        let wanted: HashSet<Key> = jobs.iter().map(|j| j.key).collect();
        for (key, cancel) in &self.in_flight {
            cancel.store(!wanted.contains(key), Ordering::Relaxed);
        }
        *self.wanted.lock() = wanted;
        for job in jobs {
            if have(&job.key) || self.in_flight.contains_key(&job.key) {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            self.in_flight.insert(job.key, cancel.clone());
            self.spawn(*job, cancel);
        }
    }

    /// Renders with `renderer` from now on: running jobs are cancelled and their results
    /// dropped.
    pub fn set_renderer(&mut self, renderer: Arc<dyn PageRenderer>) {
        for cancel in self.in_flight.values() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.in_flight.clear();
        self.generation += 1;
        self.renderer = renderer;
    }

    pub fn renderer(&self) -> &Arc<dyn PageRenderer> {
        &self.renderer
    }

    fn spawn(&self, job: Job, cancel: Arc<AtomicBool>) {
        let renderer = self.renderer.clone();
        let generation = self.generation;
        let wanted = self.wanted.clone();
        let tx = self.tx.clone();
        let signalled = self.signalled.clone();
        let notify = self.notify.clone();
        self.pool.spawn(move || {
            let start = Instant::now();
            // Not in one expression: the guard would be held for the whole render.
            let still_wanted = wanted.lock().contains(&job.key);
            let buf = (still_wanted && !cancel.load(Ordering::Relaxed))
                .then(|| {
                    let mut buf = B::new(job.rect.w, job.rect.h);
                    renderer
                        .render(&job, &cancel, buf.pixels_mut())
                        .then_some(buf)
                })
                .flatten();
            let done = Done {
                key: job.key,
                generation,
                buf,
                took: start.elapsed(),
            };
            if tx.send(done).is_ok() && !signalled.swap(true, Ordering::AcqRel) {
                notify();
            }
        });
    }

    /// Finished tiles since the last call.
    pub fn drain(&mut self) -> Vec<(Key, B)> {
        self.signalled.store(false, Ordering::Release);
        let mut out = Vec::new();
        while let Ok(done) = self.rx.try_recv() {
            if done.generation != self.generation {
                continue;
            }
            self.in_flight.remove(&done.key);
            if let Some(buf) = done.buf {
                self.rendered += 1;
                self.render_time += done.took;
                out.push((done.key, buf));
            }
        }
        out
    }

    /// Jobs started and not drained yet.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Tiles rendered so far and their mean render time.
    pub fn rendered(&self) -> (u64, Duration) {
        let mean = self
            .render_time
            .checked_div(self.rendered.max(1) as u32)
            .unwrap_or_default();
        (self.rendered, mean)
    }
}
