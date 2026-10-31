//! The one-way typesetting engine of OFDBox Folio.
//!
//! It takes [`ofdbox_folio_ir::FlowDocument`] and a font provider as input and
//! outputs [`ofdbox_folio_ir::FixedDocument`]. Layout is the only path from Flow
//! to Fixed; no reverse conversion is provided, and no rendering or file IO is
//! performed.
//!
//! The engine covers paragraphs (indent, line spacing, alignment and
//! justification), inline/block/floating images, tables, headers and footers,
//! footnotes and endnotes, automatic tables of contents, page breaks, page
//! number fields and heading outlines. The frozen rules live in
//! `crates/ofdbox-folio-layout/CONTRACT.md`.

mod engine;

pub use engine::{LAYOUT_VERSION, LayoutError, LayoutOptions, layout, layout_with};
