//! OFDBox Folio 的公共 IR 与阅读器契约。
//!
//! 本 crate 定义 Flow IR、Fixed IR、几何与画笔、资源句柄、文本层和
//! [`DocumentSource`] 等格式中立类型，不依赖任何文档格式库，也不做 I/O。
//!
//! 规划模块：`flow`、`fixed`、`geometry`、`paint`、`path`、`asset`、
//! `text`、`source`、`diagnostic`。当前阶段只建立 crate 边界，类型实现
//! 随后按冻结的公开 API 契约补齐。
