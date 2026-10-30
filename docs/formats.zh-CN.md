[English](formats.md) | 简体中文

# 格式适配

格式中立的 IR 背后的 OFD、PDF 与 DOCX 适配器；如需了解引擎全貌，请从 [`../README.zh-CN.md`](../README.zh-CN.md) 开始。

| 适配器 | crate | 方向 | 入口 |
|---|---|---|---|
| OFD | `ofdbox-folio-ofd` | 读 + 写 | `OfdDocument`、`write_fixed()` |
| PDF | `ofdbox-folio-pdf` | 读 + 写 | `PdfDocument`、`write_fixed()` |
| DOCX | `ofdbox-folio-docx` | 读 + 写 | `read_flow()`、`write_flow()` |

## OFD 写出

写出器消费 Fixed IR，输出 OFD 1.0 包（`Version="1.0"`）。它与读取器一起位于 `ofdbox-folio-ofd`，两个方向共用同一套包模型。

### 示例

```rust
use ofdbox_folio::ir::FixedDocument;
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

let bytes = write_fixed(&document, &OfdWriteOptions::default())?;
```

### 包覆盖范围

路径、文本（含 `CGTransform` 与 `DeltaX/DeltaY`）、图片、组裁剪、透明度、轴向/径向渐变、字体名称与内嵌字体字节。

对象 `Boundary` 取整页，实际位置由 `CTM` 承载；行的位置差直接来自字形局部坐标，避免字体度量引入偏差。

### 字体

内嵌字体按文档实际用字做 TrueType 子集化（保留字形 ID，只清空未用轮廓，复合字形依赖一并保留），字体不可裁剪时整份嵌入；默认使用固定 ZIP 时间戳和内容摘要 `DocID`，相同输入产生相同字节。

### 已知限制

- 图案与网格填充尚未写出，会回退为首个色标。
- 组透明度分配到子元素颜色 alpha，不保留独立图层语义。

## PDF 读写

`ofdbox-folio-pdf` 提供用于读取的 `PdfDocument`（`DocumentSource`）和用于写出的 `write_fixed()`。与 OFD 一样，两个方向都由共享的 Fixed IR 驱动。

### 示例

```rust
use ofdbox_folio::pdf::{PdfDocument, PdfOptions, PdfReadOptions, write_fixed};

let mut source = PdfDocument::open_path("input.pdf", PdfReadOptions::default())?;
let page = source.load_page(0)?;
let bytes = write_fixed(&fixed, &PdfOptions::default())?;
```

### 读取覆盖

路径、文本（Identity-H 与简单字体，`ToUnicode`、`Tr` 渲染模式、字距与水平缩放）、图片（含 CCITT G3/G4）、`sh` 着色与 PatternType 2 渐变图案、内联图片、Form XObject、裁剪、透明度、页面旋转、大纲与附件、加密 PDF（用户或所有者密码）。`PdfDocument` 实现 `DocumentSource`，提供页面尺寸、文本层、字体名与资源表。

### 写出覆盖

路径、裁剪、图片、渐变、透明组、文字（嵌入 TrueType 字体并子集化，失败时退化为字形轮廓）、元数据与书签。JPEG 以 `DCTDecode` 原样嵌入，PNG/JBIG2/BMP 解码后以 `FlateDecode` 嵌入。

### 已知限制

- PatternType 1 分块图案与 JPEG2000 尚未读取。
- 写出时 TIFF/CCITT 图片资源仍不支持；PDF 读取的 CCITT 会先解码为 PNG。
- trailer 中直接内联的 `/Encrypt` 会先改写成间接对象并强制 xref 重建，大型加密文件首次打开会明显变慢。

## GPU 后端

`ofdbox-folio-render` 的 `wgpu` feature（门面 `gpu`）提供 compute 后端，`gpu` 默认关闭。

### 示例

```rust
use ofdbox_folio::render::{WgpuBackend, WgpuContext, WgpuTarget};

let backend = WgpuBackend::new(WgpuContext::new_blocking()?)?;
let target = WgpuTarget::new(backend.context(), page.display_size(), 144.0)?;
backend.render_to_target(&page.content, &target)?;
let rgba = target.read_rgba(backend.context())?;
```

### 覆盖范围

实色填充、描边、线性渐变、裁剪与文字轮廓。

### 已知限制

- 图片、径向渐变与混合模式尚未实现。
- 需要完整特性时使用 CPU 渲染器。

## DOCX 写出

`ofdbox_folio::docx::write_flow()` 把 Flow IR 写成 DOCX 包（`[Content_Types].xml`、`document.xml`、`styles.xml`、页眉页脚、`footnotes.xml`/`endnotes.xml`、`media/*`、`docProps`），覆盖分节与页面设置（含分栏）、段落与字符格式、制表位与前导符、表格、行内与浮动图片、页眉页脚、`PAGE`/`NUMPAGES` 域、超链接、脚注尾注引用、自动目录域、书签与大纲级别。固定 ZIP 时间戳，相同输入产生相同字节；`write_flow_with_report()` 额外返回降级诊断。DOCX 读取由统一 Reader 分发（`read_flow` + 排版 + `FixedDocumentSource`）；见 [reader.zh-CN.md](reader.zh-CN.md)。
