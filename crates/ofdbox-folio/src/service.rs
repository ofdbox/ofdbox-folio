//! Compile-time selectable page-snapshot service.
//!
//! Without `threaded`, requests are served on the caller thread. With
//! `threaded`, a worker owns the `Document` and only `PageSnapshot` values cross
//! the thread boundary. `Document` itself never needs to be `Send`.

use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(feature = "threaded")]
use ofdbox_folio_ir::PageError;
use ofdbox_folio_render::PageSnapshot;

use crate::error::Error;
use crate::fonts::SystemFonts;
use crate::reader::{CacheOptions, Document, OpenOptions};
use ofdbox_folio_ir::FontProvider;

/// Creates a font provider inside the service worker.
pub trait FontProviderFactory: Send + Sync {
    /// Creates a provider owned by the calling thread.
    fn create(&self) -> Box<dyn FontProvider>;
}

/// Factory for the system font provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFontsFactory;

impl FontProviderFactory for SystemFontsFactory {
    fn create(&self) -> Box<dyn FontProvider> {
        Box::new(SystemFonts::new())
    }
}

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
    cancelled: std::collections::HashSet<u64>,
    next: u64,
}

#[cfg(not(feature = "threaded"))]
impl SnapshotService {
    /// Opens a document on the current thread.
    pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        Self::open_path_with_factory(path, Arc::new(SystemFontsFactory))
    }

    /// Opens a document with a custom font provider factory.
    pub fn open_path_with_factory(
        path: impl AsRef<std::path::Path>,
        factory: Arc<dyn FontProviderFactory>,
    ) -> Result<Self, Error> {
        let options = OpenOptions {
            cache: CacheOptions::default(),
            font_provider: Some(factory.create()),
            ..OpenOptions::default()
        };
        Ok(Self {
            document: Document::open_path(path, options)?,
            events: VecDeque::new(),
            cancelled: std::collections::HashSet::new(),
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

    /// Cancels a snapshot request; its queued events are discarded.
    pub fn cancel(&mut self, request: u64) {
        self.cancelled.insert(request);
    }

    /// Pops the next ready event.
    pub fn try_recv(&mut self) -> Option<SnapshotEvent> {
        while let Some(event) = self.events.pop_front() {
            let request = match &event {
                SnapshotEvent::Snapshot { request, .. } | SnapshotEvent::Error { request, .. } => {
                    *request
                }
            };
            if !self.cancelled.contains(&request) {
                return Some(event);
            }
        }
        None
    }
}

#[cfg(feature = "threaded")]
mod threaded {
    use std::collections::HashSet;
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
        cancelled: HashSet<u64>,
        next: AtomicU64,
        worker: Option<JoinHandle<()>>,
    }

    impl SnapshotService {
        /// Starts a worker and opens the document there.
        pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
            Self::open_path_with_factory(path, Arc::new(SystemFontsFactory))
        }

        /// Starts a worker with a custom font provider factory.
        pub fn open_path_with_factory(
            path: impl AsRef<std::path::Path>,
            factory: Arc<dyn FontProviderFactory>,
        ) -> Result<Self, Error> {
            let path = path.as_ref().to_path_buf();
            let (command_tx, command_rx) = mpsc::channel::<Command>();
            let (event_tx, event_rx) = mpsc::channel::<SnapshotEvent>();
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
            let worker = thread::Builder::new()
                .name("ofdbox-folio-pages".to_owned())
                .spawn(move || {
                    let options = OpenOptions {
                        cache: CacheOptions::default(),
                        font_provider: Some(factory.create()),
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
                    cancelled: HashSet::new(),
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

        /// Cancels a snapshot request; its events are discarded.
        pub fn cancel(&mut self, request: u64) {
            self.cancelled.insert(request);
        }

        /// Pops the next ready event without blocking.
        pub fn try_recv(&mut self) -> Option<SnapshotEvent> {
            while let Ok(event) = self.rx.try_recv() {
                let request = match &event {
                    SnapshotEvent::Snapshot { request, .. }
                    | SnapshotEvent::Error { request, .. } => *request,
                };
                if !self.cancelled.contains(&request) {
                    return Some(event);
                }
            }
            None
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
    #[cfg(all(feature = "threaded", feature = "ofd"))]
    use super::{PipelineEvent, PipelineRequest, PipelineService};
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

    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    #[test]
    fn render_service_wgpu_produces_a_tile() {
        use super::{RenderBackend, RenderEvent, RenderRequest, RenderService};
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new_with_backend(RenderBackend::Wgpu);
        let id = service.request(RenderRequest {
            snapshot,
            width: 8,
            height: 8,
            dpi: 96.0,
        });
        let mut frame = None;
        for _ in 0..1000 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame {
                    request,
                    width,
                    height,
                    rgba,
                } = event
                {
                    assert_eq!(request, id);
                    assert_eq!((width, height), (8, 8));
                    frame = Some(rgba);
                }
            }
            if frame.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(frame.expect("wgpu frame").len(), 8 * 8 * 4);
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

    #[cfg(all(feature = "threaded", feature = "ofd"))]
    #[test]
    fn threaded_pipeline_renders_generated_ofd() {
        use ofdbox_folio_ir::{FixedDocument, Page, PageInfo, Scene, Size};
        use ofdbox_folio_ofd::{OfdWriteOptions, write_fixed};

        let mut fixed = FixedDocument::default();
        fixed.pages.push(Page::new(
            PageInfo::default(),
            Scene {
                size: Size::new(20.0, 20.0),
                ..Scene::default()
            },
        ));
        let bytes = write_fixed(&fixed, &OfdWriteOptions::default()).expect("write ofd");
        let path = std::env::temp_dir().join(format!(
            "ofdbox-pipeline-{}-{}.ofd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp");
        let mut pipeline = PipelineService::open_path(&path).expect("open pipeline");
        pipeline.request(PipelineRequest {
            page: 0,
            width: 16,
            height: 16,
            dpi: 96.0,
        });
        let mut frame = None;
        for _ in 0..400 {
            while let Some(event) = pipeline.try_recv() {
                if let PipelineEvent::Frame {
                    width,
                    height,
                    rgba,
                    ..
                } = event
                {
                    assert_eq!((width, height), (16, 16));
                    frame = Some(rgba);
                }
            }
            if frame.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = std::fs::remove_file(&path);
        assert_eq!(frame.expect("threaded frame").len(), 16 * 16 * 4);
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

/// Render backend used by [`RenderService`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderBackend {
    /// CPU renderer.
    #[default]
    Cpu,
    /// wgpu renderer.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    Wgpu,
}

enum Renderer {
    Cpu(ofdbox_folio_render::CpuRenderer),
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    Wgpu(ofdbox_folio_render::WgpuBackend),
}

impl Renderer {
    fn new(backend: RenderBackend) -> Result<Self, String> {
        match backend {
            RenderBackend::Cpu => Ok(Self::Cpu(ofdbox_folio_render::CpuRenderer::with_defaults())),
            #[cfg(any(feature = "gpu27", feature = "gpu30"))]
            RenderBackend::Wgpu => {
                let context = ofdbox_folio_render::WgpuContext::new_blocking()
                    .map_err(|error| error.to_string())?;
                let backend = ofdbox_folio_render::WgpuBackend::new(context)
                    .map_err(|error| error.to_string())?;
                Ok(Self::Wgpu(backend))
            }
        }
    }

    fn render(&mut self, request: &RenderRequest) -> Result<Vec<u8>, String> {
        match self {
            Self::Cpu(renderer) => {
                let mut target = ofdbox_folio_render::RasterTarget::from_pixels(
                    request.width,
                    request.height,
                    request.dpi,
                )
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
            #[cfg(any(feature = "gpu27", feature = "gpu30"))]
            Self::Wgpu(backend) => {
                let target = ofdbox_folio_render::WgpuTarget::from_pixels(
                    backend.context(),
                    request.width,
                    request.height,
                    request.dpi,
                )
                .map_err(|error| error.to_string())?;
                backend
                    .render_plan_region(Arc::clone(&request.snapshot.plan), &target, (0.0, 0.0))
                    .map_err(|error| error.to_string())?;
                target
                    .read_rgba(backend.context())
                    .map_err(|error| error.to_string())
            }
        }
    }
}

#[cfg(not(feature = "threaded-render"))]
/// Compile-time inline render service.
pub struct RenderService {
    renderer: Renderer,
    events: VecDeque<RenderEvent>,
    cancelled: std::collections::HashSet<u64>,
    next: u64,
}

#[cfg(not(feature = "threaded-render"))]
impl RenderService {
    /// Creates an inline CPU render service.
    pub fn new() -> Self {
        Self::new_with_backend(RenderBackend::Cpu)
    }

    /// Creates an inline render service for the selected backend.
    pub fn new_with_backend(backend: RenderBackend) -> Self {
        Self {
            renderer: Renderer::new(backend).expect("render backend"),
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
        match self.renderer.render(&request) {
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
        /// Starts a worker with the selected backend.
        pub fn new_with_backend(backend: RenderBackend) -> Self {
            let (command_tx, command_rx) = mpsc::channel::<Command>();
            let (event_tx, event_rx) = mpsc::channel::<RenderEvent>();
            let worker = thread::Builder::new()
                .name("ofdbox-folio-render".to_owned())
                .spawn(move || {
                    let mut renderer = Renderer::new(backend).expect("render backend");
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
                                let event = match renderer.render(&request) {
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

        /// Creates a threaded CPU render service.
        pub fn new() -> Self {
            Self::new_with_backend(RenderBackend::Cpu)
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

/// A page-load and render request for [`PipelineService`].
#[derive(Clone, Debug)]
pub struct PipelineRequest {
    /// Page index.
    pub page: usize,
    /// Tile width in pixels.
    pub width: u32,
    /// Tile height in pixels.
    pub height: u32,
    /// Render DPI.
    pub dpi: f64,
}

/// One event emitted by [`PipelineService`].
#[derive(Debug)]
pub enum PipelineEvent {
    /// The snapshot is ready and a render was queued.
    Page {
        /// Pipeline request identifier.
        request: u64,
        /// Immutable page snapshot.
        snapshot: Arc<PageSnapshot>,
    },
    /// Rendering started.
    Started {
        /// Pipeline request identifier.
        request: u64,
    },
    /// Rendered RGBA tile.
    Frame {
        /// Pipeline request identifier.
        request: u64,
        /// Tile width.
        width: u32,
        /// Tile height in pixels.
        height: u32,
        /// Straight-alpha RGBA pixels.
        rgba: Vec<u8>,
    },
    /// Page or render failure.
    Error {
        /// Pipeline request identifier.
        request: u64,
        /// Error message.
        message: String,
    },
}

struct PendingPipeline {
    request: PipelineRequest,
    snapshot_id: Option<u64>,
    render_id: Option<u64>,
}

/// Parse and render pipeline selected by the same compile-time features.
pub struct PipelineService {
    documents: SnapshotService,
    renderer: RenderService,
    pending: std::collections::HashMap<u64, PendingPipeline>,
    by_snapshot: std::collections::HashMap<u64, u64>,
    by_render: std::collections::HashMap<u64, u64>,
    events: VecDeque<PipelineEvent>,
    next: u64,
}

impl PipelineService {
    /// Opens a document and uses the CPU renderer.
    pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        Self::open_path_with_backend(path, RenderBackend::Cpu)
    }

    /// Opens a document with the selected render backend.
    pub fn open_path_with_backend(
        path: impl AsRef<std::path::Path>,
        backend: RenderBackend,
    ) -> Result<Self, Error> {
        Self::open_path_with_factory_and_backend(path, Arc::new(SystemFontsFactory), backend)
    }

    /// Opens a document with a custom factory and render backend.
    pub fn open_path_with_factory_and_backend(
        path: impl AsRef<std::path::Path>,
        factory: Arc<dyn FontProviderFactory>,
        backend: RenderBackend,
    ) -> Result<Self, Error> {
        Ok(Self {
            documents: SnapshotService::open_path_with_factory(path, factory)?,
            renderer: RenderService::new_with_backend(backend),
            pending: std::collections::HashMap::new(),
            by_snapshot: std::collections::HashMap::new(),
            by_render: std::collections::HashMap::new(),
            events: VecDeque::new(),
            next: 1,
        })
    }

    /// Queues a page load followed by a render.
    pub fn request(&mut self, request: PipelineRequest) -> u64 {
        let id = self.next;
        self.next += 1;
        let snapshot_id = self.documents.request(request.page);
        self.by_snapshot.insert(snapshot_id, id);
        self.pending.insert(
            id,
            PendingPipeline {
                request,
                snapshot_id: Some(snapshot_id),
                render_id: None,
            },
        );
        id
    }

    /// Cancels a pipeline request.
    pub fn cancel(&mut self, request: u64) {
        if let Some(pending) = self.pending.remove(&request) {
            if let Some(snapshot) = pending.snapshot_id {
                self.documents.cancel(snapshot);
                self.by_snapshot.remove(&snapshot);
            }
            if let Some(render) = pending.render_id {
                self.renderer.cancel(render);
                self.by_render.remove(&render);
            }
        }
    }

    /// Advances the pipeline and pops the next event.
    pub fn try_recv(&mut self) -> Option<PipelineEvent> {
        while let Some(event) = self.documents.try_recv() {
            match event {
                SnapshotEvent::Snapshot { request, snapshot } => {
                    let Some(pipeline) = self.by_snapshot.remove(&request) else {
                        continue;
                    };
                    let Some(pending) = self.pending.get_mut(&pipeline) else {
                        continue;
                    };
                    pending.snapshot_id = None;
                    let render_id = self.renderer.request(RenderRequest {
                        snapshot: Arc::clone(&snapshot),
                        width: pending.request.width,
                        height: pending.request.height,
                        dpi: pending.request.dpi,
                    });
                    self.by_render.insert(render_id, pipeline);
                    if let Some(pending) = self.pending.get_mut(&pipeline) {
                        pending.render_id = Some(render_id);
                    }
                    self.events.push_back(PipelineEvent::Page {
                        request: pipeline,
                        snapshot,
                    });
                }
                SnapshotEvent::Error { request, message } => {
                    let Some(pipeline) = self.by_snapshot.remove(&request) else {
                        continue;
                    };
                    self.pending.remove(&pipeline);
                    self.events.push_back(PipelineEvent::Error {
                        request: pipeline,
                        message,
                    });
                }
            }
        }
        while let Some(event) = self.renderer.try_recv() {
            let pipeline = match &event {
                RenderEvent::Started { request }
                | RenderEvent::Frame { request, .. }
                | RenderEvent::Error { request, .. } => self.by_render.get(request).copied(),
            };
            let Some(pipeline) = pipeline else {
                continue;
            };
            match event {
                RenderEvent::Started { .. } => {
                    self.events
                        .push_back(PipelineEvent::Started { request: pipeline });
                }
                RenderEvent::Frame {
                    width,
                    height,
                    rgba,
                    ..
                } => {
                    self.pending.remove(&pipeline);
                    self.events.push_back(PipelineEvent::Frame {
                        request: pipeline,
                        width,
                        height,
                        rgba,
                    });
                }
                RenderEvent::Error { message, .. } => {
                    self.pending.remove(&pipeline);
                    self.events.push_back(PipelineEvent::Error {
                        request: pipeline,
                        message,
                    });
                }
            }
        }
        self.events.pop_front()
    }
}
