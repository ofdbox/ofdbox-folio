[English](README.md) | 简体中文

# ofdbox-folio-docx

[`ofdbox-folio`](../../README.zh-CN.md) 的 DOCX 读写适配器：用 `read_flow()` 把 DOCX 包解析为 Flow IR，再用 `write_flow()` 把 Flow IR 写回 DOCX 包。

## Public Entry Points

- `read_flow()` 与 `read_flow_path()` 从 DOCX 字节或文件返回 `FlowDocument`；`DocxReadOptions` 限制单个解压后部件的字节数。
- `write_flow()` 返回 DOCX 字节，`write_flow_with_report()` 额外返回降级诊断，`write_flow_path()` 直接写入文件；三者都接收 `DocxWriteOptions`。
- `probe()` 判断字节是否为 DOCX（包含 `word/document.xml` 的 ZIP）；`DocxError` 是适配器错误类型。

本 crate 不接触 Fixed IR，也不做排版或渲染；DOCX 排版近似 Word，但不承诺逐行一致。

## Coverage

`ofdbox_folio::docx::write_flow()` 把 Flow IR 写成 DOCX 包（`[Content_Types].xml`、`document.xml`、`styles.xml`、页眉页脚、`footnotes.xml`/`endnotes.xml`、`media/*`、`docProps`），覆盖分节与页面设置（含分栏）、段落与字符格式、制表位与前导符、表格、行内与浮动图片、页眉页脚、`PAGE`/`NUMPAGES` 域、超链接、脚注尾注引用、自动目录域、书签与大纲级别。固定 ZIP 时间戳，相同输入产生相同字节；`write_flow_with_report()` 额外返回降级诊断。DOCX 读取由统一 Reader 分发（`read_flow` + 排版 + `FixedDocumentSource`）；见 [门面 & Reader](../ofdbox-folio/README.zh-CN.md)。

## Known Limitations

- 读取时样式按 OOXML 级联（文档默认 → 段落样式链 → 字符样式链 → 直接格式）逐字段解析后压平为直接格式，因此从读入的文档无法还原原始具名样式链。
- 无法表达的浮动图片会降级为行内图片，合并单元格按普通单元格处理；两者都会在 `FlowDocument.report` 留下诊断，内容不会被静默丢弃。
- DOCX 排版近似 Word，不承诺逐行一致；表格与页眉页脚的一致性仍在加固。

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [Reader](../ofdbox-folio/README.md) · [Reader 中文](../ofdbox-folio/README.zh-CN.md) — 统一 Reader 如何经 `read_flow` 与排版分发 DOCX。
- [ofdbox-folio-ir](../ofdbox-folio-ir) — 本 crate 使用的 Flow IR、`FlowDocument` 与诊断契约。
- [ofdbox-folio-layout](../ofdbox-folio-layout) — 把本 crate 读出的 Flow IR 排成 Fixed IR 页面。
- [ofdbox-folio](../ofdbox-folio) — 把 `read_flow` + 排版 + `FixedDocumentSource` 串起来的门面。
