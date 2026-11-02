//! Compile-time selectable page-snapshot service.
//!
//! Without `threaded`, requests are served on the caller thread. With
//! `threaded`, a worker owns the `Document` and only `PageSnapshot` values cross
//! the thread boundary. `Document` itself never needs to be `Send`.

#[cfg(not(feature = "threaded"))]
use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(feature = "threaded")]
use ofdbox_folio_ir::PageError;
use ofdbox_folio_render::PageSnapshot;

use crate::error::Error;
use crate::fonts::SystemFonts;
use crate::reader::{CacheOptions, Document, OpenOptions};

/// One result produced by a [`SnapshotService`].
#[derive(Debug)]
pub enum SnapshotEvent {
    /// A page snapshot is ready.
    Snapshot {
        /// Request identifier.
        request: u64,
        /// Immutable page snapshot.
        snapshot: Arc<PageSnapshot>,
    },
    /// A page failed to load.
    Error {
        /// Request identifier.
        request: u64,
        /// Error message.
        message: String,
    },
}

/// Opens documents and produces page snapshots on the selected execution model.
#[cfg(not(feature = "threaded"))]
pub struct SnapshotService {
    document: Document,
    events: VecDeque<SnapshotEvent>,
    next: u64,
}

#[cfg(not(feature = "threaded"))]
impl SnapshotService {
    /// Opens a document on the current thread.
    pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        let options = OpenOptions {
            cache: CacheOptions::default(),
            font_provider: Some(Box::new(SystemFonts::new())),
            ..OpenOptions::default()
        };
        Ok(Self {
            document: Document::open_path(path, options)?,
            events: VecDeque::new(),
            next: 1,
        })
    }

    /// Requests a page snapshot; the result is queued for [`Self::try_recv`].
    pub fn request(&mut self, page: usize) -> u64 {
        let request = self.next;
        self.next += 1;
        let event = match self.document.page_snapshot(page) {
            Ok(snapshot) => SnapshotEvent::Snapshot { request, snapshot },
            Err(error) => SnapshotEvent::Error {
                request,
                message: error.to_string(),
            },
        };
        self.events.push_back(event);
        request
    }

    /// Pops the next ready event.
    pub fn try_recv(&mut self) -> Option<SnapshotEvent> {
        self.events.pop_front()
    }
}

#[cfg(feature = "threaded")]
mod threaded {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread::{self, JoinHandle};

    use super::*;

    enum Command {
        Load { request: u64, page: usize },
    }

    /// Threaded snapshot service; the worker owns the `Document`.
    pub struct SnapshotService {
        tx: Sender<Command>,
        rx: Receiver<SnapshotEvent>,
        next: AtomicU64,
        worker: Option<JoinHandle<()>>,
    }

