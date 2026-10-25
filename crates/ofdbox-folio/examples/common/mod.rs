//! 示例共用的系统字体提供者。
//!
//! 实现已抽到 `ofdbox_folio::fonts`，CLI 与示例共用同一份「族名 → 字体文件」规则，
//! 保证同一份文档在不同入口排出同样的页数。

pub use ofdbox_folio::fonts::SystemFonts;
