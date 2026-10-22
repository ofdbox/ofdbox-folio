//! OFDBox Folio 多格式文档引擎门面。
//!
//! 门面统一格式探测、阅读器页面缓存、创建 API 与便捷转换，并重新导出
//! 内部 crate。具体接口按冻结的公开 API 契约逐步实现。
//!
//! - [`ir`]：Flow IR、Fixed IR 与阅读器契约；
//! - [`Document`]：带字节预算缓存的按需阅读器；
//! - `layout`：Flow 到 Fixed 的单向排版；
//! - `render`：Fixed IR 的位图渲染；
//! - `ofd` / `pdf` / `docx`：格式适配器。

mod error;
mod reader;

/// 统一门面错误。
pub use error::Error;
/// 跨页选择与搜索常用类型。
pub use ofdbox_folio_ir::{SearchHit, SearchQuery, Selection, TextPosition, TextRange};
/// 阅读器与缓存类型。
pub use reader::{CacheOptions, CacheStats, Document, OpenOptions, PageSelection, ReaderOptions};

/// Flow IR、Fixed IR 与阅读器契约。
pub use ofdbox_folio_ir as ir;

/// Flow IR 到 Fixed IR 的单向排版。
#[cfg(feature = "layout")]
pub use ofdbox_folio_layout as layout;

/// Fixed IR 的 CPU 渲染。
#[cfg(feature = "cpu")]
pub use ofdbox_folio_render as render;

/// OFD 与 Fixed IR 的读写适配。
#[cfg(feature = "ofd")]
pub use ofdbox_folio_ofd as ofd;

/// PDF 与 Fixed IR 的读写适配。
#[cfg(feature = "pdf")]
pub use ofdbox_folio_pdf as pdf;

/// DOCX 与 Flow IR 的读写适配。
#[cfg(feature = "docx")]
pub use ofdbox_folio_docx as docx;
