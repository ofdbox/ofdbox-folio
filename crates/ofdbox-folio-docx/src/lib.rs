//! OFDBox Folio 的 DOCX 适配层。
//!
//! 负责把 DOCX 解析为 Flow IR，以及把 Flow IR 写出为 DOCX。本 crate 不接触
//! Fixed IR；DOCX 的版式接近 Word 但不承诺逐行一致。
//!
//! 当前阶段只建立 crate 边界，读写实现随后按里程碑补齐。
