[English](README.md) | 简体中文

# ofdbox-folio-ofd

[`ofdbox-folio`](../../README.zh-CN.md) 的 OFD 读写适配器：把 OFD 包解析为 Fixed IR 页面，并把 Fixed IR 写回 OFD 1.0 包，两个方向共用同一套包模型。

## Public Entry Points

- `OfdDocument` 实现 `DocumentSource`：可用 `open_path()`、`open_path_with_fonts()`、`open_bytes()` 或 `open_bytes_with_fonts()` 打开，再用 `page_info()`、`load_page()`、`load_attachment()`、`font_names()`、`info()` 与 `resources()`。
- `write_fixed()` 按 `OfdWriteOptions`（`creator`、可选的固定 `doc_id`）写出 `FixedDocument`。
- `probe()` 在不解析页面的情况下判断是否为 OFD 包；`OfdReadOptions` 控制注解、不支持特性策略、图片上限、宽松解析与解密（`password`、`certificate_private_key`）。
- `LenientMode`、`OfdRepair` 与 `OfdDiagnostic` 报告读取期间执行的修复；`OfdError` 是适配器错误类型。

### Example

```rust
use ofdbox_folio::ir::FixedDocument;
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

let bytes = write_fixed(&document, &OfdWriteOptions::default())?;
```

## Coverage

写出器消费 Fixed IR，输出 OFD 1.0 包（`Version="1.0"`）。它与读取器一起位于 `ofdbox-folio-ofd`，两个方向共用同一套包模型。

### Package Coverage

路径、文本（含 `CGTransform` 与 `DeltaX/DeltaY`）、图片、组裁剪、透明度、轴向/径向渐变、字体名称与内嵌字体字节。

对象 `Boundary` 取整页，实际位置由 `CTM` 承载；行的位置差直接来自字形局部坐标，避免字体度量引入偏差。

### Fonts

内嵌字体按文档实际用字做 TrueType 子集化（保留字形 ID，只清空未用轮廓，复合字形依赖一并保留），字体不可裁剪时整份嵌入；默认使用固定 ZIP 时间戳和内容摘要 `DocID`，相同输入产生相同字节。

## Known Limitations

- 图案与网格填充尚未写出，会回退为首个色标。
- 组透明度分配到子元素颜色 alpha，不保留独立图层语义。

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — 本 crate 使用的 Fixed IR、`DocumentSource` 与资源契约。
- [ofdbox-folio-pdf](../ofdbox-folio-pdf) — 另一个定版适配器，同样从 Fixed IR 写出。
- [ofdbox-folio-render](../ofdbox-folio-render) — 渲染本 crate 读取的页面。
