[English](architecture.md) | 简体中文

# 架构

本文说明 OFDBox Folio 的内部结构；目标、当前状态与用法见 [README](../README.zh-CN.md)。

## Crate 架构

```text
ofdbox-folio                门面：格式探测、Reader、创建 API、便捷转换
├─ ofdbox-folio-ofd        OFD 读取/写出         → ofdbox-core
├─ ofdbox-folio-pdf        PDF 读取/写出         → lopdf
├─ ofdbox-folio-docx       DOCX 读取/写出        → zip + quick-xml
├─ ofdbox-folio-layout     Flow → Fixed，单向排版
├─ ofdbox-folio-render     Fixed → 位图，CPU/GPU 渲染与缓存
└─ ofdbox-folio-ir         Flow IR、Fixed IR、几何、资源、文本层、Reader 契约
```

依赖方向固定：格式适配器、布局与渲染都依赖 `ofdbox-folio-ir`，而 `ofdbox-folio-ir` 不依赖任何格式库或渲染后端。

| crate | 公开职责 | 关键约束 |
|---|---|---|
| `ofdbox-folio-ir` | Flow IR、Fixed IR、几何/路径/画笔、资源、`TextLayer`、`DocumentSource`、`FixedDocumentSource`、诊断 | 无格式依赖，无 I/O |
| `ofdbox-folio-layout` | `layout()`、`LayoutSession`、`FontProvider`、布局选项与错误 | 只做 Flow → Fixed，不读时钟/随机数/系统字体 |
| `ofdbox-folio-render` | `CpuRenderer`、渲染目标、瓦片、字形与图片缓存 | 只消费 Fixed IR，不感知源格式 |
| `ofdbox-folio-ofd` | `OfdDocument`（`DocumentSource`）、`write_fixed()` | 对外不暴露 `ofdbox_core` 类型 |
| `ofdbox-folio-pdf` | `PdfDocument`（`DocumentSource`）、`write_fixed()` | 对外不暴露 `lopdf` 类型 |
| `ofdbox-folio-docx` | `read_flow()`/`read_flow_path()` → Flow、`write_flow()` → DOCX、`probe()` | 不接触 Fixed IR |
| `ofdbox-folio` | `Document::open_bytes()`/`open_path()`、`from_fixed()`/`from_flow()`、创建 API、便捷转换 | 统一错误、缓存与格式分发（DOCX → Flow → 排版） |

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
- Fixed IR 是位图渲染、OFD 写出与 PDF 写出的共同入口。
- 打开文档时只解析元数据、页面表、大纲和资源索引；`DocumentSource::load_page()` 按需解析单页，Reader 使用带字节预算的 LRU 缓存。单页损坏只影响该页：阅读器显示占位内容并保留诊断。页面缓存、选择与搜索的实现见 [reader.zh-CN.md](reader.zh-CN.md)。
- 同一个 Reader 也会分发 DOCX：把包读成 Flow IR，用调用方提供的 `FontProvider` 立即排版，再把 `FixedDocument` 包成 `FixedDocumentSource`，因此 DOCX 页面复用同一套缓存、选择与搜索 API。`Document::from_fixed`/`from_flow` 也以同样方式包装排版结果。

### 两种 IR

**Flow IR** 是语义化、可编辑的流式文档模型，包含段落、行内 run、标题、表格、图片、字段、目录、分节、页眉页脚；长度以整数 twips 表示，不涉及绝对坐标。

**Fixed IR** 是不可变、可缓存的固定版式页面。原点在左上，x 向右，y 向下，长度以毫米为单位。每页包含：

- `Scene`：语义化页面内容树，供渲染与写出消费；
- `TextLayer`：按逻辑阅读顺序排列的文本，以及字符/字形簇映射，供选择、复制和搜索；
- `PageInfo`：media/crop box、旋转与页码标签；
- 链接区域与模板层引用。

