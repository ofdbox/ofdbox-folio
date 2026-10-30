[English](README.md) | 简体中文

# ofdbox-folio

OFDBox Folio 是以“页”为中心的多格式文档引擎：读取 OFD、PDF、DOCX，统一为 Flow IR / Fixed IR，并负责排版、渲染与写出。

## 特性

- 用同一个 Reader 打开 OFD/PDF/DOCX：OFD/PDF 按需加载页面，DOCX 读入 Flow IR 后立即排版。
- 用一套创建 API 构建 Flow IR：可直接写 DOCX，也可排版后写 OFD/PDF/位图。
- 以 Fixed IR 作为统一读取模型：按需加载、文本选择/搜索、页面缓存、增量渲染。
- 排版产出的 `FixedDocument` 可以包回 Reader（`FixedDocumentSource`），复用缓存、选择与搜索。
- 相同输入、相同字体字节、相同引擎版本，输出确定。
- CPU 渲染器 + 可选 wgpu 后端；OFD/PDF 写出支持字体子集化。

整个引擎围绕一条主流程组织：

```text
创建 API ───┐
            ├──► Flow IR ──────► DOCX 写出
DOCX 读取 ──┘        │
                     │ 排版（单向）
                     ▼
                Fixed IR ──────┬──► 位图渲染
                     ▲         ├──► OFD 写出
                     │         └──► PDF 写出
                     │
OFD / PDF 读取 ──────┘
```

Flow IR 是语义化、可编辑的流式模型（twip，无绝对坐标）；Fixed IR 是不可变、可缓存的页面模型（毫米，原点左上、y 向下）。排版是唯一向下路径；OFD/PDF 读取绕过排版，DOCX 写出直接消费 Flow IR。

## 架构

```text
ofdbox-folio                门面：格式探测、Reader、创建 API、便捷转换
├─ ofdbox-folio-ofd        OFD 读写              → ofdbox-core
├─ ofdbox-folio-pdf        PDF 读写              → lopdf
├─ ofdbox-folio-docx       DOCX 读写             → zip + quick-xml
├─ ofdbox-folio-layout     Flow → Fixed，单向排版
├─ ofdbox-folio-render     Fixed → 位图，CPU/GPU 渲染与缓存
└─ ofdbox-folio-ir         Flow IR、Fixed IR、几何、资源、文本层、Reader 契约
```

格式适配、排版与渲染都依赖 `ofdbox-folio-ir`；`ofdbox-folio-ir` 不依赖任何格式库或渲染后端。

细节：[架构](docs/architecture.zh-CN.md) · [门面 & Reader](crates/ofdbox-folio/README.zh-CN.md) · [IR](crates/ofdbox-folio-ir/README.zh-CN.md) · [排版](crates/ofdbox-folio-layout/README.zh-CN.md) · [OFD](crates/ofdbox-folio-ofd/README.zh-CN.md) · [PDF](crates/ofdbox-folio-pdf/README.zh-CN.md) · [DOCX](crates/ofdbox-folio-docx/README.zh-CN.md) · [渲染](crates/ofdbox-folio-render/README.zh-CN.md)。

## 快速开始

```bash
cargo test --workspace
```

读取页面（OFD、PDF、DOCX 使用同一套 API）：

```rust
use ofdbox_folio::{Document, OpenOptions};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;
let page = document.page(0)?;

// DOCX 会在打开时读入 Flow IR 并完成排版。
let mut document = Document::open_path("input.docx", OpenOptions::default())?;
```

构建 Flow IR 并写 DOCX，或排版后写 OFD：

```rust
use ofdbox_folio::{
    docx::{DocxWriteOptions, write_flow},
    ir::{Block, FlowDocument, Inline, ParaStyle, Paragraph, TextRun, TextStyle},
    layout::layout,
    ofd::{OfdWriteOptions, write_fixed},
};

let mut flow = FlowDocument::new(Default::default());
let mut paragraph = Paragraph::new(ParaStyle::default());
paragraph.runs.push(Inline::Text(TextRun::new("你好，世界", TextStyle::default())));
flow.push(Block::Paragraph(paragraph));

let docx = write_flow(&flow, &DocxWriteOptions::default())?;

let mut fonts = MyFontProvider::new();
let fixed = layout(&flow, &mut fonts)?;
let ofd = write_fixed(&fixed, &OfdWriteOptions::default())?;
```

