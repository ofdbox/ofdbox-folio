//! OFDBox Folio 的 Fixed IR 渲染层。
//!
//! 只消费 [`ofdbox_folio_ir::Page`] 等 Fixed IR 类型，不感知源文档格式。
//! 负责 CPU 光栅化、瓦片、字形图集、图片解码和渲染缓存。
//!
//! 规划模块：`cpu`、`tile`、`cache`、`text`、`image`。
//! 当前阶段只建立 crate 边界，渲染实现随后按里程碑补齐。
