//! OFDBox Folio multi-format document engine facade.
//!
//! The facade unifies format probing, reader page caching, the creation API and
//! convenience conversions, and re-exports the internal crates. Concrete interfaces are
//! implemented step by step against the frozen public API contract.
//!
//! - [`ir`]: Flow IR, Fixed IR and reader contracts;
//! - [`Document`]: on-demand reader with a byte-budget cache;
//! - `layout`: one-way layout from Flow to Fixed;
//! - `render`: bitmap rendering of Fixed IR;
//! - `ofd` / `pdf` / `docx`: format adapters.

mod error;
mod reader;

/// Unified facade error.
pub use error::Error;
/// Common types for cross-page selection and search.
pub use ofdbox_folio_ir::{SearchHit, SearchQuery, Selection, TextPosition, TextRange};
/// Reader and cache types.
pub use reader::{CacheOptions, CacheStats, Document, OpenOptions, PageSelection, ReaderOptions};

/// Flow IR, Fixed IR and reader contracts.
pub use ofdbox_folio_ir as ir;

/// System font resolution (family name → local font file), shared by the examples and the CLI.
pub mod fonts;

/// One-way layout from Flow IR to Fixed IR.
#[cfg(feature = "layout")]
pub use ofdbox_folio_layout as layout;

/// Bitmap rendering of Fixed IR.
#[cfg(any(feature = "cpu", feature = "gpu"))]
pub use ofdbox_folio_render as render;

/// Read/write adapter between OFD and Fixed IR.
#[cfg(feature = "ofd")]
pub use ofdbox_folio_ofd as ofd;

/// Read/write adapter between PDF and Fixed IR.
#[cfg(feature = "pdf")]
pub use ofdbox_folio_pdf as pdf;

/// Read/write adapter between DOCX and Flow IR.
#[cfg(feature = "docx")]
pub use ofdbox_folio_docx as docx;
