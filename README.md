# ofdbox-folio

OFDBox Folio 是一个以“页”为中心的多格式文档引擎：读取 OFD、PDF、DOCX，统一到 Flow IR / Fixed IR，完成排版、渲染与写出。

**当前状态：设计契约已冻结，项目骨架已建立，crate 实现尚未开始。** 本文描述目标架构，不代表已交付能力。

## 目标

- 读取 OFD、PDF，直接得到 Fixed IR 页面。
- 读取 DOCX，得到 Flow IR，经布局计算得到 Fixed IR。
- 用一套创建 API 构造 Flow IR，直接写出 DOCX，或经布局写出 OFD、PDF、位图。
- 以 Fixed IR 为统一读模型，支持惰性加载、文本选择/搜索、分页缓存和增量渲染。
- 同一输入、同一字体字节、同一引擎版本，产生确定的 FixedDocument。

## 非目标

- 不做 Fixed IR 到 Flow IR 的反向转换。
- 不承诺 DOCX 排版与 Word 逐行一致；布局目标是结构、阅读顺序和可读性正确。
- 不在首版追求 PDF 全特性覆盖（Type3、透明组、加密、复杂 CMap 等分期处理）。
- 不提供 FixedDocument 的编辑 API；阅读器不承担文档编辑职责。
- 不把 OFD/PDF/DOCX 的格式类型泄漏到 IR、布局或渲染接口中。

## Crate 架构

```text
ofdbox-folio                门面：格式探测、Reader、创建 API、便捷转换
├─ ofdbox-folio-ofd        OFD 读取/写出        → ofdbox-core
├─ ofdbox-folio-pdf        PDF 读取/写出        → lopdf
├─ ofdbox-folio-docx       DOCX 读取/写出       → zip + quick-xml
├─ ofdbox-folio-layout     Flow → Fixed，单向排版
├─ ofdbox-folio-render     Fixed → 位图，CPU/GPU 渲染与缓存
└─ ofdbox-folio-ir         Flow IR、Fixed IR、几何、资源、文本层、Reader 契约
```

依赖方向固定：格式适配器、布局、渲染都依赖 `ofdbox-folio-ir`；`ofdbox-folio-ir` 不依赖任何格式库或渲染后端。

| crate | 公开职责 | 关键约束 |
|---|---|---|
| `ofdbox-folio-ir` | Flow IR、Fixed IR、几何/路径/画笔、资源、`TextLayer`、`DocumentSource`、诊断 | 无格式依赖，无 I/O |
| `ofdbox-folio-layout` | `layout()`、`LayoutSession`、`FontProvider`、布局选项与错误 | 只做 Flow → Fixed，不读时钟/随机数/系统字体 |
| `ofdbox-folio-render` | `CpuRenderer`、渲染目标、瓦片、字形与图片缓存 | 只消费 Fixed IR，不感知源格式 |
| `ofdbox-folio-ofd` | `OfdDocument`（`DocumentSource`）、`write_fixed()` | 对外不暴露 `ofdbox_core` 类型 |
| `ofdbox-folio-pdf` | `PdfDocument`（`DocumentSource`）、`write_fixed()` | 对外不暴露 `lopdf` 类型 |
| `ofdbox-folio-docx` | `read_docx()` → Flow、`write_docx()` ← Flow | 不接触 Fixed IR |
| `ofdbox-folio` | `open_bytes()`/`open_path()`、`Document`、创建 API、便捷转换 | 统一错误、缓存与格式分发 |

## 工作原理

### 主流程

```text
创建 API ───┐
            ├──► Flow IR ──────► DOCX 写出
DOCX 读取 ──┘        │
                     │ layout（单向）
                     ▼
                Fixed IR ──────┬──► 位图渲染
                     ▲         ├──► OFD 写出
                     │         └──► PDF 写出
                     │
OFD / PDF 读取 ──────┘
```

- Flow IR 在上，Fixed IR 在下；布局是两者之间唯一的向下通路。
- DOCX 写出直接消费 Flow IR，不经过布局和 Fixed IR。
- OFD/PDF 读取直接产出 Fixed IR，不经过 Flow IR 和布局。
- Fixed IR 是位图渲染、OFD 写出、PDF 写出的共同入口。
- 阅读时打开文档只解析元数据、页面表、大纲和资源索引；`DocumentSource::load_page()` 按需解析单页，Reader 用字节预算的 LRU 缓存。单页损坏只影响该页，阅读器显示占位并保留诊断。

### 两种 IR

**Flow IR**：语义化、可编辑的流式文档模型。段落、行内 run、标题、表格、图片、字段、目录、分节、页眉页脚。长度用整数 twips，不包含绝对坐标。

**Fixed IR**：不可变、可缓存的固定版式页面。原点在左上，x 向右，y 向下，长度用毫米。每页包含：

- `Scene`：语义化页面内容树，供渲染与写出；
- `TextLayer`：逻辑阅读顺序的文本与字符/字形簇映射，供选择、复制、搜索；
- `PageInfo`：media/crop box、旋转、页码标签；
- 链接区域与模板层引用。

`FixedDocument` 是布局的完整输出；`Page` 是阅读器的加载单位。两者共享同一套内容树，但构建方式不同：布局一次性产出，阅读器按需解析。

### 阅读器性能模型