`FixedDocument` 是布局的完整输出；`Page` 是阅读器的加载单位。两者共享同一套内容树，但构建方式不同：布局一次性产出，阅读器按需解析。

### 排版（A1）

`ofdbox_folio::layout()` 把 Flow IR 排成 Fixed IR，规则冻结在 [layout-contract.zh-CN.md](layout-contract.zh-CN.md)。

- **断行**：允许在 CJK 边界与空格处断行，行首/行尾禁则把标点回吞；行尾空格不占行宽。
- **段落**：支持首行/左右缩进、单倍/多倍/固定/最小行距、段前段后、左/中/右与两端对齐；两端对齐优先拉伸空格，没有空格时把余量均摊到字间。
- **行高与字体回退**：单倍行高取字体 `hhea` 的 ascent/descent/lineGap，基线按 ascent 定位；主字体缺字形时逐字回退到兜底字体。
- **分页与分栏**：按行分页，段前间距在页首丢弃；`PageSetup::columns` 支持等宽多栏流式排版（最后一栏排满才换页）；引用总页数时排版跑两遍，第一遍统计、第二遍代入。
- **图片**：块级图片按声明尺寸或 DPI 换算，超宽时等比缩放并记录诊断；行内图片是不可断单元，底部贴基线。
- **字体**：`FontProvider` 按「字族 + 粗斜体」解析字体字节，度量取自 `hmtx`；缺字记录诊断并按 `.notdef` 宽度排版。
- **大纲**：排版时登记 `heading_level`，按层级生成 `OutlineItem` 树，目标页与 y 坐标取自标题首行。
- **表格**：支持固定/权重列宽、单元格子框排版（列宽减内边距）、按行跨页并重复表头、逐格边框；单行高于本页剩余空间时拆分续排。
- **页眉页脚**：页眉从上边距上方往下排，页脚先量高再贴底；支持首页不同/偶数页，`PAGE`/`NUMPAGES` 与正文共用同一套页码。
- **浮动对象**：`Block::Floating` 按页面坐标绝对定位，支持不绕排、上下型与四周型（整段让开）三种绕排。
- **脚注尾注**：`Inline::Note` 按出现顺序编号；脚注在引用页底部预留空间并画分隔线，尾注收集到文档末尾单独成页。
- **制表位**：声明的制表位超出正文宽度时裁到右边缘，放不进剩余空间的内容紧接排布（均记诊断）；`ParaStyle::tabs` 支持左/中/右/小数点对齐与点线、虚线、实线前导符，制表符按当前 x 跳到下一个制表位，无显式制表位时按 12.5 mm 格点前进。
- **自动目录**：`Block::Toc` 展开为「条目 + 制表符 + 页码」的段落，页码落在版心右侧的右制表位上并带点线前导符；先空跑收集页码，再迭代代入直到稳定。
- **样式**：命名样式沿 `based_on` 链继承，直接格式覆盖非默认字段。

### 系统字体

`ofdbox_folio::fonts::SystemFonts` 是唯一的「字体族名 → 本机字体文件」解析实现（候选表 + 缓存 + 结果稳定），由示例与 CLI 共用。`used_fonts()` 返回实际命中的文件，CLI 的 `--report` 把它们写成 `FONT <族名> -> <文件>`。

### DOCX 读写

`ofdbox_folio::docx::write_flow()` 把 Flow IR 写进 DOCX 包（`[Content_Types].xml`、`document.xml`、`styles.xml`、页眉页脚、`footnotes.xml`/`endnotes.xml`、`media/*`、`docProps`），覆盖分节与页面设置（含分栏）、段落与字符格式、制表位与前导符、表格（边框/表头行/单元格内边距）、行内与浮动图片（`wp:inline`/`wp:anchor`）、页眉页脚、`PAGE`/`NUMPAGES` 域、超链接、脚注尾注引用、自动目录域、书签与大纲级别。附件以「清单 + 原文件部件」形式写入包中（Word 不展示，本库可往返），ZIP 时间戳固定，因此相同输入产生相同字节；`write_flow_with_report` 还会额外返回降级诊断。

