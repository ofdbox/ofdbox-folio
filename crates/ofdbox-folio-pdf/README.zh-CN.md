[English](README.md) | 简体中文

# ofdbox-folio-pdf

[`ofdbox-folio`](../../README.zh-CN.md) 的 PDF 读写适配器：把 PDF 页面解析为 Fixed IR，并把 Fixed IR 写回 PDF，两个方向都由共享的 Fixed IR 驱动。

## Public Entry Points

- `PdfDocument` 实现 `DocumentSource`：可用 `open_path()`、`open_path_with_fonts()`、`open_bytes()` 或 `open_bytes_with_fonts()` 打开，再用 `page_info()`、`load_page()`、`load_attachment()`、`font_names()`、`info()` 与 `resources()`。加密文件通过 `PdfReadOptions` 传入用户或所有者密码。
- `write_fixed()` 按 `PdfOptions` 写出 `FixedDocument`；`Writer` 提供增量路径（`set_metadata()`、`add_outline()`、`add_page()`、`add_scene()`、`finish()`）。
- `probe()` 检测 PDF 文件头；`PdfReadOptions` 限制页面流字节数、图片像素与 Form XObject 递归深度。`PdfError` 是适配器错误类型。

### Example

```rust
use ofdbox_folio::pdf::{PdfDocument, PdfOptions, PdfReadOptions, write_fixed};

let mut source = PdfDocument::open_path("input.pdf", PdfReadOptions::default())?;
let page = source.load_page(0)?;
let bytes = write_fixed(&fixed, &PdfOptions::default())?;
```

## Coverage

`ofdbox-folio-pdf` 提供用于读取的 `PdfDocument`（`DocumentSource`）和用于写出的 `write_fixed()`。与 OFD 一样，两个方向都由共享的 Fixed IR 驱动。

### Reading Coverage

路径、文本（Identity-H 与简单字体，`ToUnicode`、`Tr` 渲染模式、字距与水平缩放）、图片（含 CCITT G3/G4）、`sh` 着色与 PatternType 2 渐变图案、内联图片、Form XObject、裁剪、透明度、页面旋转、大纲与附件、加密 PDF（用户或所有者密码）。`PdfDocument` 实现 `DocumentSource`，提供页面尺寸、文本层、字体名与资源表。

### Writing Coverage

路径、裁剪、图片、渐变、透明组、文字（嵌入 TrueType 字体并子集化，失败时退化为字形轮廓）、元数据与书签。JPEG 以 `DCTDecode` 原样嵌入，PNG/JBIG2/BMP 解码后以 `FlateDecode` 嵌入。

## Known Limitations

- PatternType 1 分块图案与 JPEG2000 尚未读取。
- 写出时 TIFF/CCITT 图片资源仍不支持；PDF 读取的 CCITT 会先解码为 PNG。
- trailer 中直接内联的 `/Encrypt` 会先改写成间接对象并强制 xref 重建，大型加密文件首次打开会明显变慢。

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — 本 crate 使用的 Fixed IR、`DocumentSource` 与资源契约。
- [ofdbox-folio-ofd](../ofdbox-folio-ofd) — 另一个定版适配器，同样从 Fixed IR 写出。
- [ofdbox-folio-render](../ofdbox-folio-render) — 渲染本 crate 读取的页面。
