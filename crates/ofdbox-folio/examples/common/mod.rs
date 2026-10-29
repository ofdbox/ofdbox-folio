//! System font provider shared by the examples.
//!
//! The implementation has been extracted into `ofdbox_folio::fonts`; the CLI and the
//! examples share the same "family name → font file" rules, so the same document lays
//! out into the same page count from every entry point.

pub use ofdbox_folio::fonts::SystemFonts;