`examples/create_document.rs` 从零构建文档，并用同一份 Flow IR 同时写出 DOCX 与 OFD（标题、字符格式、点线制表位、表格、行内与浮动图片、脚注、目录域、带页码字段的页眉页脚；OFD 路径经过排版与字体子集化）：

```bash
cargo run -p ofdbox-folio --example create_document -- out.docx  # 同时产出 out.ofd
```

### DOCX 读取

`ofdbox_folio::docx::read_flow()` 把 DOCX 读成 Flow IR：

- **样式**：字段按 OOXML 层叠（`docDefaults` → 默认样式 → 段落/字符样式链 → 直接格式）解析后拍平，显式写回的默认值也能生效；`based_on` 与主题字体（`majorFont`/`minorFont`、`Hans` 脚本）都会解析。
- **块**：段落、表格（列宽、表头行、边框、单元格内边距）、行内图片（显示尺寸取自 `wp:extent`）、段前与段中分页符、大纲级别 → 标题、目录内容控件 → `Block::Toc`。
- **列表**：`numbering.xml` 中的编号/项目符号成为段落首个 run 的标记，并配合悬挂缩进；Symbol 字体的私用区码位映射为普通符号。
- **页眉页脚**：`headerReference`/`footerReference` 读成 `HeaderFooterSet`，`fldSimple` 与复杂域中的 `PAGE`/`NUMPAGES` 转成 `Field`。
- **元数据**：`docProps/core.xml` 与 `app.xml`。
- **降级**：浮动图片按行内处理，合并单元格按普通单元格处理，VML、脚注、符号字体等记入 `FlowDocument.report` 诊断。
- **Reader 集成**：`Document::open_bytes` 识别 DOCX 后调用 `read_flow()`，排版并包成 `FixedDocumentSource`；DOCX 页面因此与原生格式共用缓存、选择、搜索与命中测试 API。

### 文本与字体

- shaping 在布局阶段完成；Fixed IR 保存字形 ID、位置、advance 与字符/字形簇映射。
- 读取 OFD 时保留 `CGTransform`，读取 PDF 时使用 `ToUnicode` CMap，DOCX 使用逻辑顺序。
- 无法解析到字体的文本（Type3、缺失字体）降级为路径，降级区段记录在文本层中。
- 字体由调用方显式提供，不扫描系统字体；缺字体时按策略报错或回退，并写入报告。

### 确定性

- Flow IR 使用 twips，Fixed IR 使用毫米；边界只换算一次，OFD 输出量化到 0.001 mm。
- 布局不读时钟、随机数或环境变量，也不依赖 HashMap 遍历顺序。
- `FixedDocument` 记录布局引擎版本与字体内容指纹。
- 相同 Flow + 相同字体字节 + 相同选项 + 相同版本 → 布局结果一致。
- 阅读器的加载时序可以变化，但页面内容不会因此改变。

## 关键契约

1. **单向**：只允许 Flow → Fixed；Fixed IR 不保留指向 Flow 的反向引用。
2. **格式中立**：IR、布局与渲染接口中不出现 OFD/PDF/DOCX/ZIP/XML 类型。
3. **页面不可变**：`Page` 可缓存并跨线程共享，文档不会一次性构建所有页面。
4. **单位固定**：Flow 用 twips，Fixed 用毫米；坐标系原点在左上、y 向下。
5. **沉默**：不写日志、不打印；无法表示的项进入报告，失败返回错误。
6. **无 panic**：所有格式解析器对不可信输入返回错误，并受页数、解压、图片像素与递归深度上限约束。
7. **不泄漏依赖**：`ofdbox_core`、`lopdf`、`quick_xml`、`zip`、`rustybuzz`、`ttf_parser`、`vello`、`wgpu` 类型不得出现在公开签名中。
