//! OFDBox Folio 的 DOCX 适配层。
//!
//! 负责 DOCX ↔ Flow IR 的双向适配：`read_flow` 把 DOCX 解析成 Flow IR，
//! `write_flow` 把 Flow IR 写成 DOCX 包。本 crate 不接触 Fixed IR，也不做排版
//! 与渲染；DOCX 的版式接近 Word 但不承诺逐行一致。
//!
//! 实现约定：
//!
//! - 样式按 OOXML 的层叠（文档默认 → 段落样式链 → 字符样式链 → 直接格式）
//!   逐字段解析，再**拍平**成直接格式写进 Flow IR。这样「显式写回默认值」也能
//!   生效；IR 的命名样式链只保留给代码构造的文档使用。
//! - 列表编号来自 `numbering.xml`，作为段落首个文本 run 的标记加悬挂缩进写出。
//! - 浮动图片降级为行内图片、合并单元格按普通单元格处理，都会在
//!   `FlowDocument.report` 里留诊断，不静默丢内容。
//! - 库保持静默：不打印、不写日志。

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

/// DOCX 读取选项。
#[derive(Clone, Debug)]
pub struct DocxReadOptions {
    /// 单个包部件解压后的字节上限。
    pub max_part_bytes: usize,
}

impl Default for DocxReadOptions {
    fn default() -> Self {
        Self {
            max_part_bytes: 64 * 1024 * 1024,
        }
    }
}

/// DOCX 读取错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DocxError {
    /// ZIP 容器不可读。
    #[error("DOCX 包无效：{0}")]
    Package(String),
    /// 缺少必需部件或结构不符合 OOXML。
    #[error("DOCX 结构无效：{0}")]
    Structure(String),
    /// 超过资源上限。
    #[error("DOCX 超出限制：{0}")]
    Limit(String),
    /// XML 语法错误。
    #[error("DOCX XML 解析失败：{0}")]
    Xml(String),
    /// 文件读写失败。
    #[error("DOCX 读写失败：{0}")]
    Io(String),
}

/// 字节是否像一个 DOCX（ZIP 且含 `word/document.xml`）。
pub fn probe(bytes: &[u8]) -> bool {
    let Ok(archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    archive.index_for_name("word/document.xml").is_some()
}

/// 从内存写出 DOCX 包。
pub use writer::{DocxWriteOptions, write_flow, write_flow_path, write_flow_with_report};

/// 从内存读取 DOCX，产出 Flow IR。
pub fn read_flow(bytes: &[u8], options: &DocxReadOptions) -> Result<FlowDocument, DocxError> {
    body::read(bytes, options)
}

/// 从文件读取 DOCX，产出 Flow IR。
pub fn read_flow_path(
    path: impl AsRef<Path>,
    options: &DocxReadOptions,
) -> Result<FlowDocument, DocxError> {
    let bytes = std::fs::read(path.as_ref()).map_err(|error| DocxError::Io(error.to_string()))?;
    read_flow(&bytes, options)
}