命令行渲染或转换：

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example docx_to_ofd -- input.docx output.ofd
```

## 保证与限制

- 单向：只允许 Flow → Fixed；Fixed IR 不保留对 Flow 的反向引用。
- 格式中立：OFD/PDF/DOCX 类型不出现在 IR、排版或渲染接口中。
- 页面不可变：`Page` 可缓存、可跨线程共享；OFD/PDF 打开时不会构建全部页面（DOCX 会立即排版）。
- 单位固定：Flow 用 twip，Fixed 用毫米，原点左上、y 向下。
- 确定性：不读时钟、随机数或环境变量；相同输入 + 字体 + 选项 + 版本 → 相同输出。
- 失败以错误或诊断返回；解析器面对不可信输入不 panic。

已知限制：

- OFD 未内嵌字体时需要调用方提供 `FontProvider`；否则文本仍保留在文本层，但不绘制。
- OFD 模板页目前被展平进页面内容。
- 页面缓存只计算页面结构；字体和图片是文档级共享资源。
- GPU 后端覆盖实色填充、描边、线性渐变、裁剪和文字轮廓；图片、径向渐变和混合模式仅 CPU 支持。
- OFD 的 Pattern/Mesh 填充尚未写出；PDF 的平铺 Pattern 和 JPEG2000 尚未读取。
- DOCX 在打开时立即排版，页数与排版耗时都在打开阶段支付；页面缓存缓存的是排版后的页面，而不是延迟解析。
- DOCX 排版只是近似 Word（不保证逐行一致）；表格、页眉页脚的保真度仍在打磨。

## 状态与路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| R1 Reader 内核 | IR、OFD 读取、CPU 渲染、文本层、页面缓存 | 已完成 |
| R2 Flow 文档 | Flow IR、排版（分栏/表格/页眉页脚/目录）、DOCX 读写、创建 API | 进行中 |
| R3 PDF 加固 | 字体/CMap、Pattern、透明度、加密、坏文件容忍 | 进行中 |
| R4 文档能力 | 注释/表单、附件、大纲编辑、GPU | 计划中 |

## 验证

```bash
cargo test --workspace
```

验收要点：生成的 OFD 可被严格读取，严格重写后字节一致；排版摘要在跨平台稳定；Reader 内存随总页数增长保持有界；选择/搜索不重复解析页面；单页损坏不影响其它页面。

## 文档与许可

- [架构](docs/architecture.zh-CN.md) · [门面 & Reader](crates/ofdbox-folio/README.zh-CN.md) · [IR](crates/ofdbox-folio-ir/README.zh-CN.md) · [排版](crates/ofdbox-folio-layout/README.zh-CN.md) · [OFD](crates/ofdbox-folio-ofd/README.zh-CN.md) · [PDF](crates/ofdbox-folio-pdf/README.zh-CN.md) · [DOCX](crates/ofdbox-folio-docx/README.zh-CN.md) · [渲染](crates/ofdbox-folio-render/README.zh-CN.md)
- English docs: [Architecture](docs/architecture.md) · [Facade & Reader](crates/ofdbox-folio/README.md) · [IR](crates/ofdbox-folio-ir/README.md) · [Layout](crates/ofdbox-folio-layout/README.md) · [OFD](crates/ofdbox-folio-ofd/README.md) · [PDF](crates/ofdbox-folio-pdf/README.md) · [DOCX](crates/ofdbox-folio-docx/README.md) · [Render](crates/ofdbox-folio-render/README.md)
- [English README](README.md)

本项目采用 MIT 或 Apache-2.0 双许可，可任选其一：[LICENSE-MIT](LICENSE-MIT) · [LICENSE-APACHE](LICENSE-APACHE)。
