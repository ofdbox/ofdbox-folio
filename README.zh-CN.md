[English](README.md) | 简体中文

# ofdbox-folio

OFDBox Folio 是以“页”为中心的多格式文档引擎：读取 OFD、PDF、DOCX，统一为 Flow IR / Fixed IR，并负责排版、渲染与写出。

## 特性

- 直接读取 OFD/PDF 得到 Fixed IR；读取 DOCX 得到 Flow IR，再排版为 Fixed IR。
- 用一套创建 API 构建 Flow IR：可直接写 DOCX，也可排版后写 OFD/PDF/位图。
- 以 Fixed IR 作为统一读取模型：按需加载、文本选择/搜索、页面缓存、增量渲染。
- 相同输入、相同字体字节、相同引擎版本，输出确定。
- CPU 渲染器 + 可选 wgpu 后端；OFD/PDF 写出支持字体子集化。

整个引擎围绕一条主流程组织：

```text
创建 API ──────┐
               │
DOCX 读取 ─────┤
               ├─► Flow IR ──► DOCX 写出
                      │
                      │ 排版（单向）
                      ▼
                  Fixed IR ──┬─► 位图渲染
                      ▲      ├─► OFD 写出
                      │      └─► PDF 写出
OFD / PDF 读取 ───────┘
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

细节：[架构](docs/architecture.zh-CN.md) · [Reader](docs/reader.zh-CN.md) · [格式适配](docs/formats.zh-CN.md) · [布局契约](docs/layout-contract.zh-CN.md)。

## 快速开始

```bash
cargo test --workspace
```

读取页面：

```rust
let mut document = ofdbox_folio::open_path("input.ofd", &OpenOptions::default())?;
let page = document.page(0)?;
```

构建 Flow IR 并写 DOCX，或排版后写 OFD：

```rust
let mut builder = DocumentBuilder::new(Metadata::default());
builder.heading(1, "第一章");
builder.paragraph(
    ParagraphBuilder::new(ParaStyle::default())
        .span("你好，世界", TextStyle::default())
        .build(),
);
let flow = builder.build()?;

let docx = ofdbox_folio::to_docx(&flow, &DocxWriteOptions::default())?;

let mut fonts = MyFontProvider::new();
let fixed = ofdbox_folio::layout(&flow, &mut fonts, &LayoutOptions::default())?;
let ofd = ofdbox_folio::to_ofd(&fixed, &OfdWriteOptions::default())?;
```

命令行渲染或转换：

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example docx_to_ofd -- input.docx output.ofd
```

## 保证与限制

- 单向：只允许 Flow → Fixed；Fixed IR 不保留对 Flow 的反向引用。
- 格式中立：OFD/PDF/DOCX 类型不出现在 IR、排版或渲染接口中。
- 页面不可变：`Page` 可缓存、可跨线程共享；打开文档时不会构建全部页面。
- 单位固定：Flow 用 twip，Fixed 用毫米，原点左上、y 向下。
- 确定性：不读时钟、随机数或环境变量；相同输入 + 字体 + 选项 + 版本 → 相同输出。
- 失败以错误或诊断返回；解析器面对不可信输入不 panic。

已知限制：

- OFD 未内嵌字体时需要调用方提供 `FontProvider`；否则文本仍保留在文本层，但不绘制。
- OFD 模板页目前被展平进页面内容。
- 页面缓存只计算页面结构；字体和图片是文档级共享资源。
- GPU 后端覆盖实色填充、描边、线性渐变、裁剪和文字轮廓；图片、径向渐变和混合模式仅 CPU 支持。
- OFD 的 Pattern/Mesh 填充尚未写出；PDF 的平铺 Pattern 和 JPEG2000 尚未读取。

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

- [架构](docs/architecture.zh-CN.md) · [Reader](docs/reader.zh-CN.md) · [格式适配](docs/formats.zh-CN.md) · [布局契约](docs/layout-contract.zh-CN.md)
- English docs: [Architecture](docs/architecture.md) · [Reader](docs/reader.md) · [Format adapters](docs/formats.md) · [Layout contract](docs/layout-contract.md)
- [English README](README.md)

本项目采用 MIT 或 Apache-2.0 双许可，可任选其一：[LICENSE-MIT](LICENSE-MIT) · [LICENSE-APACHE](LICENSE-APACHE)。