    impl SnapshotService {
        /// Starts a worker and opens the document there.
        pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
            let path = path.as_ref().to_path_buf();
            let (command_tx, command_rx) = mpsc::channel::<Command>();
            let (event_tx, event_rx) = mpsc::channel::<SnapshotEvent>();
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
            let worker = thread::Builder::new()
                .name("ofdbox-folio-pages".to_owned())
                .spawn(move || {
                    let options = OpenOptions {
                        cache: CacheOptions::default(),
                        font_provider: Some(Box::new(SystemFonts::new())),
                        ..OpenOptions::default()
                    };
                    let mut document = match Document::open_path(path, options) {
                        Ok(document) => {
                            let _ = ready_tx.send(Ok(()));
                            document
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    while let Ok(Command::Load { request, page }) = command_rx.recv() {
                        let event = match document.page_snapshot(page) {
                            Ok(snapshot) => SnapshotEvent::Snapshot { request, snapshot },
                            Err(error) => SnapshotEvent::Error {
                                request,
                                message: error.to_string(),
                            },
                        };
                        if event_tx.send(event).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|error| Error::Page(PageError::Io(error.to_string())))?;
            match ready_rx.recv() {
                Ok(Ok(())) => Ok(Self {
                    tx: command_tx,
                    rx: event_rx,
                    next: AtomicU64::new(1),
                    worker: Some(worker),
                }),
                Ok(Err(message)) => {
                    let _ = worker.join();
                    Err(Error::Page(PageError::Decode(message)))
                }
                Err(_) => {
                    let _ = worker.join();
                    Err(Error::Page(PageError::Io(
                        "page worker stopped during open".to_owned(),
                    )))
                }
            }
        }

        /// Queues a page request.
        pub fn request(&mut self, page: usize) -> u64 {
            let request = self.next.fetch_add(1, Ordering::Relaxed);
            let _ = self.tx.send(Command::Load { request, page });
            request
        }

        /// Pops the next ready event without blocking.
        pub fn try_recv(&mut self) -> Option<SnapshotEvent> {
            self.rx.try_recv().ok()
        }
    }

    impl Drop for SnapshotService {
        fn drop(&mut self) {
            let (tx, _) = mpsc::channel();
            let _ = std::mem::replace(&mut self.tx, tx);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(feature = "threaded")]
pub use threaded::SnapshotService;

#[cfg(test)]
mod tests {
    use super::{RenderEvent, RenderRequest, RenderService, SnapshotService};
    use ofdbox_folio_ir::{Page, PageInfo, Scene, Size};
    use ofdbox_folio_render::PageSnapshot;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn missing_path_fails_to_open() {
        let result = SnapshotService::open_path("Z:/definitely-missing-document.ofd");
        assert!(result.is_err());
    }

    #[cfg(not(feature = "threaded-render"))]
    #[test]
    fn cancel_discards_queued_events() {
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new();
        let id = service.request(RenderRequest {
            snapshot,
            width: 4,
            height: 4,
            dpi: 96.0,
        });
        service.cancel(id);
        assert!(service.try_recv().is_none());
    }

    #[test]
    fn render_service_produces_a_tile() {
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new();
        let request = service.request(RenderRequest {
            snapshot,
            width: 8,
            height: 8,
            dpi: 96.0,
        });
        let mut frame = None;
        for _ in 0..200 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame {
                    request: id,
                    width,
                    height,
                    rgba,
                } = event
                {
                    assert_eq!(id, request);
                    assert_eq!((width, height), (8, 8));
                    frame = Some(rgba);
                }
            }
            if frame.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(frame.expect("frame").len(), 8 * 8 * 4);
    }
}

/// A request to render one page snapshot into an RGBA tile.
#[derive(Clone, Debug)]
pub struct RenderRequest {
    /// Immutable page snapshot.
    pub snapshot: Arc<PageSnapshot>,
    /// Target tile width in pixels.
    pub width: u32,
    /// Target tile height in pixels.
    pub height: u32,
    /// Render DPI.
    pub dpi: f64,
}

/// One event produced by a [`RenderService`].
#[derive(Debug)]
pub enum RenderEvent {
    /// Rendering has started.
    Started {
        /// Request identifier.
        request: u64,
    },
    /// A rendered RGBA tile is ready.
    Frame {
        /// Request identifier.
        request: u64,
        /// Tile width.
        width: u32,
        /// Tile height.
        height: u32,
        /// Straight-alpha RGBA pixels.
        rgba: Vec<u8>,
    },
    /// Rendering failed.
    Error {
        /// Request identifier.
        request: u64,
        /// Error message.
        message: String,
    },
}

fn render_request(
    renderer: &mut ofdbox_folio_render::CpuRenderer,
    request: &RenderRequest,
) -> Result<Vec<u8>, String> {
    let mut target =
        ofdbox_folio_render::RasterTarget::from_pixels(request.width, request.height, request.dpi)
            .map_err(|error| error.to_string())?;
    renderer
        .render_plan_region(
            &request.snapshot.plan,
            &mut target,
            ofdbox_folio_render::RenderRegion::new((0.0, 0.0), request.dpi),
        )
        .map_err(|error| error.to_string())?;
    Ok(target.data().to_vec())
}

#[cfg(not(feature = "threaded-render"))]
/// Compile-time inline render service.
pub struct RenderService {
    renderer: ofdbox_folio_render::CpuRenderer,
    events: VecDeque<RenderEvent>,
    cancelled: std::collections::HashSet<u64>,
    next: u64,
}

#[cfg(not(feature = "threaded-render"))]
impl RenderService {
    /// Creates an inline render service.
    pub fn new() -> Self {
        Self {
            renderer: ofdbox_folio_render::CpuRenderer::with_defaults(),
            events: VecDeque::new(),
            cancelled: std::collections::HashSet::new(),
            next: 1,
        }
    }

    /// Renders synchronously and queues `Started` / `Frame` / `Error`.
    pub fn request(&mut self, request: RenderRequest) -> u64 {
        let id = self.next;
        self.next += 1;
        self.events.push_back(RenderEvent::Started { request: id });
        match render_request(&mut self.renderer, &request) {
            Ok(rgba) => self.events.push_back(RenderEvent::Frame {
                request: id,
                width: request.width,
                height: request.height,
                rgba,
            }),
            Err(message) => self.events.push_back(RenderEvent::Error {
                request: id,
                message,
            }),
        }
        id
    }

    /// Cancels a request; its queued events are discarded.
    pub fn cancel(&mut self, request: u64) {
        self.cancelled.insert(request);
    }

    /// Pops the next event, discarding cancelled requests.
    pub fn try_recv(&mut self) -> Option<RenderEvent> {
        while let Some(event) = self.events.pop_front() {
            let id = match &event {
                RenderEvent::Started { request }
                | RenderEvent::Frame { request, .. }
                | RenderEvent::Error { request, .. } => *request,
            };
            if !self.cancelled.contains(&id) {
                return Some(event);
            }
        }
        None
    }
}

#[cfg(not(feature = "threaded-render"))]
impl Default for RenderService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "threaded-render")]
mod render_thread {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread::{self, JoinHandle};

