//! OFDBox Folio 的单向排版引擎。
//!
//! 输入 [`ofdbox_folio_ir::FlowDocument`] 与字体提供者，输出
//! [`ofdbox_folio_ir::FixedDocument`]。布局是 Flow 到 Fixed 的唯一通路，
//! 不提供反向转换。
//!
//! 规划模块：`font`、`linebreak`、`pagination`、`table`、`toc`。
//! 当前阶段只建立 crate 边界，算法随后按里程碑实现。
