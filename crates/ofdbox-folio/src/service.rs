//! Compile-time selectable page-snapshot service.
//!
//! Without `threaded`, requests are served on the caller thread. With
//! `threaded`, a worker owns the `Document` and only `PageSnapshot` values cross
//! the thread boundary. `Document` itself never needs to be `Send`.

use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(feature = "threaded")]
use ofdbox_folio_ir::PageError;
use ofdbox_folio_render::{CacheBudget, PageSnapshot};

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
    #[cfg(feature = "ofd")]
    use super::PipelineEvent;
    #[cfg(all(any(feature = "gpu27", feature = "gpu30"), feature = "ofd"))]
    use super::SystemFontsFactory;
    #[cfg(feature = "ofd")]
    use super::{PipelineFitRequest, PipelineOptions, PipelineRequest, PipelineService};
    use super::{RenderBackend, RenderEvent, RenderRequest, RenderService, SnapshotService};
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
    fn backend_init_failure_is_an_error() {
        let result = RenderService::new_with_backend(RenderBackend::Failing);
        assert!(result.is_err());
    }

    #[cfg(feature = "threaded-render")]
    #[test]
    fn backend_init_failure_is_reported_from_worker() {
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service =
            RenderService::new_with_backend(RenderBackend::Failing).expect("worker starts");
        let id = service.request(RenderRequest {
            snapshot,
            width: 4,
            height: 4,
            dpi: 96.0,
            origin_px: (0.0, 0.0),
        });
        let mut message = None;
        for _ in 0..200 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Error {
                    request,
                    message: text,
                } = event
                {
                    assert_eq!(request, id);
                    message = Some(text);
                }
            }
            if message.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(message.expect("worker error").contains("intentional"));
    }

    #[test]
    fn render_service_reports_tile_origin() {
        let scene = Scene {
            size: Size::new(20.0, 20.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new();
        let id = service.request(RenderRequest {
            snapshot,
            width: 8,
            height: 8,
            dpi: 96.0,
            origin_px: (8.0, 4.0),
        });
        let mut origin = None;
        for _ in 0..200 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame {
                    request, origin_px, ..
                } = event
                {
                    assert_eq!(request, id);
                    origin = Some(origin_px);
                }
            }
            if origin.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(origin, Some((8.0, 4.0)));
    }

    #[test]
    fn tile_origin_changes_rendered_content() {
        use ofdbox_folio_ir::{
            Element, FillRule, Group, Matrix, Paint, Path as IrPath, PathElement, Point, Rgba,
        };

        let mut path = IrPath::default();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(2.0, 0.0));
        path.line_to(Point::new(2.0, 2.0));
        path.line_to(Point::new(0.0, 2.0));
        path.close();
        let mut root = Group::default();
        root.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::new(255, 0, 0, 255))),
            stroke: None,
            rule: FillRule::NonZero,
        }));
        let scene = Scene {
            size: Size::new(20.0, 20.0),
            root,
            diagnostics: Vec::new(),
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new();
        service.request(RenderRequest {
            snapshot: Arc::clone(&snapshot),
            width: 8,
            height: 8,
            dpi: 96.0,
            origin_px: (0.0, 0.0),
        });
        service.request(RenderRequest {
            snapshot: Arc::clone(&snapshot),
            width: 8,
            height: 8,
            dpi: 96.0,
            origin_px: (8.0, 0.0),
        });

        let mut first = None;
        let mut second = None;
        for _ in 0..200 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame {
                    origin_px, rgba, ..
                } = event
                {
                    if origin_px == (0.0, 0.0) {
                        first = Some(rgba);
                    } else {
                        second = Some(rgba);
                    }
                }
            }
            if first.is_some() && second.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let first = first.expect("origin tile");
        let second = second.expect("offset tile");
        assert!(first.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
        assert_ne!(first, second);
    }

    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    #[test]
    fn render_service_can_render_into_a_target_without_readback() {
        let context =
            Arc::new(ofdbox_folio_render::WgpuContext::new_blocking().expect("shared context"));
        let target = Arc::new(
            ofdbox_folio_render::WgpuTarget::from_pixels(&context, 4, 4, 96.0).expect("target"),
        );
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new_with_shared_context(RenderBackend::Wgpu, context)
            .expect("shared render service");
        let id = service.request_to_target(
            RenderRequest {
                snapshot,
                width: 4,
                height: 4,
                dpi: 96.0,
                origin_px: (0.0, 0.0),
            },
            Arc::clone(&target),
        );
        let mut rendered = None;
        for _ in 0..1000 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Target {
                    request,
                    target: value,
                    ..
                } = event
                {
                    assert_eq!(request, id);
                    rendered = Some(value);
                }
            }
            if rendered.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(Arc::ptr_eq(&target, &rendered.expect("target frame")));
    }

    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    #[test]
    fn render_service_reuses_shared_wgpu_context() {
        let context =
            Arc::new(ofdbox_folio_render::WgpuContext::new_blocking().expect("shared context"));
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            ..Scene::default()
        };
        let snapshot = Arc::new(PageSnapshot::from_page(Page::new(
            PageInfo::default(),
            scene,
        )));
        let mut service = RenderService::new_with_shared_context(RenderBackend::Wgpu, context)
            .expect("shared render service");
        let id = service.request(RenderRequest {
            snapshot,
            width: 4,
            height: 4,
            dpi: 96.0,
            origin_px: (0.0, 0.0),
        });
        let mut frame = None;
        for _ in 0..1000 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame { request, rgba, .. } = event {
                    assert_eq!(request, id);
                    frame = Some(rgba);
                }
            }
            if frame.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(frame.expect("shared context frame").len(), 4 * 4 * 4);
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
        let mut service =
            RenderService::new_with_backend(RenderBackend::Wgpu).expect("wgpu render service");
        let id = service.request(RenderRequest {
            snapshot,
            width: 8,
            height: 8,
            dpi: 96.0,
            origin_px: (8.0, 0.0),
        });
        let mut frame = None;
        for _ in 0..1000 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame {
                    request,
                    width,
                    height,
                    origin_px,
                    rgba,
                } = event
                {
                    assert_eq!(request, id);
                    assert_eq!((width, height), (8, 8));
                    assert_eq!(origin_px, (8.0, 0.0));
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
            origin_px: (0.0, 0.0),
        });
        service.cancel(id);
        assert!(service.try_recv().is_none());
    }

    #[cfg(feature = "ofd")]
    #[test]
    fn fit_request_centers_the_page() {
        use ofdbox_folio_ir::{FixedDocument, Page, PageInfo, Scene, Size};
        use ofdbox_folio_ofd::{OfdWriteOptions, write_fixed};

        let mut fixed = FixedDocument::default();
        fixed.pages.push(Page::new(
            PageInfo::default(),
            Scene {
                size: Size::new(10.0, 10.0),
                ..Scene::default()
            },
        ));
        let bytes = write_fixed(&fixed, &OfdWriteOptions::default()).expect("write ofd");
        let path = std::env::temp_dir().join(format!(
            "ofdbox-fit-{}-{}.ofd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp");
        let mut pipeline = PipelineService::open_path(&path).expect("pipeline");
        pipeline
            .request_fit(
                PipelineFitRequest {
                    page: 0,
                    width: 20,
                    height: 40,
                },
                PipelineOptions::default(),
            )
            .expect("fit request");
        let mut frame = None;
        for _ in 0..400 {
            while let Some(event) = pipeline.try_recv() {
                if let PipelineEvent::Frame {
                    width,
                    height,
                    origin_px,
                    ..
                } = event
                {
                    frame = Some((width, height, origin_px));
                }
            }
            if frame.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = std::fs::remove_file(&path);
        let (width, height, origin) = frame.expect("fit frame");
        assert_eq!((width, height), (20, 40));
        assert!((origin.1 - 10.0).abs() < 0.01);
    }

    #[cfg(feature = "ofd")]
    #[test]
    fn cancel_before_drops_older_generation() {
        use super::{PipelineOptions, PipelinePriority};
        use ofdbox_folio_ir::{FixedDocument, Page, PageInfo, Scene, Size};
        use ofdbox_folio_ofd::{OfdWriteOptions, write_fixed};

        let mut fixed = FixedDocument::default();
        fixed.pages.push(Page::new(
            PageInfo::default(),
            Scene {
                size: Size::new(10.0, 10.0),
                ..Scene::default()
            },
        ));
        let bytes = write_fixed(&fixed, &OfdWriteOptions::default()).expect("write ofd");
        let path = std::env::temp_dir().join(format!(
            "ofdbox-cancel-{}-{}.ofd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp");
        let mut pipeline = PipelineService::open_path(&path).expect("pipeline");
        pipeline
            .request_with_options(
                PipelineRequest {
                    page: 0,
                    width: 8,
                    height: 8,
                    dpi: 96.0,
                    origin_px: (0.0, 0.0),
                },
                PipelineOptions {
                    priority: PipelinePriority::Low,
                    generation: 1,
                },
            )
            .expect("request");
        pipeline.cancel_before(2);
        assert!(pipeline.try_recv().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(all(any(feature = "gpu27", feature = "gpu30"), feature = "ofd"))]
    #[test]
    fn pipeline_renders_into_external_target() {
        use super::{PipelineEvent, PipelineOptions};
        use ofdbox_folio_ir::{FixedDocument, Page, PageInfo, Scene, Size};
        use ofdbox_folio_ofd::{OfdWriteOptions, write_fixed};

        let mut fixed = FixedDocument::default();
        fixed.pages.push(Page::new(
            PageInfo::default(),
            Scene {
                size: Size::new(10.0, 10.0),
                ..Scene::default()
            },
        ));
        let bytes = write_fixed(&fixed, &OfdWriteOptions::default()).expect("write ofd");
        let path = std::env::temp_dir().join(format!(
            "ofdbox-pipeline-target-{}-{}.ofd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp");
        let context = Arc::new(ofdbox_folio_render::WgpuContext::new_blocking().expect("context"));
        let target = Arc::new(
            ofdbox_folio_render::WgpuTarget::from_pixels(&context, 8, 8, 96.0).expect("target"),
        );
        let mut pipeline = PipelineService::open_path_with_shared_context(
            &path,
            Arc::new(SystemFontsFactory),
            RenderBackend::Wgpu,
            context,
        )
        .expect("pipeline");
        pipeline
            .request_to_target(
                PipelineRequest {
                    page: 0,
                    width: 8,
                    height: 8,
                    dpi: 96.0,
                    origin_px: (0.0, 0.0),
                },
                Arc::clone(&target),
                PipelineOptions::default(),
            )
            .expect("request");
        let mut rendered = None;
        for _ in 0..1000 {
            while let Some(event) = pipeline.try_recv() {
                if let PipelineEvent::Target { target: value, .. } = event {
                    rendered = Some(value);
                }
            }
            if rendered.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = std::fs::remove_file(&path);
        assert!(Arc::ptr_eq(&target, &rendered.expect("pipeline target")));
    }

    #[cfg(all(any(feature = "gpu27", feature = "gpu30"), feature = "ofd"))]
    #[test]
    fn pipeline_fits_into_external_target() {
        use super::{PipelineEvent, PipelineOptions};
        use ofdbox_folio_ir::{FixedDocument, Page, PageInfo, Scene, Size};
        use ofdbox_folio_ofd::{OfdWriteOptions, write_fixed};

        let mut fixed = FixedDocument::default();
        fixed.pages.push(Page::new(
            PageInfo::default(),
            Scene {
                size: Size::new(10.0, 10.0),
                ..Scene::default()
            },
        ));
        let bytes = write_fixed(&fixed, &OfdWriteOptions::default()).expect("write ofd");
        let path = std::env::temp_dir().join(format!(
            "ofdbox-fit-target-{}-{}.ofd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp");
        let context = Arc::new(ofdbox_folio_render::WgpuContext::new_blocking().expect("context"));
        let read_context = Arc::clone(&context);
        let target = Arc::new(
            ofdbox_folio_render::WgpuTarget::from_pixels(&context, 20, 40, 96.0)
                .expect("target")
                .with_transparent_background(),
        );
        let mut pipeline = PipelineService::open_path_with_shared_context(
            &path,
            Arc::new(SystemFontsFactory),
            RenderBackend::Wgpu,
            context,
        )
        .expect("pipeline");
        pipeline
            .request_fit_to_target(
                PipelineFitRequest {
                    page: 0,
                    width: 20,
                    height: 40,
                },
                Arc::clone(&target),
                PipelineOptions::default(),
            )
            .expect("fit target request");
        let mut rendered = None;
        for _ in 0..1000 {
            while let Some(event) = pipeline.try_recv() {
                if let PipelineEvent::Target {
                    target: value,
                    origin_px,
                    ..
                } = event
                {
                    rendered = Some((value, origin_px));
                }
            }
            if rendered.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = std::fs::remove_file(&path);
        let (value, origin_px) = rendered.expect("fit target");
        assert!(Arc::ptr_eq(&target, &value));
        assert_eq!((target.width(), target.height()), (20, 40));
        let pixels = target.read_rgba(&read_context).expect("read target");
        assert_eq!(pixels[3], 0);
        assert!((origin_px.1 - 10.0).abs() < 0.01);
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
        pipeline
            .request(PipelineRequest {
                page: 0,
                width: 16,
                height: 16,
                dpi: 96.0,
                origin_px: (0.0, 0.0),
            })
            .expect("request");
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
            origin_px: (0.0, 0.0),
        });
        let mut frame = None;
        for _ in 0..200 {
            while let Some(event) = service.try_recv() {
                if let RenderEvent::Frame {
                    request: id,
                    width,
                    height,
                    origin_px: _,
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
    /// Tile origin in whole-page device pixels.
    pub origin_px: (f64, f64),
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
        /// Tile origin in whole-page device pixels.
        origin_px: (f64, f64),
        /// Straight-alpha RGBA pixels.
        rgba: Vec<u8>,
    },
    /// A caller-provided wgpu target finished rendering.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    Target {
        /// Request identifier.
        request: u64,
        /// Tile width.
        width: u32,
        /// Tile height.
        height: u32,
        /// Tile origin in whole-page device pixels.
        origin_px: (f64, f64),
        /// Render DPI.
        dpi: f64,
        /// Rendered target.
        target: Arc<ofdbox_folio_render::WgpuTarget>,
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
    /// Test-only backend that always fails to initialize.
    #[cfg(test)]
    Failing,
}

enum Renderer {
    Cpu(ofdbox_folio_render::CpuRenderer),
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    Wgpu(ofdbox_folio_render::WgpuBackend),
}

fn cpu_renderer(budget: Option<CacheBudget>) -> ofdbox_folio_render::CpuRenderer {
    match budget {
        Some(budget) => ofdbox_folio_render::CpuRenderer::with_shared_budget(
            ofdbox_folio_render::RenderOptions::default(),
            budget,
        ),
        None => ofdbox_folio_render::CpuRenderer::with_defaults(),
    }
}

impl Renderer {
    fn new(backend: RenderBackend, budget: Option<CacheBudget>) -> Result<Self, String> {
        match backend {
            RenderBackend::Cpu => Ok(Self::Cpu(cpu_renderer(budget))),
            #[cfg(test)]
            RenderBackend::Failing => Err("intentional backend failure".to_owned()),
            #[cfg(any(feature = "gpu27", feature = "gpu30"))]
            RenderBackend::Wgpu => {
                let context = ofdbox_folio_render::WgpuContext::new_blocking()
                    .map_err(|error| error.to_string())?;
                let backend =
                    ofdbox_folio_render::WgpuBackend::from_shared_with_options_and_budget(
                        Arc::new(context),
                        ofdbox_folio_render::WgpuOptions::default(),
                        budget,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(Self::Wgpu(backend))
            }
        }
    }

    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    fn new_shared(
        backend: RenderBackend,
        context: Arc<ofdbox_folio_render::WgpuContext>,
        budget: Option<CacheBudget>,
    ) -> Result<Self, String> {
        match backend {
            RenderBackend::Cpu => Ok(Self::Cpu(cpu_renderer(budget))),
            RenderBackend::Wgpu => {
                let backend =
                    ofdbox_folio_render::WgpuBackend::from_shared_with_options_and_budget(
                        context,
                        ofdbox_folio_render::WgpuOptions::default(),
                        budget,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(Self::Wgpu(backend))
            }
            #[cfg(test)]
            RenderBackend::Failing => Err("intentional backend failure".to_owned()),
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
                        ofdbox_folio_render::RenderRegion::new(request.origin_px, request.dpi),
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
                    .render_plan_region(
                        Arc::clone(&request.snapshot.plan),
                        &target,
                        request.origin_px,
                    )
                    .map_err(|error| error.to_string())?;
                target
                    .read_rgba(backend.context())
                    .map_err(|error| error.to_string())
            }
        }
    }

    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    fn render_to_target(
        &mut self,
        request: &RenderRequest,
        target: &Arc<ofdbox_folio_render::WgpuTarget>,
    ) -> Result<(), String> {
        match self {
            Self::Wgpu(backend) => {
                if let Some(message) = backend.take_device_lost() {
                    return Err(format!("wgpu device lost: {message}"));
                }
                backend
                    .render_plan_region(
                        Arc::clone(&request.snapshot.plan),
                        target,
                        request.origin_px,
                    )
                    .map_err(|error| error.to_string())?;
                if let Some(message) = backend.take_device_lost() {
                    return Err(format!("wgpu device lost: {message}"));
                }
                Ok(())
            }
            Self::Cpu(_) => Err("CPU renderer cannot render into a wgpu target".to_owned()),
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
        Self::new_with_backend(RenderBackend::Cpu).expect("CPU renderer")
    }

    /// Creates an inline render service for the selected backend.
    pub fn new_with_backend(backend: RenderBackend) -> Result<Self, Error> {
        Self::new_with_backend_and_budget(backend, None)
    }

    /// Creates an inline render service whose decoded-image cache shares
    /// `budget` with other backends.
    pub fn new_with_backend_and_budget(
        backend: RenderBackend,
        budget: Option<CacheBudget>,
    ) -> Result<Self, Error> {
        Ok(Self {
            renderer: Renderer::new(backend, budget).map_err(Error::Service)?,
            events: VecDeque::new(),
            cancelled: std::collections::HashSet::new(),
            next: 1,
        })
    }

    /// Creates an inline render service from an existing wgpu context.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    pub fn new_with_shared_context(
        backend: RenderBackend,
        context: Arc<ofdbox_folio_render::WgpuContext>,
    ) -> Result<Self, Error> {
        Self::new_with_shared_context_and_budget(backend, context, None)
    }

    /// Creates an inline render service from an existing wgpu context whose
    /// decoded-image cache shares `budget` with other backends.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    pub fn new_with_shared_context_and_budget(
        backend: RenderBackend,
        context: Arc<ofdbox_folio_render::WgpuContext>,
        budget: Option<CacheBudget>,
    ) -> Result<Self, Error> {
        Ok(Self {
            renderer: Renderer::new_shared(backend, context, budget).map_err(Error::Service)?,
            events: VecDeque::new(),
            cancelled: std::collections::HashSet::new(),
            next: 1,
        })
    }

    /// Renders into a caller-provided wgpu target without CPU readback.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    pub fn request_to_target(
        &mut self,
        request: RenderRequest,
        target: Arc<ofdbox_folio_render::WgpuTarget>,
    ) -> u64 {
        let id = self.next;
        self.next += 1;
        self.events.push_back(RenderEvent::Started { request: id });
        match self.renderer.render_to_target(&request, &target) {
            Ok(()) => self.events.push_back(RenderEvent::Target {
                request: id,
                width: request.width,
                height: request.height,
                origin_px: request.origin_px,
                dpi: request.dpi,
                target,
            }),
            Err(message) => self.events.push_back(RenderEvent::Error {
                request: id,
                message,
            }),
        }
        id
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
                origin_px: request.origin_px,
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
                #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                RenderEvent::Target { request, .. } => *request,
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

    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    type SharedContext = Arc<ofdbox_folio_render::WgpuContext>;
    #[cfg(not(any(feature = "gpu27", feature = "gpu30")))]
    type SharedContext = ();
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    type TargetHandle = Arc<ofdbox_folio_render::WgpuTarget>;
    #[cfg(not(any(feature = "gpu27", feature = "gpu30")))]
    type TargetHandle = ();

    enum Command {
        Render {
            id: u64,
            request: RenderRequest,
            target: Option<TargetHandle>,
        },
        Cancel {
            id: u64,
        },
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
        pub fn new_with_backend(backend: RenderBackend) -> Result<Self, Error> {
            Self::new_with_backend_and_budget(backend, None)
        }

        /// Starts a worker with the selected backend whose decoded-image cache
        /// shares `budget` with other backends.
        pub fn new_with_backend_and_budget(
            backend: RenderBackend,
            budget: Option<CacheBudget>,
        ) -> Result<Self, Error> {
            Self::new_with_context(backend, None, budget)
        }

        /// Starts a worker from an existing wgpu context.
        #[cfg(any(feature = "gpu27", feature = "gpu30"))]
        pub fn new_with_shared_context(
            backend: RenderBackend,
            context: SharedContext,
        ) -> Result<Self, Error> {
            Self::new_with_shared_context_and_budget(backend, context, None)
        }

        /// Starts a worker from an existing wgpu context whose decoded-image
        /// cache shares `budget` with other backends.
        #[cfg(any(feature = "gpu27", feature = "gpu30"))]
        pub fn new_with_shared_context_and_budget(
            backend: RenderBackend,
            context: SharedContext,
            budget: Option<CacheBudget>,
        ) -> Result<Self, Error> {
            Self::new_with_context(backend, Some(context), budget)
        }

        fn new_with_context(
            backend: RenderBackend,
            shared: Option<SharedContext>,
            budget: Option<CacheBudget>,
        ) -> Result<Self, Error> {
            let (command_tx, command_rx) = mpsc::channel::<Command>();
            let (event_tx, event_rx) = mpsc::channel::<RenderEvent>();
            let worker = thread::Builder::new()
                .name("ofdbox-folio-render".to_owned())
                .spawn(move || {
                    let mut renderer: Option<Renderer> = None;
                    let mut init_error: Option<String> = None;
                    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                    if let Some(context) = &shared {
                        match Renderer::new_shared(backend, Arc::clone(context), budget.clone()) {
                            Ok(value) => renderer = Some(value),
                            Err(message) => init_error = Some(message),
                        }
                    }
                    let _ = &shared;
                    let mut cancelled = HashSet::new();
                    while let Ok(command) = command_rx.recv() {
                        match command {
                            Command::Cancel { id } => {
                                cancelled.insert(id);
                            }
                            Command::Render {
                                id,
                                request,
                                target,
                            } => {
                                if cancelled.contains(&id) {
                                    continue;
                                }
                                if let Some(message) = &init_error {
                                    let _ = event_tx.send(RenderEvent::Error {
                                        request: id,
                                        message: message.clone(),
                                    });
                                    continue;
                                }
                                if renderer.is_none() {
                                    match Renderer::new(backend, budget.clone()) {
                                        Ok(value) => renderer = Some(value),
                                        Err(message) => {
                                            init_error = Some(message.clone());
                                            let _ = event_tx.send(RenderEvent::Error {
                                                request: id,
                                                message,
                                            });
                                            continue;
                                        }
                                    }
                                }
                                let renderer = renderer.as_mut().expect("renderer initialized");
                                if event_tx.send(RenderEvent::Started { request: id }).is_err() {
                                    break;
                                }
                                #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                                if let Some(target) = target {
                                    let event = match renderer.render_to_target(&request, &target) {
                                        Ok(()) => RenderEvent::Target {
                                            request: id,
                                            width: request.width,
                                            height: request.height,
                                            origin_px: request.origin_px,
                                            dpi: request.dpi,
                                            target,
                                        },
                                        Err(message) => RenderEvent::Error {
                                            request: id,
                                            message,
                                        },
                                    };
                                    if event_tx.send(event).is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                let _ = &target;
                                let event = match renderer.render(&request) {
                                    Ok(rgba) => RenderEvent::Frame {
                                        request: id,
                                        width: request.width,
                                        height: request.height,
                                        origin_px: request.origin_px,
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
                .map_err(Error::Io)?;
            Ok(Self {
                tx: command_tx,
                rx: event_rx,
                next: AtomicU64::new(1),
                cancelled: HashSet::new(),
                worker: Some(worker),
            })
        }

        /// Creates a threaded CPU render service.
        pub fn new() -> Self {
            Self::new_with_backend(RenderBackend::Cpu).expect("CPU renderer")
        }

        /// Queues a render request.
        pub fn request(&mut self, request: RenderRequest) -> u64 {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let _ = self.tx.send(Command::Render {
                id,
                request,
                target: None,
            });
            id
        }

        /// Queues a render into a caller-provided wgpu target without CPU readback.
        #[cfg(any(feature = "gpu27", feature = "gpu30"))]
        pub fn request_to_target(&mut self, request: RenderRequest, target: TargetHandle) -> u64 {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let _ = self.tx.send(Command::Render {
                id,
                request,
                target: Some(target),
            });
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
                    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                    RenderEvent::Target { request, .. } => *request,
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
    /// Tile origin in whole-page device pixels.
    pub origin_px: (f64, f64),
}

/// A request that fits a whole page into a target bitmap.
#[derive(Clone, Copy, Debug)]
pub struct PipelineFitRequest {
    /// Page index.
    pub page: usize,
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
}

/// Priority for a pipeline request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PipelinePriority {
    /// Background prefetch.
    Low,
    /// Normal page load.
    #[default]
    Normal,
    /// Current page.
    High,
}

impl PipelinePriority {
    fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
        }
    }
}

/// Scheduling options for a pipeline request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineOptions {
    /// Request priority.
    pub priority: PipelinePriority,
    /// Caller generation; lower generations can be cancelled in bulk.
    pub generation: u64,
}

/// One event emitted by [`PipelineService`].
#[derive(Debug)]
pub enum PipelineEvent {
    /// The snapshot is ready and a render was queued.
    Page {
        /// Pipeline request identifier.
        request: u64,
        /// Caller generation.
        generation: u64,
        /// Immutable page snapshot.
        snapshot: Arc<PageSnapshot>,
    },
    /// Rendering started.
    Started {
        /// Pipeline request identifier.
        request: u64,
        /// Caller generation.
        generation: u64,
    },
    /// Rendered RGBA tile.
    Frame {
        /// Pipeline request identifier.
        request: u64,
        /// Caller generation.
        generation: u64,
        /// Tile width.
        width: u32,
        /// Tile height in pixels.
        height: u32,
        /// Tile origin in whole-page device pixels.
        origin_px: (f64, f64),
        /// Straight-alpha RGBA pixels.
        rgba: Vec<u8>,
    },
    /// A caller-provided wgpu target finished rendering.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    Target {
        /// Pipeline request identifier.
        request: u64,
        /// Caller generation.
        generation: u64,
        /// Tile width.
        width: u32,
        /// Tile height.
        height: u32,
        /// Tile origin in whole-page device pixels.
        origin_px: (f64, f64),
        /// Render DPI.
        dpi: f64,
        /// Rendered target.
        target: Arc<ofdbox_folio_render::WgpuTarget>,
    },
    /// Page or render failure.
    Error {
        /// Pipeline request identifier.
        request: u64,
        /// Caller generation.
        generation: u64,
        /// Error message.
        message: String,
    },
}

struct PendingPipeline {
    request: PipelineRequest,
    options: PipelineOptions,
    seq: u64,
    fit: Option<(u32, u32)>,
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    target: Option<Arc<ofdbox_folio_render::WgpuTarget>>,
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
    queued: Vec<u64>,
    active_snapshots: usize,
    max_in_flight: usize,
    max_pending: usize,
    floor_generation: u64,
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

    /// Opens a document using an existing wgpu context.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    pub fn open_path_with_shared_context(
        path: impl AsRef<std::path::Path>,
        factory: Arc<dyn FontProviderFactory>,
        backend: RenderBackend,
        context: Arc<ofdbox_folio_render::WgpuContext>,
    ) -> Result<Self, Error> {
        Ok(Self {
            documents: SnapshotService::open_path_with_factory(path, factory)?,
            renderer: RenderService::new_with_shared_context(backend, context)?,
            pending: std::collections::HashMap::new(),
            by_snapshot: std::collections::HashMap::new(),
            by_render: std::collections::HashMap::new(),
            queued: Vec::new(),
            active_snapshots: 0,
            max_in_flight: 2,
            max_pending: 32,
            floor_generation: 0,
            events: VecDeque::new(),
            next: 1,
        })
    }

    /// Opens a document with a custom factory and render backend.
    pub fn open_path_with_factory_and_backend(
        path: impl AsRef<std::path::Path>,
        factory: Arc<dyn FontProviderFactory>,
        backend: RenderBackend,
    ) -> Result<Self, Error> {
        Ok(Self {
            documents: SnapshotService::open_path_with_factory(path, factory)?,
            renderer: RenderService::new_with_backend(backend)?,
            pending: std::collections::HashMap::new(),
            by_snapshot: std::collections::HashMap::new(),
            by_render: std::collections::HashMap::new(),
            queued: Vec::new(),
            active_snapshots: 0,
            max_in_flight: 2,
            max_pending: 32,
            floor_generation: 0,
            events: VecDeque::new(),
            next: 1,
        })
    }

    /// Queues a page load followed by a render with default options.
    pub fn request(&mut self, request: PipelineRequest) -> Result<u64, Error> {
        self.request_with_options(request, PipelineOptions::default())
    }

    /// Queues a page load with explicit priority and generation.
    pub fn request_with_options(
        &mut self,
        request: PipelineRequest,
        options: PipelineOptions,
    ) -> Result<u64, Error> {
        if self.pending.len() >= self.max_pending {
            return Err(Error::Service("pipeline queue is full".to_owned()));
        }
        let id = self.next;
        self.next += 1;
        self.pending.insert(
            id,
            PendingPipeline {
                request,
                options,
                seq: id,
                fit: None,
                #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                target: None,
                snapshot_id: None,
                render_id: None,
            },
        );
        self.queued.push(id);
        self.pump();
        Ok(id)
    }

    /// Queues a fit-to-target render of the whole page.
    pub fn request_fit(
        &mut self,
        request: PipelineFitRequest,
        options: PipelineOptions,
    ) -> Result<u64, Error> {
        if request.width == 0 || request.height == 0 {
            return Err(Error::Service("fit target must be non-empty".to_owned()));
        }
        if self.pending.len() >= self.max_pending {
            return Err(Error::Service("pipeline queue is full".to_owned()));
        }
        let id = self.next;
        self.next += 1;
        self.pending.insert(
            id,
            PendingPipeline {
                request: PipelineRequest {
                    page: request.page,
                    width: request.width,
                    height: request.height,
                    dpi: 0.0,
                    origin_px: (0.0, 0.0),
                },
                options,
                seq: id,
                fit: Some((request.width, request.height)),
                #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                target: None,
                snapshot_id: None,
                render_id: None,
            },
        );
        self.queued.push(id);
        self.pump();
        Ok(id)
    }

    /// Queues a fit-to-target render into a caller-provided wgpu target.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    pub fn request_fit_to_target(
        &mut self,
        request: PipelineFitRequest,
        target: Arc<ofdbox_folio_render::WgpuTarget>,
        options: PipelineOptions,
    ) -> Result<u64, Error> {
        if request.width == 0 || request.height == 0 {
            return Err(Error::Service("fit target must be non-empty".to_owned()));
        }
        if self.pending.len() >= self.max_pending {
            return Err(Error::Service("pipeline queue is full".to_owned()));
        }
        let id = self.next;
        self.next += 1;
        self.pending.insert(
            id,
            PendingPipeline {
                request: PipelineRequest {
                    page: request.page,
                    width: request.width,
                    height: request.height,
                    dpi: 0.0,
                    origin_px: (0.0, 0.0),
                },
                options,
                seq: id,
                fit: Some((request.width, request.height)),
                target: Some(target),
                snapshot_id: None,
                render_id: None,
            },
        );
        self.queued.push(id);
        self.pump();
        Ok(id)
    }

    /// Queues a render into a caller-provided wgpu target without CPU readback.
    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
    pub fn request_to_target(
        &mut self,
        request: PipelineRequest,
        target: Arc<ofdbox_folio_render::WgpuTarget>,
        options: PipelineOptions,
    ) -> Result<u64, Error> {
        if self.pending.len() >= self.max_pending {
            return Err(Error::Service("pipeline queue is full".to_owned()));
        }
        let id = self.next;
        self.next += 1;
        self.pending.insert(
            id,
            PendingPipeline {
                request,
                options,
                seq: id,
                fit: None,
                target: Some(target),
                snapshot_id: None,
                render_id: None,
            },
        );
        self.queued.push(id);
        self.pump();
        Ok(id)
    }

    /// Cancels all requests with a generation lower than `generation`.
    pub fn cancel_before(&mut self, generation: u64) {
        self.floor_generation = self.floor_generation.max(generation);
        let stale: Vec<u64> = self
            .pending
            .iter()
            .filter_map(|(id, pending)| (pending.options.generation < generation).then_some(*id))
            .collect();
        for id in stale {
            self.cancel(id);
        }
    }

    /// Cancels a pipeline request.
    pub fn cancel(&mut self, request: u64) {
        if let Some(pending) = self.pending.remove(&request) {
            self.queued.retain(|value| *value != request);
            if let Some(snapshot) = pending.snapshot_id {
                self.documents.cancel(snapshot);
                self.by_snapshot.remove(&snapshot);
                self.active_snapshots = self.active_snapshots.saturating_sub(1);
            }
            if let Some(render) = pending.render_id {
                self.renderer.cancel(render);
                self.by_render.remove(&render);
            }
        }
    }

    fn pump(&mut self) {
        while self.active_snapshots < self.max_in_flight {
            let next = self
                .queued
                .iter()
                .enumerate()
                .max_by_key(|(_, id)| {
                    self.pending.get(id).map_or((0, 0), |pending| {
                        (pending.options.priority.rank(), pending.seq)
                    })
                })
                .map(|(index, _)| index);
            let Some(index) = next else {
                break;
            };
            let id = self.queued.remove(index);
            let Some(pending) = self.pending.get_mut(&id) else {
                continue;
            };
            let snapshot = self.documents.request(pending.request.page);
            pending.snapshot_id = Some(snapshot);
            self.by_snapshot.insert(snapshot, id);
            self.active_snapshots += 1;
        }
    }

    /// Advances the pipeline and pops the next event.
    pub fn try_recv(&mut self) -> Option<PipelineEvent> {
        self.pump();
        while let Some(event) = self.documents.try_recv() {
            match event {
                SnapshotEvent::Snapshot { request, snapshot } => {
                    self.active_snapshots = self.active_snapshots.saturating_sub(1);
                    let Some(pipeline) = self.by_snapshot.remove(&request) else {
                        continue;
                    };
                    let Some(pending) = self.pending.get_mut(&pipeline) else {
                        continue;
                    };
                    pending.snapshot_id = None;
                    if pending.options.generation < self.floor_generation {
                        self.pending.remove(&pipeline);
                        continue;
                    }
                    let (dpi, origin_px) = if pending.fit.is_some() {
                        let page = snapshot.plan.size;
                        let dpi = (f64::from(pending.request.width) / page.width * 25.4)
                            .min(f64::from(pending.request.height) / page.height * 25.4)
                            .max(0.01);
                        let page_width = page.width / 25.4 * dpi;
                        let page_height = page.height / 25.4 * dpi;
                        (
                            dpi,
                            (
                                (f64::from(pending.request.width) - page_width) * 0.5,
                                (f64::from(pending.request.height) - page_height) * 0.5,
                            ),
                        )
                    } else {
                        (pending.request.dpi, pending.request.origin_px)
                    };
                    let render_request = RenderRequest {
                        snapshot: Arc::clone(&snapshot),
                        width: pending.request.width,
                        height: pending.request.height,
                        dpi,
                        origin_px,
                    };
                    #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                    let render_id = if let Some(target) = pending.target.take() {
                        self.renderer.request_to_target(render_request, target)
                    } else {
                        self.renderer.request(render_request)
                    };
                    #[cfg(not(any(feature = "gpu27", feature = "gpu30")))]
                    let render_id = self.renderer.request(render_request);
                    self.by_render.insert(render_id, pipeline);
                    pending.render_id = Some(render_id);
                    self.events.push_back(PipelineEvent::Page {
                        request: pipeline,
                        generation: pending.options.generation,
                        snapshot,
                    });
                }
                SnapshotEvent::Error { request, message } => {
                    self.active_snapshots = self.active_snapshots.saturating_sub(1);
                    let Some(pipeline) = self.by_snapshot.remove(&request) else {
                        continue;
                    };
                    let Some(pending) = self.pending.remove(&pipeline) else {
                        continue;
                    };
                    self.events.push_back(PipelineEvent::Error {
                        request: pipeline,
                        generation: pending.options.generation,
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
                #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                RenderEvent::Target { request, .. } => self.by_render.get(request).copied(),
            };
            let Some(pipeline) = pipeline else {
                continue;
            };
            let Some((generation, _request_origin)) = self
                .pending
                .get(&pipeline)
                .map(|pending| (pending.options.generation, pending.request.origin_px))
            else {
                continue;
            };
            match event {
                RenderEvent::Started { .. } => {
                    self.events.push_back(PipelineEvent::Started {
                        request: pipeline,
                        generation,
                    });
                }
                RenderEvent::Frame {
                    width,
                    height,
                    origin_px,
                    rgba,
                    ..
                } => {
                    self.pending.remove(&pipeline);
                    self.events.push_back(PipelineEvent::Frame {
                        request: pipeline,
                        generation,
                        width,
                        height,
                        origin_px,
                        rgba,
                    });
                }
                RenderEvent::Error { message, .. } => {
                    self.pending.remove(&pipeline);
                    self.events.push_back(PipelineEvent::Error {
                        request: pipeline,
                        generation,
                        message,
                    });
                }
                #[cfg(any(feature = "gpu27", feature = "gpu30"))]
                RenderEvent::Target {
                    width,
                    height,
                    origin_px,
                    dpi,
                    target,
                    ..
                } => {
                    self.pending.remove(&pipeline);
                    self.events.push_back(PipelineEvent::Target {
                        request: pipeline,
                        generation,
                        width,
                        height,
                        origin_px,
                        dpi,
                        target,
                    });
                }
            }
        }
        self.events.pop_front()
    }
}