    use super::*;

    enum Command {
        Render { id: u64, request: RenderRequest },
        Cancel { id: u64 },
    }

    /// Compile-time threaded render service.
    pub struct RenderService {
        tx: Sender<Command>,
        rx: Receiver<RenderEvent>,
        next: AtomicU64,
        cancelled: HashSet<u64>,
        worker: Option<JoinHandle<()>>,
    }

    impl RenderService {
        /// Starts a worker owning a CPU renderer.
        pub fn new() -> Self {
            let (command_tx, command_rx) = mpsc::channel::<Command>();
            let (event_tx, event_rx) = mpsc::channel::<RenderEvent>();
            let worker = thread::Builder::new()
                .name("ofdbox-folio-render".to_owned())
                .spawn(move || {
                    let mut renderer = ofdbox_folio_render::CpuRenderer::with_defaults();
                    let mut cancelled = HashSet::new();
                    while let Ok(command) = command_rx.recv() {
                        match command {
                            Command::Cancel { id } => {
                                cancelled.insert(id);
                            }
                            Command::Render { id, request } => {
                                if cancelled.contains(&id) {
                                    continue;
                                }
                                if event_tx.send(RenderEvent::Started { request: id }).is_err() {
                                    break;
                                }
                                let event = match render_request(&mut renderer, &request) {
                                    Ok(rgba) => RenderEvent::Frame {
                                        request: id,
                                        width: request.width,
                                        height: request.height,
                                        rgba,
                                    },
                                    Err(message) => RenderEvent::Error {
                                        request: id,
                                        message,
                                    },
                                };
                                if event_tx.send(event).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                })
                .expect("render worker thread");
            Self {
                tx: command_tx,
                rx: event_rx,
                next: AtomicU64::new(1),
                cancelled: HashSet::new(),
                worker: Some(worker),
            }
        }

        /// Queues a render request.
        pub fn request(&mut self, request: RenderRequest) -> u64 {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let _ = self.tx.send(Command::Render { id, request });
            id
        }

        /// Marks a request as cancelled.
        pub fn cancel(&mut self, request: u64) {
            self.cancelled.insert(request);
            let _ = self.tx.send(Command::Cancel { id: request });
        }

        /// Pops the next event, dropping cancelled requests.
        pub fn try_recv(&mut self) -> Option<RenderEvent> {
            while let Ok(event) = self.rx.try_recv() {
                let id = match &event {
                    RenderEvent::Started { request }
                    | RenderEvent::Frame { request, .. }
                    | RenderEvent::Error { request, .. } => *request,
                };
                if !self.cancelled.contains(&id) {
                    return Some(event);
                }
            }
            None
        }
    }

    impl Default for RenderService {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for RenderService {
        fn drop(&mut self) {
            let (tx, _) = mpsc::channel();
            let _ = std::mem::replace(&mut self.tx, tx);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(feature = "threaded-render")]
pub use render_thread::RenderService;