- **页面惰性**：首屏时间与总页数无关；内存由缓存预算和可视页决定。
- **资源分离**：字体、图片按内容共享；图片按目标缩放档位解码，缩略图不触发全分辨率解码。
- **渲染列表缓存**：页面首次绘制时生成扁平渲染指令列表，滚动和缩放复用。
- **文本层独立**：绘制顺序不等于阅读顺序；文本层保存行框、基线和字符映射，选择/搜索不重新解析页面。
- **渐进布局**：DOCX 解析成 Flow IR 后按顺序产出页面，先显示首页，总页数字段后到。
- **错误隔离**：页级错误不终止文档加载。

### 文本与字体

- 布局阶段完成 shaping，Fixed IR 保存字形 ID、位置、advance 和字符/字形簇映射。
- 阅读 OFD 时保留 `CGTransform`；阅读 PDF 时使用 `ToUnicode` CMap；DOCX 使用逻辑顺序。
- 无法解析为字体的文本（Type3、缺失字体）降级为路径，并在文本层记录降级区段。
- 字体由调用方显式提供，不扫描系统字体；缺字体按策略报错或回退并写入报告。

### 确定性

- Flow IR 使用 twips，Fixed IR 使用毫米；边界一次性换算，OFD 输出量化到 0.001 mm。
- 布局不读时钟、随机数、环境变量，不使用 HashMap 遍历序。
- FixedDocument 记录布局引擎版本和字体内容指纹。
- 相同 Flow + 相同字体字节 + 相同选项 + 相同版本，布局结果一致。
- 阅读器的加载时序可以变化，但页面内容不因此变化。

## 关键契约

1. **单向**：只允许 Flow → Fixed；Fixed IR 不保留 Flow 反向引用。
2. **格式中立**：IR、布局、渲染接口中不出现 OFD/PDF/DOCX/ZIP/XML 类型。
3. **页面不可变**：`Page` 可缓存、可跨线程共享；文档不一次性构建全部页面。
4. **单位固定**：Flow 用 twips，Fixed 用毫米；坐标系为左上原点、y 向下。
5. **沉默**：不写日志、不打印；不可表示项进入报告，失败返回错误。
6. **无 panic**：所有格式解析器对不可信输入返回错误，并受页数、解压、图片像素与递归深度上限约束。
7. **不泄漏依赖**：`ofdbox_core`、`lopdf`、`quick_xml`、`zip`、`rustybuzz`、`ttf_parser`、`vello`、`wgpu` 类型不得出现在公开签名中。

## API 示意

以下为结构示意，最终以 crate 的公开 API 为准。

```rust
// 读取：格式探测 + 按需取页
let mut doc = ofdbox_folio::open_path("input.ofd", &OpenOptions::default())?;
let info = doc.info();
let page = doc.page(0)?;

// 创建：构造 Flow IR
let mut builder = DocumentBuilder::new(Metadata::default());
builder.heading(1, "第一章");
builder.paragraph(
    ParagraphBuilder::new(ParaStyle::default())
        .span("正文", TextStyle::default())
        .build(),
);
let flow = builder.build()?;

// 直接写出 DOCX
let docx = ofdbox_folio::to_docx(&flow, &DocxWriteOptions::default())?;

// 布局后写出 OFD
let mut fonts = MyFontProvider::new();
let fixed = ofdbox_folio::layout(&flow, &mut fonts, &LayoutOptions::default())?;
let ofd = ofdbox_folio::to_ofd(&fixed, &OfdWriteOptions::default())?;
```

## 路线

| 阶段 | 内容 | 验收 |
|---|---|---|
| R1 阅读器内核 | IR、OFD 读取、CPU 渲染、文本层、页面/资源缓存 | 惰性加载、选择/搜索可用、页级错误隔离、wasm 构建 |
| R2 流式文档 | Flow IR、布局、DOCX 读写、创建 API | Word → OFD/PDF/位图链路打通，布局确定 |
| R3 PDF 阅读硬化 | 字体与 CMap、图案/渐变、透明、加密、坏文件容错 | 真实 PDF 语料可读，降级有报告 |
| R4 文档能力 | 注释/表单、附件、大纲编辑、GPU 后端 | 阅读器功能闭环 |

当前处于 R1 之前的契约与骨架阶段。

## 测试与验收

- 生成的 OFD 必须能被 `ofdbox-core` 严格读取并严格重写后字节一致。
- 相同输入的布局结果生成稳定摘要；跨平台 CI 比较。
- 阅读器内存随总页数保持有界；翻页复用字体、图片和渲染缓存。
- 文本选择、复制、搜索不重新解析页面。
- 单页损坏不影响其他页。
- 以现有 OFD 语料为基准，并补充 PDF/DOCX 语料。

## 与 OFDBox 的关系

- `ofdbox-folio` 是独立仓库，位于 `ofdbox` 组织下，与工具箱仓库 `ofdbox/ofdbox` 分开发布。
- OFD 结构解析、密码学与签章暂时依赖 `ofdbox-core`、`ofdbox-crypto`、`ofdbox-seal`。
- 新引擎承接并最终替代 `ofdbox-render` 的读取、渲染与写出能力；旧 crate 迁移完成后打弃用标记。
- 新仓库内部 crate 统一使用 `ofdbox-folio-` 前缀，避免过渡期与旧 crate 抢名。

## 许可证

本项目采用 MIT 或 Apache-2.0 双协议，任选其一：

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)

