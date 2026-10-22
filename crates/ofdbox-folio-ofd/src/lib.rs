//! OFDBox Folio 的 OFD 适配层。
//!
//! 负责把 OFD 包解析为 Fixed IR 页面，以及把 Fixed IR 写出为 OFD 包。
//! 对外不暴露 `ofdbox-core` 的类型；结构解析、密码学与签章能力复用
//! `ofdbox-core`、`ofdbox-crypto`、`ofdbox-seal`。
//!
//! 当前阶段只建立 crate 边界，读写实现随后按里程碑补齐。
