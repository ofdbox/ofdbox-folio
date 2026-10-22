//! OFDBox Folio 的 PDF 适配层。
//!
//! 负责把 PDF 页面解析为 Fixed IR，以及把 Fixed IR 写出为 PDF。
//! 对外不暴露 `lopdf` 的类型；PDF 阅读的字体、CMap、图案、透明与加密
//! 按路线图分期硬化。
//!
//! 当前阶段只建立 crate 边界，读写实现随后按里程碑补齐。
