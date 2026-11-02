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
    use super::SnapshotService;

    #[test]
    fn missing_path_fails_to_open() {
        let result = SnapshotService::open_path("Z:/definitely-missing-document.ofd");
        assert!(result.is_err());
    }
}
