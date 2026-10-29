//! DOCX adaptation layer for OFDBox Folio.
//!
//! Handles bidirectional DOCX ↔ Flow IR adaptation: `read_flow` parses DOCX into
//! Flow IR, while `write_flow` writes Flow IR into a DOCX package. This crate does not
//! touch Fixed IR and does no layout or rendering; DOCX layout approximates Word but
//! does not promise line-by-line fidelity.
//!
//! Implementation notes:
//!
//! - Styles are resolved field by field following the OOXML cascade (document
//!   defaults → paragraph style chain → character style chain → direct formatting) and
//!   then **flattened** into direct formatting in the Flow IR. This also makes an
//!   "explicitly written back default" take effect; the IR named style chain is kept
//!   only for documents constructed in code.
//! - List numbering comes from `numbering.xml` and is written out as a marker on the
//!   paragraph's first text run, together with a hanging indent.
//! - Floating images downgraded to inline images and merged cells treated as ordinary
//!   cells both leave a diagnostic in `FlowDocument.report`; content is never dropped
//!   silently.
//! - The library stays quiet: no printing, no logging.

mod body;
mod image;
mod numbering;
mod package;
mod props;
mod styles;
mod writer;
mod xml;

use std::io::Cursor;
use std::path::Path;

use ofdbox_folio_ir::FlowDocument;

/// DOCX read options.
#[derive(Clone, Debug)]
pub struct DocxReadOptions {
    /// Byte limit for a single decompressed package part.
    pub max_part_bytes: usize,
}

impl Default for DocxReadOptions {
    fn default() -> Self {
        Self {
            max_part_bytes: 64 * 1024 * 1024,
        }
    }
}

/// DOCX read error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DocxError {
    /// The ZIP container is unreadable.
    #[error("invalid DOCX package: {0}")]
    Package(String),
    /// A required part is missing or the structure does not match OOXML.
    #[error("invalid DOCX structure: {0}")]
    Structure(String),
    /// A resource limit was exceeded.
    #[error("DOCX limit exceeded: {0}")]
    Limit(String),
    /// XML syntax error.
    #[error("DOCX XML parse failed: {0}")]
    Xml(String),
    /// File I/O failed.
    #[error("DOCX I/O failed: {0}")]
    Io(String),
}

/// Whether the bytes look like a DOCX (a ZIP containing `word/document.xml`).
pub fn probe(bytes: &[u8]) -> bool {
    let Ok(archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    archive.index_for_name("word/document.xml").is_some()
}

/// Write a DOCX package to memory.
pub use writer::{DocxWriteOptions, write_flow, write_flow_path, write_flow_with_report};

/// Read a DOCX from memory and produce Flow IR.
pub fn read_flow(bytes: &[u8], options: &DocxReadOptions) -> Result<FlowDocument, DocxError> {
    body::read(bytes, options)
}

/// Read a DOCX from a file and produce Flow IR.
pub fn read_flow_path(
    path: impl AsRef<Path>,
    options: &DocxReadOptions,
) -> Result<FlowDocument, DocxError> {
    let bytes = std::fs::read(path.as_ref()).map_err(|error| DocxError::Io(error.to_string()))?;
    read_flow(&bytes, options)
}
